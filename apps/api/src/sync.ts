import { Hono, type MiddlewareHandler } from 'hono';
import type { AppEnv, Caller, Env } from './env';
import { bearer, requireUser, sha256Hex, verifyUserToken } from './auth';
import { ensureServerTable, PART_BYTES, readLimited } from './service';
import { Sha256 } from './sha256';
import { BadRequest, NotFound, Surreal, UserError, fromBase64, recordId, toBase64 } from './surreal';
import { SYNC_KEY_PREFIX, approveDevice, cleanText, connectOrigin, deviceName, deviceRequest, pollDevice, startDeviceFlow } from './sync-device';

/**
 * Paper sync for the desktop app, under /api/sync (docs/sync-protocol.md is
 * the contract). A paper is a `paper_sync` row, each snapshot of it an
 * immutable `paper_revision` listing every file by SHA-256, and the bytes are
 * blobs stored once per owner (`sync_blob`, files in the `materials` bucket).
 *
 * Callers are the desktop with its sync key (sync-device.ts) or the web app
 * with the user's session. Everything runs as the API's database user, so
 * every query here filters by the caller's id itself.
 */

export const MAX_BLOB_BYTES = 50 * 1024 * 1024;
export const MAX_HASHES = 1000;
export const MAX_FILES = 5000;
export const MAX_REVISIONS = 200;
const MAX_COMMIT_BODY = 4 * 1024 * 1024;
const MAX_BODY = 64 * 1024;
const MAX_PATH = 512;
const MAX_NAME = 200;
const MAX_MESSAGE = 2000;
const SHA = /^[0-9a-f]{64}$/;
const ENGINES = ['typst', 'latex'] as const;
type Engine = (typeof ENGINES)[number];

const newKey = (table: string) => `${table}:k${crypto.randomUUID().replaceAll('-', '')}`;
const tooLarge = (what: string) => new UserError(`${what} is too large.`, 413);

// ── Auth ──────────────────────────────────────────────────────────────────

/** Who sent this token: a device's sync key, or the web app's session. */
export async function syncCaller(env: Env, token: string): Promise<Caller | null> {
  if (!token.startsWith(SYNC_KEY_PREFIX)) {
    const userId = await verifyUserToken(env, token);
    return userId ? { userId, token } : null;
  }
  if (!/^oyl_sync_[A-Za-z0-9_-]{43}$/.test(token)) return null;
  await ensureServerTable(env, 'sync_device');
  // Checked on every request, so revoking the connection in Settings stops the key at once.
  const row = await Surreal.system(env).query<{ owner: string; name: string } | null>(
    `RETURN (SELECT owner, name FROM sync_device WHERE token_hash = $hash
       AND connection.id != NONE AND connection.revoked_at = NONE AND connection.owner = owner LIMIT 1)[0]`,
    { hash: await sha256Hex(token) },
  );
  return row ? { userId: row.owner, token, device: row.name } : null;
}

/** A device's sync key or the signed-in user's session; sets `c.var.caller`. */
export const requireSyncCaller: MiddlewareHandler<AppEnv> = async (c, next) => {
  const token = bearer(c.req.header('authorization'));
  const caller = token ? await syncCaller(c.env, token) : null;
  if (!caller) return c.json({ error: 'Signed out, or this device was disconnected. Sign in again.' }, 401);
  c.set('caller', caller);
  await next();
};

// ── Input ─────────────────────────────────────────────────────────────────

/** A JSON object body of at most `max` bytes. An empty body is `{}`. */
async function jsonBody(request: Request, max = MAX_BODY): Promise<Record<string, unknown>> {
  if (Number(request.headers.get('content-length') ?? 0) > max) throw tooLarge('This request');
  let bytes: Uint8Array;
  try {
    bytes = await readLimited(request.body, max);
  } catch (error) {
    if (error instanceof BadRequest) throw tooLarge('This request');
    throw error;
  }
  if (!bytes.byteLength) return {};
  let value: unknown;
  try {
    value = JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    throw new BadRequest('The body is not valid JSON.');
  }
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new BadRequest('The body must be a JSON object.');
  return value as Record<string, unknown>;
}

/**
 * A file's path inside a paper as stored, or null. Relative, `/`-separated,
 * NFC, no empty, `.` or `..` segments, no control characters or backslashes,
 * and never inside `.oyl/` or `.git/`.
 */
export function syncPath(raw: unknown): string | null {
  if (typeof raw !== 'string' || !raw || raw.length > MAX_PATH || /[\\\u0000-\u001f\u007f]/.test(raw)) return null;
  if (raw.normalize('NFC') !== raw) return null;
  const segments = raw.split('/');
  if (segments.some((s) => !s || s === '.' || s === '..')) return null;
  return segments[0] === '.oyl' || segments[0] === '.git' ? null : raw;
}

export interface FileEntry {
  sha256: string;
  size: number;
}

export interface FileMap {
  /** Sorted by path. */
  entries: [string, FileEntry][];
  /** The stored form: JSON with the paths in sorted order. */
  json: string;
  size: number;
}

/** A revision's `files`, checked, in canonical form. */
export function parseFiles(raw: unknown): FileMap {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) throw new BadRequest('`files` must be an object of path to { sha256, size }.');
  const given = Object.entries(raw as Record<string, unknown>);
  if (given.length > MAX_FILES) throw new BadRequest(`A revision holds at most ${MAX_FILES} files.`);
  const folded = new Set<string>();
  const entries: [string, FileEntry][] = [];
  let size = 0;
  for (const [rawPath, value] of given) {
    const path = syncPath(rawPath);
    if (!path) throw new BadRequest(`Not a usable file path: ${JSON.stringify(rawPath.slice(0, 200))}`);
    // macOS folders ignore case: two such paths could not both be written out.
    const key = path.toLowerCase();
    if (folded.has(key)) throw new BadRequest(`Two files differ only in case: ${JSON.stringify(path.slice(0, 200))}`);
    folded.add(key);
    const f = value as Partial<FileEntry> | null;
    if (!f || typeof f !== 'object' || typeof f.sha256 !== 'string' || !SHA.test(f.sha256)) {
      throw new BadRequest(`${JSON.stringify(path.slice(0, 200))} needs a sha256 of 64 lowercase hex digits.`);
    }
    if (typeof f.size !== 'number' || !Number.isSafeInteger(f.size) || f.size < 0 || f.size > MAX_BLOB_BYTES) {
      throw new BadRequest(`${JSON.stringify(path.slice(0, 200))} needs a size in bytes, at most ${MAX_BLOB_BYTES}.`);
    }
    entries.push([path, { sha256: f.sha256, size: f.size }]);
    size += f.size;
  }
  entries.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  const json = `{${entries.map(([p, f]) => `${JSON.stringify(p)}:{"sha256":"${f.sha256}","size":${f.size}}`).join(',')}}`;
  return { entries, json, size };
}

function commitAuthor(raw: unknown, caller: Caller): string {
  if (raw === undefined || raw === null || raw === '') return caller.device ? `device:${caller.device}` : 'web';
  if (raw === 'web' || raw === 'agent') return raw;
  if (typeof raw === 'string' && raw.startsWith('device:')) {
    const name = cleanText(raw.slice('device:'.length), 60);
    if (name) return `device:${name}`;
  }
  throw new BadRequest('`author` must be `device:<name>`, `web` or `agent`.');
}

const commitMessage = (raw: unknown): string | null => {
  if (raw === undefined || raw === null) return null;
  if (typeof raw !== 'string') throw new BadRequest('`message` must be a string.');
  return raw.replace(/[\u0000-\u0008\u000b-\u001f\u007f]/g, '').trim().slice(0, MAX_MESSAGE) || null;
};

const paperName = (raw: unknown): string => {
  const name = cleanText(raw, MAX_NAME);
  if (!name) throw new BadRequest('A paper needs a name.');
  return name;
};

const paperEngine = (raw: unknown): Engine => {
  if (!ENGINES.includes(raw as Engine)) throw new BadRequest('`engine` must be `typst` or `latex`.');
  return raw as Engine;
};

const paperMain = (raw: unknown): string => {
  const main = syncPath(raw);
  if (!main) throw new BadRequest('`main` must be the entry file\'s path inside the paper, like `main.typ`.');
  return main;
};

// ── Papers ────────────────────────────────────────────────────────────────

interface PaperRow {
  id: string;
  name: string;
  engine: Engine;
  main: string;
  head?: string | null;
  updated_by?: string | null;
  created_at: string;
  updated_at: string;
  deleted_at?: string | null;
}

export const paperJson = (p: PaperRow) => ({
  id: p.id,
  name: p.name,
  engine: p.engine,
  main: p.main,
  head: p.head ?? null,
  updated_by: p.updated_by ?? null,
  created: p.created_at,
  updated: p.updated_at,
  deleted: Boolean(p.deleted_at),
});

async function ownedPaper(env: Env, userId: string, raw: unknown): Promise<PaperRow> {
  const id = recordId('paper_sync', raw);
  const row = await Surreal.system(env).query<PaperRow | null>(
    'RETURN (SELECT * FROM type::record($id) WHERE owner = type::record($owner))[0]',
    { id, owner: userId },
  );
  if (!row) throw new NotFound('Paper not found.');
  return row;
}

export async function listPapers(env: Env, userId: string) {
  const rows = await Surreal.system(env).query<PaperRow[]>(
    'SELECT * FROM paper_sync WHERE owner = type::record($owner) ORDER BY updated_at DESC',
    { owner: userId },
  );
  return (rows ?? []).map(paperJson);
}

/**
 * A new paper. An `id` the client made up (`paper_sync:k<32 hex>`) makes a
 * retry safe: the same id again answers the paper already made.
 */
export async function createPaper(env: Env, userId: string, body: Record<string, unknown>): Promise<{ created: boolean; paper: ReturnType<typeof paperJson> }> {
  const name = paperName(body.name);
  const engine = paperEngine(body.engine);
  const main = paperMain(body.main);
  let id = newKey('paper_sync');
  if (body.id !== undefined) {
    id = recordId('paper_sync', body.id);
    if (!/^paper_sync:k[0-9a-f]{32}$/.test(id)) throw new BadRequest('A paper id the client picks looks like paper_sync:k<32 hex digits>.');
  }
  const db = Surreal.system(env);
  try {
    const rows = await db.query<PaperRow[]>(
      'CREATE type::record($id) CONTENT { owner: type::record($owner), name: $name, engine: $engine, main: $main }',
      { id, owner: userId, name, engine, main },
    );
    return { created: true, paper: paperJson(rows[0]) };
  } catch (error) {
    if (!(error instanceof Error && /already exists/i.test(error.message))) throw error;
    const mine = await db.query<PaperRow | null>(
      'RETURN (SELECT * FROM type::record($id) WHERE owner = type::record($owner))[0]',
      { id, owner: userId },
    );
    if (!mine) throw new UserError('That paper id is taken.', 409);
    return { created: false, paper: paperJson(mine) };
  }
}

/** Renames, moves the entry file, changes the engine, or (un)deletes. */
export async function updatePaper(env: Env, userId: string, raw: unknown, body: Record<string, unknown>) {
  const paper = await ownedPaper(env, userId, raw);
  const sets: string[] = [];
  const vars: Record<string, unknown> = { id: paper.id, owner: userId };
  if ('name' in body) {
    sets.push('name = $name');
    vars.name = paperName(body.name);
  }
  if ('main' in body) {
    sets.push('main = $main');
    vars.main = paperMain(body.main);
  }
  if ('engine' in body) {
    sets.push('engine = $engine');
    vars.engine = paperEngine(body.engine);
  }
  if ('deleted' in body) {
    if (typeof body.deleted !== 'boolean') throw new BadRequest('`deleted` must be true or false.');
    sets.push(body.deleted ? 'deleted_at = deleted_at ?? time::now()' : 'deleted_at = NONE');
  }
  if (!sets.length) throw new BadRequest('Nothing to change: send `name`, `main`, `engine` or `deleted`.');
  const rows = await Surreal.system(env).query<PaperRow[]>(
    `UPDATE type::record($id) SET ${sets.join(', ')} WHERE owner = type::record($owner) RETURN AFTER`,
    vars,
  );
  if (!rows?.length) throw new NotFound('Paper not found.');
  return paperJson(rows[0]);
}

// ── Revisions ─────────────────────────────────────────────────────────────

interface RevisionRow {
  id: string;
  n: number;
  parent?: string | null;
  files: string;
  file_count: number;
  size: number;
  author: string;
  message?: string | null;
  created_at: string;
}

export function revisionJson(r: RevisionRow, withFiles: boolean) {
  return {
    revision: r.id,
    n: r.n,
    parent: r.parent ?? null,
    author: r.author,
    message: r.message ?? null,
    created: r.created_at,
    file_count: r.file_count,
    size: r.size,
    ...(withFiles ? { files: JSON.parse(r.files) as Record<string, FileEntry> } : {}),
  };
}

async function revisionOf(env: Env, userId: string, paperId: string, raw: unknown): Promise<RevisionRow | null> {
  return Surreal.system(env).query<RevisionRow | null>(
    'RETURN (SELECT * FROM type::record($id) WHERE paper = type::record($paper) AND owner = type::record($owner))[0]',
    { id: recordId('paper_revision', raw), paper: paperId, owner: userId },
  );
}

export async function readHead(env: Env, userId: string, rawPaper: unknown) {
  const paper = await ownedPaper(env, userId, rawPaper);
  const head = paper.head ? await revisionOf(env, userId, paper.id, paper.head) : null;
  if (!head) throw new NotFound('This paper has no revisions yet.');
  return revisionJson(head, true);
}

export async function readRevision(env: Env, userId: string, rawPaper: unknown, rawRevision: unknown) {
  const paper = await ownedPaper(env, userId, rawPaper);
  const row = await revisionOf(env, userId, paper.id, rawRevision);
  if (!row) throw new NotFound('Revision not found.');
  return revisionJson(row, true);
}

/** Newest first, without file maps. `before`: only revisions numbered below it. */
export async function listRevisions(env: Env, userId: string, rawPaper: unknown, limit: number, before: number | null) {
  const paper = await ownedPaper(env, userId, rawPaper);
  const rows = await Surreal.system(env).query<RevisionRow[]>(
    `SELECT id, n, parent, author, message, created_at, file_count, size FROM paper_revision
     WHERE paper = type::record($paper) AND owner = type::record($owner)${before ? ' AND n < $before' : ''}
     ORDER BY n DESC LIMIT $limit`,
    { paper: paper.id, owner: userId, before, limit },
  );
  return (rows ?? []).map((r) => revisionJson(r, false));
}

export type CommitResult =
  | { status: 201; body: { revision: string; n: number } }
  | { status: 200; body: { revision: string; n: number; unchanged: true } };

/**
 * Makes a new head, compare-and-set: `parent` must be the current head (null
 * for a paper without revisions), else 409 with the head. Every blob must be
 * stored already (422 lists the missing ones). The head moves and the
 * revision appears in one transaction, or neither happens.
 *
 * Asking for what already is answers 200 without a new revision: the same
 * files on the head, or a retry of a commit that went through (the head was
 * made on `parent` with exactly these files).
 */
export async function commitRevision(env: Env, caller: Caller, rawPaper: unknown, body: Record<string, unknown>): Promise<CommitResult> {
  const paperId = recordId('paper_sync', rawPaper);
  if (body.parent !== null && body.parent !== undefined && typeof body.parent !== 'string') {
    throw new BadRequest('`parent` must be the revision the commit was made on, or null for the first.');
  }
  const parent = typeof body.parent === 'string' ? recordId('paper_revision', body.parent) : null;
  const files = parseFiles(body.files);
  const author = commitAuthor(body.author, caller);
  const message = commitMessage(body.message);
  const db = Surreal.system(env);
  let blobsChecked = false;

  for (let attempt = 0; attempt < 4; attempt++) {
    const paper = await ownedPaper(env, caller.userId, paperId);
    if (paper.deleted_at) throw new UserError('This paper was removed from the cloud. Restore it before syncing to it.', 410);
    const head = paper.head ? await revisionOf(env, caller.userId, paper.id, paper.head) : null;
    if ((head?.id ?? null) !== parent) {
      if (head && (head.parent ?? null) === parent && head.files === files.json) {
        return { status: 200, body: { revision: head.id, n: head.n, unchanged: true } };
      }
      throw new UserError('The paper changed since this commit\'s parent. Pull, merge and commit again.', 409, {
        head: head ? revisionJson(head, true) : null,
      });
    }
    if (head && head.files === files.json) return { status: 200, body: { revision: head.id, n: head.n, unchanged: true } };

    if (!blobsChecked) {
      const held = await blobSizes(env, caller.userId, [...new Set(files.entries.map(([, f]) => f.sha256))]);
      const missing = [...new Set(files.entries.map(([, f]) => f.sha256).filter((h) => !held.has(h)))];
      if (missing.length) throw new UserError('Upload these blobs first.', 422, { missing });
      const wrong = files.entries.find(([, f]) => held.get(f.sha256) !== f.size);
      if (wrong) throw new BadRequest(`The size of ${JSON.stringify(wrong[0].slice(0, 200))} does not match its blob (${held.get(wrong[1].sha256)} bytes).`);
      blobsChecked = true;
    }

    const revision = newKey('paper_revision');
    const n = (head?.n ?? 0) + 1;
    try {
      await db.queryAll(
        `BEGIN TRANSACTION;
         LET $expected = IF $parent = NONE { NONE } ELSE { type::record($parent) };
         LET $claimed = UPDATE type::record($paper) SET head = type::record($revision), updated_by = $author
           WHERE owner = type::record($owner) AND deleted_at = NONE AND head = $expected RETURN id;
         IF array::len($claimed) != 1 { THROW 'sync_head_moved'; };
         CREATE type::record($revision) CONTENT {
           owner: type::record($owner), paper: type::record($paper), n: $n, parent: $expected,
           files: $files, file_count: $count, size: $size, author: $author, message: $message
         };
         COMMIT TRANSACTION;`,
        {
          paper: paperId, owner: caller.userId, parent, revision, n, author, message,
          files: files.json, count: files.entries.length, size: files.size,
        },
      );
      return { status: 201, body: { revision, n } };
    } catch (error) {
      // Another commit won the race (or the database asks to try again): read
      // the head afresh, which answers 409, or 200 for a commit already made.
      if (error instanceof Error && /sync_head_moved|Transaction conflict|already contains/i.test(error.message)) continue;
      throw error;
    }
  }
  throw new UserError('The paper is busy. Try again in a moment.', 503);
}

// ── Blobs ─────────────────────────────────────────────────────────────────

/** A blob's row: one per owner and hash, the key derived from both. */
export const blobId = async (owner: string, sha: string) => `sync_blob:b${(await sha256Hex(`${owner}\n${sha}`)).slice(0, 40)}`;
const partPath = (id: string, n: number) => `/papers/${id.slice(id.indexOf(':') + 1)}_${n}`;
/** Where an upload's parts wait until its hash is checked. */
const stagingPath = (upload: string, n: number) => `/papers/up${upload}_${n}`;

/** The sizes of the caller's blobs among `hashes`. */
async function blobSizes(env: Env, owner: string, hashes: string[]): Promise<Map<string, number>> {
  if (!hashes.length) return new Map();
  await ensureServerTable(env, 'sync_blob');
  const ids = await Promise.all(hashes.map((h) => blobId(owner, h)));
  const rows = await Surreal.system(env).query<{ sha256: string; size: number }[]>(
    `LET $records = $ids.map(|$i| type::record($i));
     SELECT sha256, size FROM $records WHERE owner = type::record($owner);`,
    { ids, owner },
  );
  return new Map((rows ?? []).map((r) => [r.sha256, r.size]));
}

/** Which of `hashes` the caller has not uploaded, in the order asked. */
export async function missingBlobs(env: Env, owner: string, raw: unknown): Promise<string[]> {
  if (!Array.isArray(raw)) throw new BadRequest('`hashes` must be an array of SHA-256 digests.');
  if (raw.length > MAX_HASHES) throw new BadRequest(`Ask about at most ${MAX_HASHES} hashes at once.`);
  for (const h of raw) if (typeof h !== 'string' || !SHA.test(h)) throw new BadRequest('A hash is 64 lowercase hex digits.');
  const hashes = [...new Set(raw as string[])];
  const held = await blobSizes(env, owner, hashes);
  return hashes.filter((h) => !held.has(h));
}

/**
 * Stores a blob from a request body, checking it against its SHA-256 as it
 * streams in. Parts go to a place of their own first and move to the blob's
 * once the hash matched, so a wrong upload never touches a good one. A blob
 * the caller already has is not read again.
 */
export async function putBlob(
  env: Env, owner: string, sha: string, body: ReadableStream<Uint8Array> | null, declared: number | null,
): Promise<{ created: boolean; sha256: string; size: number }> {
  if (!SHA.test(sha)) throw new BadRequest('A blob is named by its SHA-256: 64 lowercase hex digits.');
  if (declared !== null && declared > MAX_BLOB_BYTES) throw tooLarge('This file');
  await ensureServerTable(env, 'sync_blob');
  const db = Surreal.system(env);
  const id = await blobId(owner, sha);
  const held = await db.query<{ size: number } | null>(
    'RETURN (SELECT size FROM type::record($id) WHERE owner = type::record($owner))[0]',
    { id, owner },
  );
  if (held) {
    await body?.cancel().catch(() => undefined);
    return { created: false, sha256: sha, size: held.size };
  }

  const upload = crypto.randomUUID().replaceAll('-', '');
  const hash = new Sha256();
  let size = 0;
  let parts = 0;
  let inflight: Promise<unknown>[] = [];
  /** Sends one part; at most three at once. */
  const send = async (bytes: Uint8Array) => {
    const sent = db.query(`type::file('materials', $key).put(encoding::base64::decode($b64));`, {
      key: stagingPath(upload, parts++),
      b64: toBase64(bytes),
    });
    sent.catch(() => undefined);
    inflight.push(sent);
    if (inflight.length >= 3) await inflight.shift();
  };
  const part = new Uint8Array(PART_BYTES);
  let fill = 0;
  const reader = body?.getReader();
  try {
    for (;;) {
      if (!reader) break;
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > MAX_BLOB_BYTES) {
        await reader.cancel().catch(() => undefined);
        throw tooLarge('This file');
      }
      hash.update(value);
      for (let off = 0; off < value.byteLength; ) {
        const take = Math.min(PART_BYTES - fill, value.byteLength - off);
        part.set(value.subarray(off, off + take), fill);
        fill += take;
        off += take;
        // The part is copied out (base64) before `send` returns, so it can be refilled.
        if (fill === PART_BYTES) {
          fill = 0;
          await send(part);
        }
      }
    }
    if (fill) await send(part.subarray(0, fill));
    await Promise.all(inflight);
    inflight = [];
    const actual = hash.hex();
    if (actual !== sha) throw new UserError('The bytes do not match their SHA-256.', 400, { sha256: actual, size });
    // Into place, then the row that points at them.
    await db.query(
      `FOR $n IN array::range(0, $parts) {
         type::file('materials', $from + <string>$n).rename($to + <string>$n);
       };`,
      { parts, from: stagingPath(upload, 0).slice(0, -1), to: partPath(id, 0).slice(0, -1) },
    );
    try {
      await db.query(
        'CREATE type::record($id) CONTENT { owner: type::record($owner), sha256: $sha, size: $size, parts: $parts }',
        { id, owner, sha, size, parts },
      );
    } catch (error) {
      // The same bytes arrived twice at once: the other upload's row stands.
      if (!(error instanceof Error && /already (exists|contains)/i.test(error.message))) throw error;
      return { created: false, sha256: sha, size };
    }
    return { created: true, sha256: sha, size };
  } catch (error) {
    await Promise.allSettled(inflight);
    if (parts) {
      await db
        .query(`FOR $n IN array::range(0, $parts) { type::file('materials', $from + <string>$n).delete(); };`, {
          parts,
          from: stagingPath(upload, 0).slice(0, -1),
        })
        .catch((err) => console.error('[sync] staging cleanup', err));
    }
    throw error;
  }
}

/**
 * A blob's bytes, streamed part by part. The hash is the ETag and the bytes
 * never change, so clients may cache it for good.
 */
export async function blobResponse(env: Env, owner: string, sha: string, ifNoneMatch: string | undefined): Promise<Response> {
  if (!SHA.test(sha)) throw new BadRequest('A blob is named by its SHA-256: 64 lowercase hex digits.');
  await ensureServerTable(env, 'sync_blob');
  const db = Surreal.system(env);
  const id = await blobId(owner, sha);
  const row = await db.query<{ size: number; parts: number } | null>(
    'RETURN (SELECT size, parts FROM type::record($id) WHERE owner = type::record($owner))[0]',
    { id, owner },
  );
  if (!row) throw new NotFound('No such blob.');
  const headers: Record<string, string> = { etag: `"${sha}"`, 'cache-control': 'private, max-age=31536000, immutable' };
  const tags = (ifNoneMatch ?? '').split(',').map((t) => t.trim().replace(/^W\//, ''));
  if (tags.includes(`"${sha}"`) || tags.includes('*')) return new Response(null, { status: 304, headers });

  const read = async (n: number) =>
    fromBase64(await db.query<string>(`RETURN encoding::base64::encode(type::file('materials', $key).get());`, { key: partPath(id, n) }));
  // The first part is read before answering, so a missing file is a JSON error.
  const first = row.parts ? await read(0) : new Uint8Array();
  let next = 1;
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      if (first.byteLength) controller.enqueue(first);
    },
    async pull(controller) {
      if (next >= row.parts) return controller.close();
      controller.enqueue(await read(next++));
    },
  });
  return new Response(stream, {
    headers: { ...headers, 'content-type': 'application/octet-stream', 'content-length': String(row.size) },
  });
}

// ── Routes (/api/sync, exposed in sp00ky.yml) ─────────────────────────────

export const syncRoutes = new Hono<AppEnv>();

// The device flow. Starting and polling are public: the device has no key yet.
syncRoutes.post('/device', async (c) => {
  const body = await jsonBody(c.req.raw);
  return c.json(await startDeviceFlow(c.env, deviceName(body.device_name), connectOrigin(c.req.raw, c.env)));
});

syncRoutes.post('/device/token', async (c) => {
  const body = await jsonBody(c.req.raw);
  const answer = await pollDevice(c.env, body.device_code);
  return c.json(answer.body, answer.status, {
    'cache-control': 'no-store',
    ...(answer.retryAfter ? { 'retry-after': String(answer.retryAfter) } : {}),
  });
});

// The web app's /connect page, with the signed-in user's session only (never a device key).
syncRoutes.post('/device/request', requireUser, async (c) => {
  const body = await jsonBody(c.req.raw);
  return c.json(await deviceRequest(c.env, body.user_code));
});

syncRoutes.post('/device/approve', requireUser, async (c) => {
  const body = await jsonBody(c.req.raw);
  return c.json(await approveDevice(c.env, c.var.caller.userId, body.user_code, body.allow));
});

syncRoutes.get('/papers', requireSyncCaller, async (c) => c.json(await listPapers(c.env, c.var.caller.userId)));

syncRoutes.post('/papers', requireSyncCaller, async (c) => {
  const made = await createPaper(c.env, c.var.caller.userId, await jsonBody(c.req.raw));
  return c.json(made.paper, made.created ? 201 : 200);
});

syncRoutes.patch('/papers/:id', requireSyncCaller, async (c) =>
  c.json(await updatePaper(c.env, c.var.caller.userId, c.req.param('id'), await jsonBody(c.req.raw))),
);

syncRoutes.get('/papers/:id/head', requireSyncCaller, async (c) => c.json(await readHead(c.env, c.var.caller.userId, c.req.param('id'))));

syncRoutes.get('/papers/:id/revisions', requireSyncCaller, async (c) => {
  const limit = Number.parseInt(c.req.query('limit') ?? '', 10);
  const before = Number.parseInt(c.req.query('before') ?? '', 10);
  return c.json(await listRevisions(
    c.env, c.var.caller.userId, c.req.param('id'),
    Number.isFinite(limit) ? Math.min(MAX_REVISIONS, Math.max(1, limit)) : 50,
    Number.isFinite(before) && before > 0 ? before : null,
  ));
});

syncRoutes.get('/papers/:id/revisions/:rev', requireSyncCaller, async (c) =>
  c.json(await readRevision(c.env, c.var.caller.userId, c.req.param('id'), c.req.param('rev'))),
);

syncRoutes.post('/papers/:id/revisions', requireSyncCaller, async (c) => {
  const result = await commitRevision(c.env, c.var.caller, c.req.param('id'), await jsonBody(c.req.raw, MAX_COMMIT_BODY));
  return c.json(result.body, result.status);
});

syncRoutes.post('/blobs/missing', requireSyncCaller, async (c) => {
  const body = await jsonBody(c.req.raw, MAX_COMMIT_BODY);
  return c.json({ missing: await missingBlobs(c.env, c.var.caller.userId, body.hashes) });
});

syncRoutes.put('/blobs/:sha256', requireSyncCaller, async (c) => {
  const length = c.req.header('content-length');
  const stored = await putBlob(c.env, c.var.caller.userId, c.req.param('sha256'), c.req.raw.body, length ? Number(length) : null);
  return c.json({ sha256: stored.sha256, size: stored.size }, stored.created ? 201 : 200);
});

syncRoutes.get('/blobs/:sha256', requireSyncCaller, (c) =>
  blobResponse(c.env, c.var.caller.userId, c.req.param('sha256'), c.req.header('if-none-match')),
);
