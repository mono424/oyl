import { Hono } from 'hono';
import type { AppEnv, Env } from '../env';
import { getOwnedCourse } from '../service';
import { BadRequest, Surreal, recordId } from '../surreal';
import { CanvasClient, canvasBase } from './canvas';
import { templateMapping } from './mapping';
import {
  getOwnedFetcher,
  loadToken,
  normalizeSettings,
  previewSource,
  saveToken,
  type SourceSettings,
  sourcePlan,
  startFetch,
} from './run';

/**
 * Sources for the web app (`/api/fetchers/*`, exposed in sp00ky.yml). The
 * student's session token decides what they may touch. Deleting a source and
 * switching it on or off are plain writes from the app; everything that needs
 * a check first (a token, a URL, a mapping) comes through here.
 */
export const fetcherRoutes = new Hono<AppEnv>();

type Body = Record<string, unknown>;

const str = (v: unknown) => (typeof v === 'string' ? v : undefined);
const token = (v: unknown) => (typeof v === 'string' && v.trim() ? v.trim() : null);

function everyHours(v: unknown, fallback = 6): number {
  const n = typeof v === 'number' ? Math.round(v) : Number.parseInt(String(v ?? ''), 10);
  return Number.isFinite(n) ? Math.min(168, Math.max(1, n)) : fallback;
}

/** A readable default name: the Canvas course, or the page's host and path. */
async function defaultName(s: SourceSettings, canvas: CanvasClient | null): Promise<string> {
  if (canvas && s.canvas_course) {
    const c = await canvas.course(s.canvas_course);
    return `bCourses: ${c.code ?? c.name}`.slice(0, 120);
  }
  const u = new URL(s.url);
  return `${u.host}${u.pathname.replace(/\/(index\.html?)?$/, '')}`.slice(0, 120);
}

/** Checks that a Canvas token works for the course before it is saved. */
async function checkCanvas(s: SourceSettings, t: string | null): Promise<CanvasClient> {
  if (!t) throw new BadRequest('Paste a bCourses access token.');
  const client = new CanvasClient(s.url, t);
  await client.course(s.canvas_course ?? '');
  return client;
}

// The courses a Canvas token can see, for the course picker.
fetcherRoutes.post('/canvas/courses', async (c) => {
  const body = await c.req.json<Body>();
  const client = new CanvasClient(canvasBase(str(body.url)), token(body.token) ?? '');
  return c.json({ courses: await client.courses() });
});

// What a source would import, without saving anything. An existing source
// (`fetcher`) uses its saved token unless a new one is given.
fetcherRoutes.post('/preview', async (c) => {
  const { userId } = c.var.caller;
  const body = await c.req.json<Body>();
  const existing = typeof body.fetcher === 'string' ? await getOwnedFetcher(c.env, userId, body.fetcher) : null;
  const s = normalizeSettings(existing ? { ...existing, ...pickSettings(body) } : body);
  const t = token(body.token) ?? (existing && s.kind === 'canvas' ? await loadToken(c.env, existing.id) : null);
  return c.json(await previewSource(c.env, s, t, existing?.id ?? null));
});

const pickSettings = (b: Body) =>
  Object.fromEntries((['include', 'import_pages', 'import_tex', 'mapping'] as const).filter((k) => k in b).map((k) => [k, b[k]]));

/**
 * Adds a source to a course and starts its first check. `user` is the
 * student's own session, so the course permission applies to the CREATE.
 */
export async function createSource(env: Env, user: Surreal, userId: string, body: Body): Promise<{ id: string; name: string }> {
  const course = await getOwnedCourse(env, userId, String(body.course ?? ''));
  const s = normalizeSettings(body);
  if (s.mapping) sourcePlan(s);
  const t = s.kind === 'canvas' ? token(body.token) : null;
  const canvas = s.kind === 'canvas' ? await checkCanvas(s, t) : null;
  const name = str(body.name)?.trim().slice(0, 120) || (await defaultName(s, canvas));
  const id = `fetcher:k${crypto.randomUUID().replaceAll('-', '')}`;
  const rows = await user.query<unknown[]>(
    `CREATE type::record($id) CONTENT {
       owner: $auth.id, course: type::record($course), kind: $kind, name: $name, url: $url,
       canvas_course: $canvas_course, include: $include, import_pages: $import_pages, import_tex: $import_tex,
       every_hours: $every_hours, enabled: true
     } RETURN id`,
    { id, course: course.id, name, ...s, mapping: undefined, every_hours: everyHours(body.every_hours) },
  );
  if (!rows?.length) throw new BadRequest('The source could not be added to this course.');
  if (s.mapping) await Surreal.system(env).query('UPDATE type::record($id) SET mapping = $mapping', { id, mapping: s.mapping });
  if (t) await saveToken(env, id, t);
  await startFetch(env, id, { manual: true });
  return { id, name };
}

fetcherRoutes.post('/create', async (c) => {
  const body = await c.req.json<Body>();
  return c.json(await createSource(c.env, Surreal.as(c.env, c.var.caller.token), c.var.caller.userId, body));
});

// Changes a source. A new token re-enables a source its old token stopped.
fetcherRoutes.post('/:id', async (c) => {
  const { userId } = c.var.caller;
  const f = await getOwnedFetcher(c.env, userId, c.req.param('id'));
  const body = await c.req.json<Body>();
  const s = normalizeSettings({ ...f, ...pickSettings(body) });
  if ('mapping' in body) sourcePlan(s);
  const t = token(body.token);
  if (t && f.kind === 'canvas') {
    await checkCanvas(s, t);
    await saveToken(c.env, f.id, t);
  }
  const patch: Record<string, unknown> = {};
  if (typeof body.name === 'string' && body.name.trim()) patch.name = body.name.trim().slice(0, 120);
  if ('include' in body) patch.include = s.include;
  if ('import_pages' in body) patch.import_pages = s.import_pages;
  if ('import_tex' in body) patch.import_tex = s.import_tex;
  if ('every_hours' in body) patch.every_hours = everyHours(body.every_hours, f.every_hours);
  if (typeof body.enabled === 'boolean') patch.enabled = body.enabled;
  if (t) patch.enabled = true;
  const clear = 'mapping' in body && !s.mapping;
  if ('mapping' in body && s.mapping) patch.mapping = s.mapping;
  await updateFetcher(c.env, f.id, patch, clear);
  return c.json({ ok: true });
});

async function updateFetcher(env: Env, id: string, patch: Record<string, unknown>, clearMapping: boolean) {
  const clears = [
    ...('include' in patch && patch.include === null ? ['include'] : []),
    ...(clearMapping ? ['mapping'] : []),
  ];
  const set = Object.fromEntries(Object.entries(patch).filter(([, v]) => v !== null));
  await Surreal.system(env).query(
    `UPDATE type::record($id) MERGE $set${clears.length ? `; UPDATE type::record($id) SET ${clears.map((k) => `${k} = NONE`).join(', ')}` : ''}`,
    { id: recordId('fetcher', id), set },
  );
}

// The mapping a source runs, for the advanced editor: its own, or the template.
fetcherRoutes.get('/:id/mapping', async (c) => {
  const f = await getOwnedFetcher(c.env, c.var.caller.userId, c.req.param('id'));
  const doc = f.mapping ? JSON.parse(f.mapping) : templateMapping({ system: f.kind, include: f.include, importPages: f.import_pages, importTex: f.import_tex });
  return c.json({ custom: !!f.mapping, mapping: doc });
});
