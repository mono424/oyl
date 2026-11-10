import type { Env } from './env';
import serverSchema from './server-schema.surql';
import { BadRequest, Conflict, NotFound, Surreal, UserError, recordId, toBase64 } from './surreal';
import { memoryConfigured, memoryQueryable } from './memory';

export interface CourseRow {
  id: string;
  owner: string;
  title: string;
  description?: string | null;
  language: string;
  tutor_notes?: string | null;
}

export interface MaterialRow {
  id: string;
  owner: string;
  course: string;
  title: string;
  /** tex: LaTeX source, indexed through its Markdown (shared/latex.ts). */
  kind: 'text' | 'markdown' | 'pdf' | 'tex';
  content: string;
  status: 'pending' | 'processing' | 'ready' | 'failed';
  page_count?: number | null;
  chunk_count: number;
  image_count: number;
  index_rev: number;
  /** Last reached section, not demonstrated mastery. Uploading never advances it. */
  learned_through: number;
  /** Revision whose Agent Memory graph finished extracting. */
  memory_rev: number;
  source_name?: string | null;
}

let schemaApplied = false;
const tablesSeen = new Set<string>();

/**
 * Applying the server schema takes about 45 s on the deployed database, too
 * long for a request someone waits on, on every cold isolate it lands on.
 * Once `table` exists it has been applied, and other routes keep it current.
 */
export async function ensureServerTable(env: Env, table: string): Promise<void> {
  if (schemaApplied || tablesSeen.has(table)) return;
  const tables = await Surreal.system(env).query<Record<string, unknown>>('RETURN (INFO FOR DB).tables;');
  if (!tables[table]) await ensureServerSchema(env);
  tablesSeen.add(table);
}

/**
 * Applies server-schema.surql once per isolate (it is idempotent). A flag, not
 * a shared promise: a Worker must not await I/O that another request started.
 */
export async function ensureServerSchema(env: Env): Promise<void> {
  if (schemaApplied) return;
  for (let attempt = 0; ; attempt++) {
    if (schemaApplied) return;
    try {
      await Surreal.system(env).queryAll(serverSchema);
      schemaApplied = true;
      return;
    } catch (error) {
      // Cold requests can apply the same idempotent definitions concurrently.
      // Retry only the database's transient write conflicts, with each request
      // owning its own I/O. Schema or permission errors must still surface.
      if (attempt >= 5 || !(error instanceof Error) || !/Transaction conflict:.*(?:Resource busy|can be retried)/i.test(error.message)) throw error;
      await new Promise((resolve) => setTimeout(resolve, 50 * 2 ** attempt + Math.random() * 100));
    }
  }
}

export async function getOwnedCourse(env: Env, userId: string, courseId: string): Promise<CourseRow> {
  const id = recordId('course', courseId);
  const row = await Surreal.system(env).query<CourseRow | null>(
    'RETURN (SELECT * FROM type::record($id) WHERE owner = type::record($owner))[0]',
    { id, owner: userId },
  );
  if (!row) throw new NotFound('Course not found.');
  return row;
}

export async function getOwnedMaterial(env: Env, userId: string, materialId: string): Promise<MaterialRow> {
  const id = recordId('material', materialId);
  // A material the browser just created may still be on its way to the
  // database (sp00ky writes locally first), so give it a moment.
  for (let attempt = 0; attempt < 4; attempt++) {
    const row = await Surreal.system(env).query<MaterialRow | null>(
      'RETURN (SELECT * FROM type::record($id) WHERE owner = type::record($owner))[0]',
      { id, owner: userId },
    );
    if (row) return row;
    await new Promise((r) => setTimeout(r, 500 * (attempt + 1)));
  }
  throw new NotFound('Material not found.');
}

// ── Embeddings ────────────────────────────────────────────────────────────

type EmbeddingOutput = { data?: number[][]; shape?: number[] };

/** Embeds texts with Workers AI. `kind` picks the query or document side. */
export async function embed(env: Env, texts: string[], kind: 'documents' | 'queries'): Promise<number[][]> {
  if (texts.length === 0) return [];
  const model = env.EMBEDDING_MODEL;
  // qwen3-embedding is asymmetric (separate query/document inputs); the bge
  // family takes plain `text`.
  const input = model.includes('qwen3-embedding') ? { [kind]: texts } : { text: texts };
  const out = (await env.AI.run(model as Parameters<Ai['run']>[0], input as never)) as EmbeddingOutput;
  if (!out.data || out.data.length !== texts.length) throw new Error(`Embedding model returned no vectors (${model}).`);
  return out.data;
}

// ── Search ────────────────────────────────────────────────────────────────

export interface SearchHit {
  material: string;
  materialTitle: string;
  materialKind: MaterialRow['kind'];
  course: string;
  courseTitle: string;
  /** The chunk's place in its material's index (revision `rev`). */
  position: number;
  rev: number;
  heading: string | null;
  text: string;
  /** PDF pages the passage comes from; null for typed and Markdown material. */
  page: number | null;
  pageEnd: number | null;
  score: number;
}

/**
 * Hybrid search over one course, or all of the owner's courses when
 * `courseId` is null: vector (HNSW) and keyword (BM25) hits fused with
 * reciprocal rank fusion. Optionally limited to one material. Chunks of a
 * material deleted mid-indexing are skipped (their `material` no longer
 * resolves).
 */
export async function searchCourse(
  env: Env,
  courseId: string | null,
  query: string,
  opts: { k?: number; materialId?: string | null; ownerId: string; reachedOnly?: boolean },
): Promise<SearchHit[]> {
  const q = query.trim();
  if (!q) return [];
  await ensureServerSchema(env);
  const k = Math.min(Math.max(opts.k ?? 6, 1), 20);
  const [vec] = await embed(env, [q], 'queries');
  const filter = `${courseId ? 'course = type::record($course_id) AND ' : ''}owner = $owner AND material.owner = $owner
    AND material.status = 'ready' AND rev = material.index_rev
    ${opts.reachedOnly ? 'AND position <= (material.learned_through ?? -1)' : ''}
    ${opts.materialId ? 'AND material = type::record($material)' : ''}`;
  const result = await Surreal.system(env).query<{
    hits: { id: string; rrf_score: number }[];
    rows: {
      id: string; text: string; heading: string | null; material: string; material_title: string;
      material_kind: MaterialRow['kind']; course: string; course_title: string; position: number; rev: number;
      page?: number | null; page_end?: number | null;
    }[];
  }>(
    `LET $owner = type::record($owner_id);
     LET $vs = SELECT id, vector::distance::knn() AS distance FROM chunk WHERE ${filter} AND embedding <|${k * 2},64|> $vec;
     LET $ft = SELECT id, search::score(1) AS score FROM chunk WHERE ${filter} AND text @1@ $q ORDER BY score DESC LIMIT ${k * 2};
     LET $hits = search::rrf([$vs, $ft], ${k}, 60);
     RETURN {
       hits: $hits.map(|$h| { id: $h.id, rrf_score: $h.rrf_score }),
       rows: SELECT id, text, heading, material, material.title AS material_title, material.kind AS material_kind,
         course, course.title AS course_title, position, rev, page, page_end FROM $hits.id
     };`,
    { course_id: courseId, owner_id: opts.ownerId, material: opts.materialId ?? null, vec, q },
  );
  const rows = new Map((result?.rows ?? []).map((r) => [r.id, r]));
  return (result?.hits ?? []).flatMap((h) => {
    const r = rows.get(h.id);
    return r
      ? [{
          material: r.material, materialTitle: r.material_title, materialKind: r.material_kind,
          course: r.course, courseTitle: r.course_title,
          position: r.position, rev: r.rev, heading: r.heading, text: r.text,
          page: r.page ?? null, pageEnd: r.page_end ?? null, score: h.rrf_score,
        }]
      : [];
  });
}

// ── Indexing ──────────────────────────────────────────────────────────────

/**
 * Takes over a material for a new processing run: bumps `index_rev` (any run
 * still going for an older rev stops at its next step) and marks it
 * processing, in one statement.
 */
async function claimRev(env: Env, materialId: string, detail: string): Promise<number> {
  const rows = await Surreal.system(env).query<{ index_rev: number }[]>(
    `UPDATE type::record($id) SET index_rev += 1, learned_through = -1, memory_rev = 0,
       status = 'processing', status_detail = $detail, error = NONE RETURN index_rev`,
    { id: materialId, detail },
  );
  if (!rows?.length) throw new NotFound('Material not found.');
  return rows[0].index_rev;
}

async function failRev(env: Env, materialId: string, rev: number, err: unknown) {
  const message = err instanceof UserError ? err.message : 'Processing could not start. Try again.';
  console.error(`[ingest] ${materialId} rev ${rev} could not start`, err);
  await Surreal.system(env)
    .query(
      `UPDATE type::record($id) SET status = 'failed', status_detail = NONE, error = $error WHERE index_rev = $rev`,
      { id: materialId, rev, error: message },
    )
    .catch(() => {});
}

/** Marks a material failed unless a processing run is under way. */
export async function markFailed(env: Env, materialId: string, message: string) {
  await Surreal.system(env)
    .query(`UPDATE type::record($id) SET status = 'failed', error = $error WHERE status != 'processing'`, { id: materialId, error: message })
    .catch((err) => console.error('[ingest] could not mark failed', err));
}

async function launch(env: Env, materialId: string, rev: number, convert: boolean, memoryOnly = false) {
  const key = materialId.slice(materialId.indexOf(':') + 1);
  await env.INGEST.create({
    id: `${key}-${rev}-${memoryOnly ? 'm' : ''}${Date.now().toString(36)}`,
    params: { materialId, rev, convert, ...(memoryOnly ? { memoryOnly } : {}) },
  });
}

/**
 * Builds the knowledge graph of a ready material from its current text,
 * keeping its search index and lecture progress. Nothing to do when Agent
 * Memory is off, or the graph of this revision is already built (unless
 * `force`).
 */
export async function startMemoryBuild(env: Env, material: MaterialRow, force = false): Promise<{ started: boolean; rev: number }> {
  const rev = material.index_rev;
  if (!memoryConfigured(env) || material.status !== 'ready' || !material.chunk_count) return { started: false, rev };
  if (!force && material.memory_rev === rev) return { started: false, rev };
  await launch(env, material.id, rev, false, true);
  return { started: true, rev };
}

/** Graph builds running at once, across everyone: Agent Memory refuses a burst. */
const GRAPH_CONCURRENT = 6;
/** Builds tried per material revision before it is left without a graph. */
const GRAPH_ATTEMPTS = 8;
const ACTIVE_RUN = new Set(['queued', 'running', 'paused', 'waiting', 'waitingForPause', 'unknown']);

interface GraphBuild { material: string; rev: number; attempts: number; run: string }

/**
 * Starts graph builds of ready materials that have no graph for their current
 * text, e.g. material processed before Agent Memory was set up, or a build
 * that failed. At most GRAPH_CONCURRENT run at once; materials of `ownerId`
 * (and `courseId`) go first. Called by the graph view, the study tools, the
 * graph-backfill schedule and each build as it ends (`finishing`: its own
 * run, which no longer counts as running).
 */
export async function buildMissingGraphs(
  env: Env, ownerId: string | null, courseId: string | null = null, finishing?: string,
): Promise<number> {
  if (!memoryConfigured(env)) return 0;
  await ensureServerSchema(env);
  const db = Surreal.system(env);
  const [rows, builds] = await db.queryAll(
    `SELECT id, owner, course, index_rev, created_at FROM material
       WHERE status = 'ready' AND chunk_count > 0 AND (memory_rev ?? 0) != index_rev ORDER BY created_at;
     SELECT material, rev, attempts, run FROM graph_build;`,
  ) as [{ id: string; owner: string; course: string; index_rev: number }[], GraphBuild[]];
  const byMaterial = new Map(builds.map((b) => [b.material, b]));
  const course = courseId ? recordId('course', courseId) : null;

  let active = 0;
  const due: { m: (typeof rows)[number]; attempts: number }[] = [];
  for (let i = 0; i < rows.length; i += 20) {
    const slice = rows.slice(i, i + 20);
    const running = await Promise.all(slice.map(async (m) => {
      const b = byMaterial.get(m.id);
      if (!b || b.rev !== m.index_rev || b.run === finishing) return false;
      try {
        return ACTIVE_RUN.has((await (await env.INGEST.get(b.run)).status()).status);
      } catch {
        return false; // The run is gone.
      }
    }));
    slice.forEach((m, j) => {
      const b = byMaterial.get(m.id);
      const attempts = b && b.rev === m.index_rev ? b.attempts : 0;
      if (running[j]) active++;
      else if (attempts < GRAPH_ATTEMPTS) due.push({ m, attempts });
    });
  }
  const mine = (m: (typeof rows)[number]) => (!ownerId || m.owner === ownerId) && (!course || m.course === course);
  due.sort((x, y) => Number(mine(y.m)) - Number(mine(x.m)));
  const start = due.slice(0, Math.max(0, GRAPH_CONCURRENT - active));
  // During an Agent Memory retrieval outage, wait instead of using up attempts.
  if (!start.length || !(await memoryQueryable(env))) return 0;

  const runs = start.map(({ m, attempts }) => ({
    id: `${m.id.slice(m.id.indexOf(':') + 1)}-${m.index_rev}-g${attempts}-${Date.now().toString(36)}`,
    material: m.id, rev: m.index_rev, attempts: attempts + 1,
  }));
  // Recorded first: a run that then fails to start counts as ended.
  await db.query(
    `FOR $r IN $runs {
       UPSERT type::record('graph_build', record::id(type::record($r.material))) CONTENT {
         material: type::record($r.material), rev: $r.rev, attempts: $r.attempts, run: $r.id, started_at: time::now()
       };
     };`,
    { runs },
  );
  const created = await env.INGEST.createBatch(runs.map((r) => ({
    id: r.id, params: { materialId: r.material, rev: r.rev, convert: false, memoryOnly: true },
  })));
  return created.length;
}

/**
 * (Re)builds a material's search index from its current Markdown, e.g. after
 * an edit. Never re-converts a PDF, so edits to converted text are kept.
 */
export async function startIngest(env: Env, materialId: string, detail = 'Queued'): Promise<number> {
  await ensureServerSchema(env);
  const rev = await claimRev(env, materialId, detail);
  try {
    await launch(env, materialId, rev, false);
  } catch (err) {
    await failRev(env, materialId, rev, err);
    throw err;
  }
  return rev;
}

export const MAX_PDF_BYTES = 30 * 1024 * 1024;
export const PART_BYTES = 1024 * 1024;

const isPdf = (b: Uint8Array) => b.length > 4 && b[0] === 0x25 && b[1] === 0x50 && b[2] === 0x44 && b[3] === 0x46; // %PDF

/** Throws a user-facing error unless `bytes` looks like a PDF we can take. */
export function validatePdf(bytes: Uint8Array) {
  if (bytes.byteLength === 0) throw new BadRequest('The file is empty.');
  if (bytes.byteLength > MAX_PDF_BYTES) throw new BadRequest('PDFs up to 30 MB are supported.');
  if (!isPdf(bytes)) throw new BadRequest('That file is not a PDF.');
}

/**
 * Stores a material's PDF and converts it. The PDF is the original the viewer
 * shows and citations point into, so it is kept until the material is
 * deleted: only a failed upload may be replaced, unless `replace` (a source
 * whose original changed, fetch/run.ts), which re-indexes from the new file.
 */
export async function storePdfAndIngest(
  env: Env,
  material: Pick<MaterialRow, 'id' | 'owner'>,
  bytes: Uint8Array,
  fileName: string | null,
  opts: { replace?: boolean } = {},
): Promise<void> {
  validatePdf(bytes);
  await ensureServerSchema(env);
  const held = await Surreal.system(env).query<{ status: string | null; parts: number }>(
    `RETURN {
       status: (SELECT VALUE status FROM ONLY type::record($m)),
       parts: count(SELECT id FROM source_part WHERE material = type::record($m))
     }`,
    { m: material.id },
  );
  if (held.parts > 0 && held.status !== 'failed' && !opts.replace) {
    throw new Conflict('This material already has its PDF. Upload the new file as new material.');
  }
  // Claim first, so a run still converting the previous upload stops before
  // it can read the parts being replaced.
  const rev = await claimRev(env, material.id, 'Uploading the PDF');
  try {
    const db = Surreal.system(env);
    await db.query('DELETE source_part WHERE material = type::record($m)', { m: material.id });
    for (let idx = 0, off = 0; off < bytes.byteLength; idx++, off += PART_BYTES) {
      // The part's bytes are the bucket file /sources/<id>, written before
      // the row that points at it.
      await db.query(
        `type::file('materials', '/sources/' + <string>record::id(type::record($id))).put(encoding::base64::decode($b64));
         CREATE type::record($id) CONTENT {
           owner: type::record($owner), material: type::record($m), idx: $idx
         };`,
        {
          id: `source_part:p${crypto.randomUUID().replaceAll('-', '')}`,
          owner: material.owner,
          m: material.id,
          idx,
          b64: toBase64(bytes.subarray(off, off + PART_BYTES)),
        },
      );
    }
    await db.query(
      `UPDATE type::record($m) SET kind = 'pdf', source_name = $name, source_size = $size, status_detail = 'Reading the PDF'`,
      { m: material.id, name: fileName, size: bytes.byteLength },
    );
    await launch(env, material.id, rev, true);
  } catch (err) {
    await failRev(env, material.id, rev, err);
    throw err;
  }
}

/** Reads a request or response body, refusing more than `max` bytes. */
export async function readLimited(body: ReadableStream<Uint8Array> | null, max: number): Promise<Uint8Array> {
  if (!body) return new Uint8Array();
  const chunks: Uint8Array[] = [];
  let total = 0;
  const reader = body.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > max) {
      await reader.cancel();
      throw new BadRequest(`Files up to ${Math.round(max / 1024 / 1024)} MB are supported.`);
    }
    chunks.push(value);
  }
  const out = new Uint8Array(total);
  let off = 0;
  for (const c of chunks) {
    out.set(c, off);
    off += c.byteLength;
  }
  return out;
}

/** Downloads a PDF the user pointed an AI client at. https only, size-capped. */
export async function fetchPdf(url: string): Promise<{ bytes: Uint8Array; name: string }> {
  const u = new URL(url);
  if (u.protocol !== 'https:') throw new BadRequest('Only https:// URLs are supported.');
  const res = await fetch(u, { redirect: 'follow', headers: { accept: 'application/pdf' } });
  if (new URL(res.url || u.href).protocol !== 'https:') throw new BadRequest('The URL redirected away from https.');
  if (!res.ok) throw new BadRequest(`Download failed (${res.status}).`);
  const bytes = await readLimited(res.body, MAX_PDF_BYTES);
  validatePdf(bytes);
  const name = decodeURIComponent(u.pathname.split('/').pop() || 'document.pdf');
  return { bytes, name };
}

// ── API keys ──────────────────────────────────────────────────────────────

export async function createApiKey(env: Env, userId: string, name: string): Promise<{ id: string; key: string }> {
  await ensureServerSchema(env);
  const { randomKey, sha256Hex } = await import('./auth');
  const key = `sc_${randomKey(30)}`;
  const id = `api_key:k${crypto.randomUUID().replaceAll('-', '')}`;
  await Surreal.system(env).query(
    `CREATE type::record($id) CONTENT { owner: type::record($owner), name: $name, prefix: $prefix, key_hash: $hash }`,
    { id, owner: userId, name: name.trim().slice(0, 80) || 'AI client', prefix: key.slice(0, 7), hash: await sha256Hex(key) },
  );
  return { id, key };
}
