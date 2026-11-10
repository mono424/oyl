import type { Connector, Plan } from '@hockeymask/core';
import { sha256Hex } from '../auth';
import type { Env } from '../env';
import { ensureServerSchema, MAX_PDF_BYTES, readLimited, startIngest, storePdfAndIngest, validatePdf } from '../service';
import { BadRequest, NotFound, Surreal, UserError, recordId } from '../surreal';
import { CanvasClient, canvasBase, canvasCourseId } from './canvas';
import { compileMapping, type ImportRow, rowsConnector, runMapping, type SourceRow, templateMapping } from './mapping';
import { publicUrl, safeFetch } from './net';
import { openToken, sealToken } from './secret';
import { fetchPage, linkVersion, pageRows } from './web';

/**
 * Sources (`fetcher` rows) end to end: read the source through its connector,
 * map the rows with hockeymask, compare them with what the source imported
 * before (`fetched_item`), and import what is new or changed as lecture
 * material. A run is a Workflow (workflow.ts) that calls these one step at a
 * time; previews call the same functions and write nothing.
 */

/** Items imported (or replaced) per run; the rest wait for the next one. */
export const MAX_ITEMS_PER_RUN = 15;
/** Linked files checked for changes per run (HEAD requests). */
const MAX_HEADS = 60;
/** A run that started this long ago without finishing is assumed lost. */
const STALE_RUN_MS = 2 * 60 * 60 * 1000;
/** A run that left items behind checks again this soon. */
const DEFERRED_RETRY_MS = 10 * 60 * 1000;
const RUNS_KEPT = 20;

export interface FetcherRow {
  id: string;
  owner: string;
  course: string;
  kind: 'canvas' | 'web';
  name: string;
  url: string;
  canvas_course?: string | null;
  include?: string | null;
  import_pages: boolean;
  /** Also import LaTeX sources (.tex) the source lists. */
  import_tex?: boolean;
  enabled: boolean;
  every_hours: number;
  mapping?: string | null;
  status: 'idle' | 'running' | 'ok' | 'partial' | 'failed';
  has_token: boolean;
  imported_count: number;
}

/** What decides what a source reads and imports. */
export type SourceSettings = Pick<FetcherRow, 'kind' | 'url' | 'canvas_course' | 'include' | 'import_pages' | 'import_tex' | 'mapping'>;

/** Checks and normalizes settings as typed by the student. */
export function normalizeSettings(raw: Partial<Record<keyof SourceSettings, unknown>>): SourceSettings {
  const import_tex = raw.import_tex === true;
  const kind = raw.kind === 'canvas' ? 'canvas' : raw.kind === 'web' ? 'web' : null;
  if (!kind) throw new BadRequest('Choose bCourses or a web page.');
  const include = typeof raw.include === 'string' && raw.include.trim() ? raw.include.trim().slice(0, 300) : null;
  const mapping = typeof raw.mapping === 'string' && raw.mapping.trim() ? raw.mapping.trim() : null;
  if (mapping && mapping.length > 100_000) throw new BadRequest('The mapping is too long.');
  const import_pages = raw.import_pages === true;
  if (kind === 'canvas') {
    return { kind, url: canvasBase(typeof raw.url === 'string' ? raw.url : null), canvas_course: canvasCourseId(raw.canvas_course), include, import_pages, import_tex, mapping };
  }
  if (typeof raw.url !== 'string' || !raw.url.trim()) throw new BadRequest('Paste the address of the course page.');
  const typed = raw.url.trim();
  return { kind, url: publicUrl(/^https?:\/\//i.test(typed) ? typed : `https://${typed}`, 'The page address').href, canvas_course: null, include, import_pages, import_tex, mapping };
}

/** The compiled mapping of a source: its own, or the template for its settings. */
export function sourcePlan(s: SourceSettings): Plan {
  return compileMapping(s.mapping ?? templateMapping({ system: s.kind, include: s.include, importPages: s.import_pages, importTex: s.import_tex }));
}

export async function getFetcher(env: Env, id: string): Promise<FetcherRow | null> {
  return Surreal.system(env).query<FetcherRow | null>('SELECT * FROM ONLY type::record($id)', { id: recordId('fetcher', id) });
}

/** The fetcher, if `userId` owns it. */
export async function getOwnedFetcher(env: Env, userId: string, id: string): Promise<FetcherRow> {
  const row = await Surreal.system(env).query<FetcherRow | null>(
    'RETURN (SELECT * FROM type::record($id) WHERE owner = type::record($owner))[0]',
    { id: recordId('fetcher', id), owner: userId },
  );
  if (!row) throw new NotFound('Source not found.');
  return row;
}

// ── Tokens ────────────────────────────────────────────────────────────────

const secretId = (fetcherId: string) => `fetcher_secret:${fetcherId.slice(fetcherId.indexOf(':') + 1)}`;

export async function saveToken(env: Env, fetcherId: string, token: string): Promise<void> {
  await ensureServerSchema(env);
  await Surreal.system(env).query(
    `UPSERT type::record($id) MERGE { fetcher: type::record($fetcher), token: $sealed };
     UPDATE type::record($fetcher) SET has_token = true;`,
    { id: secretId(fetcherId), fetcher: fetcherId, sealed: await sealToken(env, fetcherId, token.trim()) },
  );
}

export async function loadToken(env: Env, fetcherId: string): Promise<string | null> {
  const sealed = await Surreal.system(env).query<string | null>('RETURN (SELECT VALUE token FROM ONLY type::record($id))', { id: secretId(fetcherId) });
  return sealed ? openToken(env, fetcherId, sealed) : null;
}

// ── Reading ───────────────────────────────────────────────────────────────

function canvasClient(s: SourceSettings, token: string | null): CanvasClient {
  if (!token) throw new BadRequest('This bCourses source has no access token. Paste one in its settings.');
  return new CanvasClient(s.url, token);
}

/** The source as a hockeymask connector over its `items`. */
export function sourceConnector(s: SourceSettings, token: string | null): Connector {
  if (s.kind === 'canvas') return canvasClient(s, token).connector(s.canvas_course ?? '', { pages: s.import_pages });
  return rowsConnector('web', async () => {
    const page = await fetchPage(s.url);
    const version = page.markdown ? `sha256:${(await sha256Hex(page.markdown)).slice(0, 32)}` : null;
    return pageRows(page, version);
  });
}

export async function readItems(connector: Connector): Promise<SourceRow[]> {
  const rows: SourceRow[] = [];
  for await (const batch of connector.read('items')) rows.push(...(batch as SourceRow[]));
  return rows;
}

// ── Planning ──────────────────────────────────────────────────────────────

/** new: never imported. changed: imported, the original changed since. imported: up to date. dismissed: the student deleted it. */
export type ItemStatus = 'new' | 'changed' | 'imported' | 'dismissed';
export interface PlannedItem extends ImportRow {
  status: ItemStatus;
  /** The material it became, for `changed` and `imported`. */
  material: string | null;
}

interface LedgerRow { external_id: string; material?: string | null; version?: string | null }

const ledgerId = async (fetcherId: string, externalId: string) =>
  `fetched_item:f${(await sha256Hex(`${fetcherId}\n${externalId}`)).slice(0, 40)}`;

async function ledger(env: Env, fetcherId: string | null): Promise<Map<string, LedgerRow>> {
  if (!fetcherId) return new Map();
  await ensureServerSchema(env);
  const rows = await Surreal.system(env).query<LedgerRow[]>(
    'SELECT external_id, material, version FROM fetched_item WHERE fetcher = type::record($f)',
    { f: fetcherId },
  );
  return new Map((rows ?? []).map((r) => [r.external_id, r]));
}

/**
 * What the mapping's rows mean for this source now. Linked web files carry
 * no version, so imported ones are checked with a HEAD request (`heads`).
 */
export async function planImports(env: Env, fetcherId: string | null, imports: ImportRow[], opts: { heads: boolean }): Promise<PlannedItem[]> {
  const known = await ledger(env, fetcherId);
  let heads = 0;
  const out: PlannedItem[] = [];
  for (const item of imports) {
    const l = known.get(item.external_id);
    if (!l) {
      out.push({ ...item, status: 'new', material: null });
      continue;
    }
    if (!l.material) {
      out.push({ ...item, status: 'dismissed', material: null });
      continue;
    }
    let version = item.version;
    if (!version && l.version && opts.heads && item.kind !== 'markdown' && heads < MAX_HEADS) {
      heads++;
      version = await linkVersion(item.url);
    }
    const changed = !!version && !!l.version && version !== l.version;
    out.push({ ...item, version: version ?? l.version ?? null, status: changed ? 'changed' : 'imported', material: l.material });
  }
  return out;
}

/** A dry run: what the source lists and what importing it would do. */
export async function previewSource(env: Env, s: SourceSettings, token: string | null, fetcherId: string | null) {
  const plan = sourcePlan(s);
  const rows = await readItems(sourceConnector(s, token));
  const { imports, issues } = await runMapping(plan, rows);
  const items = await planImports(env, fetcherId, imports, { heads: false });
  return {
    listed: rows.length,
    items: items.slice(0, 200).map((i) => ({ id: i.external_id, title: i.title, kind: i.kind, link: i.link ?? i.url, status: i.status })),
    total: items.length,
    issues,
  };
}

// ── Importing ─────────────────────────────────────────────────────────────

const MAX_TEX_BYTES = 2 * 1024 * 1024;

/** The original's response, and a version for it when the item has none. */
async function fetchOriginal(f: FetcherRow, item: ImportRow, token: string | null, accept: string): Promise<{ res: Response; version: string | null }> {
  const res = f.kind === 'canvas'
    ? await canvasClient(f, token).download(item.url)
    : (await safeFetch(item.url, { accept })).res;
  if (!res.ok) {
    await res.body?.cancel();
    throw new BadRequest(`Download failed (${res.status}).`);
  }
  const etag = res.headers.get('etag');
  const modified = res.headers.get('last-modified');
  const version = item.version ?? (etag ? `etag:${etag.replace(/^W\//, '')}` : modified ? `modified:${modified}|${res.headers.get('content-length') ?? ''}` : null);
  return { res, version };
}

async function download(f: FetcherRow, item: ImportRow, token: string | null): Promise<{ bytes: Uint8Array; name: string; version: string | null }> {
  const { res, version } = await fetchOriginal(f, item, token, 'application/pdf,*/*');
  const bytes = await readLimited(res.body, MAX_PDF_BYTES);
  validatePdf(bytes);
  const name = `${item.title.replace(/[\\/:*?"<>|]+/g, ' ').trim() || 'lecture'}.pdf`;
  return { bytes, name, version };
}

/** The text of a page (as Markdown) or of a LaTeX file (as written). */
async function textOf(f: FetcherRow, item: ImportRow, token: string | null): Promise<{ content: string; version: string | null }> {
  if (item.kind === 'tex') {
    const { res, version } = await fetchOriginal(f, item, token, 'text/x-tex,text/plain,*/*');
    if (/html/i.test(res.headers.get('content-type') ?? '')) {
      await res.body?.cancel();
      throw new BadRequest('The file is a web page, not LaTeX source.');
    }
    const content = new TextDecoder().decode(await readLimited(res.body, MAX_TEX_BYTES));
    if (content.includes('\u0000') || !content.trim()) throw new BadRequest('The file is not LaTeX source.');
    return { content, version };
  }
  const markdown = f.kind === 'canvas' ? (await canvasClient(f, token).page(item.url)).markdown : (await fetchPage(item.url)).markdown;
  if (!markdown.trim()) throw new BadRequest('The page has no text to import.');
  return { content: markdown, version: item.version };
}

/** CREATE as the API, tolerating a sync hook that fails after the row committed (see material-create.ts). */
async function createMaterialRow(env: Env, row: Record<string, unknown> & { id: string }): Promise<void> {
  const db = Surreal.system(env);
  const { id, ...content } = row;
  try {
    await db.query(
      `CREATE type::record($id) CONTENT {
         owner: type::record($owner), course: type::record($course), title: $title, kind: $kind,
         content: $content, status: 'pending', fetcher: type::record($fetcher), source_url: $source_url
       } RETURN NONE`,
      { id, ...content },
    );
  } catch (err) {
    for (const delay of [0, 300, 1000]) {
      if (delay) await new Promise((r) => setTimeout(r, delay));
      if (await db.query<string | null>('RETURN (SELECT VALUE id FROM ONLY type::record($id))', { id }).catch(() => null)) return;
    }
    throw err;
  }
}

/**
 * Imports one planned item: a new material, or the new version of the one it
 * became. Safe to repeat: a retried step finds its material in the ledger.
 */
export async function importItem(env: Env, fetcherId: string, item: PlannedItem): Promise<'created' | 'updated' | 'skipped'> {
  const f = await getFetcher(env, fetcherId);
  if (!f) return 'skipped';
  const token = f.kind === 'canvas' ? await loadToken(env, f.id) : null;
  const db = Surreal.system(env);
  const key = await ledgerId(f.id, item.external_id);
  const entry = await db.query<{ material?: string | null; version?: string | null } | null>('SELECT material, version FROM ONLY type::record($id)', { id: key });
  const record = (material: string, version: string | null) =>
    db.query(
      `UPSERT type::record($id) MERGE {
         owner: type::record($owner), fetcher: type::record($fetcher), external_id: $external_id,
         material: type::record($material), version: $version
       }`,
      { id: key, owner: f.owner, fetcher: f.id, external_id: item.external_id, material, version },
    );

  if (item.status === 'new') {
    if (entry && !entry.material) return 'skipped'; // deleted by the student meanwhile
    // The ledger names the material before it exists, so a retried step
    // finishes this one instead of creating another.
    const id = entry?.material ?? `material:k${crypto.randomUUID().replaceAll('-', '')}`;
    if (entry?.material && (await db.query<string | null>('RETURN (SELECT VALUE id FROM ONLY type::record($id))', { id }))) return 'created';
    const base = { id, owner: f.owner, course: f.course, title: item.title, kind: item.kind, fetcher: f.id, source_url: item.link ?? item.url };
    if (item.kind === 'pdf') {
      const file = await download(f, item, token);
      await record(id, file.version);
      await createMaterialRow(env, { ...base, content: '' });
      await storePdfAndIngest(env, { id, owner: f.owner }, file.bytes, file.name);
    } else {
      const text = await textOf(f, item, token);
      await record(id, text.version);
      await createMaterialRow(env, { ...base, content: text.content });
      await startIngest(env, id);
    }
    return 'created';
  }

  if (item.status !== 'changed' || !entry?.material) return 'skipped';
  if (entry.version === item.version) return 'updated'; // a retried step
  const material = await db.query<{ id: string; kind: string } | null>('SELECT id, kind FROM ONLY type::record($id)', { id: entry.material });
  if (!material) return 'skipped';
  if (material.kind !== item.kind) throw new BadRequest('The original changed its type. Delete the material to import it again.');
  if (item.kind === 'pdf') {
    const file = await download(f, item, token);
    await storePdfAndIngest(env, { id: material.id, owner: f.owner }, file.bytes, file.name, { replace: true });
    await record(material.id, file.version);
  } else {
    const text = await textOf(f, item, token);
    await db.query('UPDATE type::record($id) SET content = $content', { id: material.id, content: text.content });
    await record(material.id, text.version);
    await startIngest(env, material.id);
  }
  return 'updated';
}

// ── Runs ──────────────────────────────────────────────────────────────────

export interface RunPlan {
  items: PlannedItem[];
  found: number;
  deferred: number;
  issues: string[];
}

/** Reads the source and decides what this run imports. */
export async function planRun(env: Env, fetcherId: string): Promise<RunPlan | null> {
  const f = await getFetcher(env, fetcherId);
  if (!f) return null;
  const token = f.kind === 'canvas' ? await loadToken(env, f.id) : null;
  const rows = await readItems(sourceConnector(f, token));
  const { imports, issues } = await runMapping(sourcePlan(f), rows);
  const planned = await planImports(env, f.id, imports, { heads: true });
  const todo = planned.filter((i) => i.status === 'new' || i.status === 'changed');
  return { items: todo.slice(0, MAX_ITEMS_PER_RUN), found: imports.length, deferred: Math.max(0, todo.length - MAX_ITEMS_PER_RUN), issues };
}

/**
 * Starts a run unless one is going. A scheduled run (`manual` false) skips a
 * disabled source.
 */
export async function startFetch(env: Env, fetcherId: string, opts: { manual: boolean }): Promise<{ started: boolean; run?: string }> {
  await ensureServerSchema(env);
  const db = Surreal.system(env);
  const claimed = await db.query<{ id: string; owner: string }[]>(
    `UPDATE type::record($id) SET status = 'running', run_started_at = time::now(), last_error = NONE
       WHERE ($manual OR enabled = true)
         AND (status != 'running' OR run_started_at = NONE OR run_started_at < <datetime> $stale)
       RETURN id, owner`,
    { id: recordId('fetcher', fetcherId), manual: opts.manual, stale: new Date(Date.now() - STALE_RUN_MS).toISOString() },
  );
  const f = claimed?.[0];
  if (!f) return { started: false };
  const runId = `fetch_run:k${crypto.randomUUID().replaceAll('-', '')}`;
  await db.query(`CREATE type::record($id) CONTENT { owner: type::record($owner), fetcher: type::record($fetcher), status: 'running' }`, {
    id: runId, owner: f.owner, fetcher: f.id,
  });
  try {
    await env.FETCH.create({ id: runId.slice(runId.indexOf(':') + 1), params: { fetcherId: f.id, runId } });
  } catch (err) {
    console.error('[fetch] could not start', f.id, err);
    await finishRun(env, f.id, runId, { status: 'failed', found: 0, created: 0, updated: 0, failed: 0, deferred: 0, error: 'The check could not start. It is tried again later.' });
    throw err;
  }
  return { started: true, run: runId };
}

export interface RunOutcome {
  status: 'ok' | 'partial' | 'failed';
  found: number;
  created: number;
  updated: number;
  failed: number;
  deferred: number;
  /** Problems worth showing, one per line. */
  detail?: string[];
  /** The headline problem, shown on the source. */
  error?: string;
  /** Stop scheduled checks (the token was refused) until the student fixes it. */
  disable?: boolean;
}

export async function finishRun(env: Env, fetcherId: string, runId: string, o: RunOutcome): Promise<void> {
  const f = await getFetcher(env, fetcherId);
  const hours = f?.every_hours ?? 6;
  const next = new Date(Date.now() + (o.deferred > 0 && o.status !== 'failed' ? DEFERRED_RETRY_MS : hours * 3600_000));
  const detail = o.detail?.length ? o.detail.slice(0, 30).join('\n').slice(0, 4000) : null;
  await Surreal.system(env).query(
    `UPDATE type::record($run) SET status = $status, found = $found, created = $created, updated = $updated,
       failed = $failed, deferred = $deferred, detail = $detail, finished_at = time::now();
     UPDATE type::record($fetcher) SET status = $status, last_error = $error, last_run_at = time::now(),
       run_started_at = NONE, next_run_at = <datetime> $next, imported_count += $created,
       enabled = enabled AND !$disable;
     LET $keep = SELECT VALUE id FROM fetch_run WHERE fetcher = type::record($fetcher) ORDER BY started_at DESC LIMIT ${RUNS_KEPT};
     DELETE fetch_run WHERE fetcher = type::record($fetcher) AND id NOTINSIDE $keep;`,
    {
      run: runId, fetcher: fetcherId, status: o.status, found: o.found, created: o.created, updated: o.updated,
      failed: o.failed, deferred: o.deferred, detail, error: o.error ?? (o.status === 'ok' ? null : o.detail?.[0] ?? null),
      next: next.toISOString(), disable: o.disable === true,
    },
  );
}

/** Sources due for their scheduled check (the `fetchers-tick` schedule). */
export async function startDueFetches(env: Env, limit = 50): Promise<number> {
  await ensureServerSchema(env);
  const due = await Surreal.system(env).query<string[]>(
    `SELECT VALUE id FROM fetcher
       WHERE enabled = true AND (next_run_at = NONE OR next_run_at <= time::now())
         AND (status != 'running' OR run_started_at = NONE OR run_started_at < <datetime> $stale)
       ORDER BY next_run_at LIMIT ${limit}`,
    { stale: new Date(Date.now() - STALE_RUN_MS).toISOString() },
  );
  let started = 0;
  for (const id of due ?? []) {
    try {
      if ((await startFetch(env, id, { manual: false })).started) started++;
    } catch (err) {
      console.error('[fetch] scheduled run', id, err);
    }
  }
  return started;
}

/** A user-facing message for anything a run hit. */
export const runError = (err: unknown) =>
  err instanceof UserError ? err.message : 'The source could not be read. It is tried again at the next check.';
