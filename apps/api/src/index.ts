import { Hono } from 'hono';
import { cors } from 'hono/cors';
import type { AppEnv } from './env';
import { requireUser } from './auth';
import { handleMcp } from './mcp';
import { approveOAuthConsent, getOAuthConsent, handleOAuth, oauthOrigin } from './oauth';
import { jobs } from './jobs';
import { fetcherRoutes } from './fetch/routes';
import { loadAnswerImage, storeAnswerImage } from './answers';
import { readVersion, restoreVersion } from './documents';
import { finishSite, serveSiteFile, storeSiteFile } from './sites';
import {
  buildMissingGraphs,
  createApiKey,
  ensureServerSchema,
  getOwnedCourse,
  getOwnedMaterial,
  markFailed,
  searchCourse,
  storePdfAndIngest,
  readLimited,
  MAX_PDF_BYTES,
} from './service';
import { pdfPartCount, pdfParts } from './pdf';
import { courseGraph } from './understanding';
import { BadRequest, Conflict, NotFound, Surreal, UserError, fromBase64, recordId } from './surreal';

import { withWorkflows } from '@spooky-sync/backend';
import { IngestMaterial } from './ingest';
import { FetchSource } from './fetch/workflow';

const app = new Hono<AppEnv>();

app.onError((err, c) => {
  if (err instanceof UserError) return c.json({ error: err.message }, err.status);
  // Anything else may carry database internals: log it, answer generically.
  console.error(err);
  return c.json({ error: 'Something went wrong. Try again in a moment.' }, 500);
});

// OAuth discovery, registration and token exchange are public protocol routes.
// Account approval below still requires the signed-in student's session.
app.use('*', async (c, next) => {
  const response = await handleOAuth(c.req.raw, c.env);
  if (response) return response;
  await next();
});

// The web app calls these from the browser.
app.use(
  '/api/*',
  cors({
    origin: (origin, c) => (c.env.APP_ORIGIN.split(',').map((o: string) => o.trim()).includes(origin) ? origin : null),
    allowHeaders: ['authorization', 'content-type', 'x-file-name'],
    allowMethods: ['GET', 'POST', 'OPTIONS'],
    maxAge: 600,
  }),
);
app.use('/api/*', requireUser);

app.get('/health', (c) => c.json({ ok: true }));

app.post('/api/oauth/request', async (c) => {
  const { requestId } = await c.req.json<{ requestId?: string }>();
  if (!requestId) throw new BadRequest('The connection request is missing. Start again from your assistant.');
  return c.json(await getOAuthConsent(c.env, c.var.caller.userId, requestId));
});

app.post('/api/oauth/approve', async (c) => {
  const body = await c.req.json<{ requestId?: string; allow?: boolean }>();
  if (!body.requestId || typeof body.allow !== 'boolean') throw new BadRequest('Choose whether to allow this connection.');
  return c.json(await approveOAuthConsent(
    c.env, c.var.caller.userId, { requestId: body.requestId, allow: body.allow }, oauthOrigin(c.req.raw, c.env),
  ));
});

// ── Materials ─────────────────────────────────────────────────────────────

app.post('/api/materials/:id/pdf', async (c) => {
  const { userId } = c.var.caller;
  const material = await getOwnedMaterial(c.env, userId, c.req.param('id'));
  const bytes = await readLimited(c.req.raw.body, MAX_PDF_BYTES);
  const name = decodeURIComponent(c.req.header('x-file-name') ?? '') || null;
  try {
    await storePdfAndIngest(c.env, material, bytes, name);
  } catch (err) {
    // A file refused before processing started (not a PDF, too big) still
    // shows up on the material, so the row does not wait forever. A refused
    // second upload leaves the material and its PDF as they are.
    if (err instanceof UserError && !(err instanceof Conflict)) await markFailed(c.env, material.id, err.message);
    throw err;
  }
  return c.json({ ok: true });
});

// The original PDF, for the viewer. Streamed part by part from the bucket.
app.get('/api/materials/:id/pdf', async (c) => {
  const id = recordId('material', c.req.param('id'));
  await ensureServerSchema(c.env);
  const m = await Surreal.system(c.env).query<{ kind: string; source_name?: string | null } | null>(
    'RETURN (SELECT kind, source_name FROM type::record($id) WHERE owner = type::record($owner))[0]',
    { id, owner: c.var.caller.userId },
  );
  const count = m?.kind === 'pdf' ? await pdfPartCount(c.env, id) : 0;
  if (!count) throw new NotFound('This material has no PDF.');
  const parts = pdfParts(c.env, id, count);
  // The first part is read before answering, so a missing file is a JSON error.
  const first = await parts.next();
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      if (!first.done) controller.enqueue(first.value);
    },
    async pull(controller) {
      const next = await parts.next();
      if (next.done) controller.close();
      else controller.enqueue(next.value);
    },
    async cancel() {
      await parts.return(undefined);
    },
  });
  const name = m?.source_name || 'lecture.pdf';
  return new Response(body, {
    headers: {
      'content-type': 'application/pdf',
      'content-disposition': `inline; filename*=UTF-8''${encodeURIComponent(name)}`,
      // A material's PDF changes only when its source replaces it, and the
      // app then asks for it with the new size (`?v=`, lib/api.ts fetchPdf).
      'cache-control': 'private, max-age=86400',
    },
  });
});

app.post('/api/courses/:id/search', async (c) => {
  const { userId } = c.var.caller;
  const course = await getOwnedCourse(c.env, userId, c.req.param('id'));
  const { query } = await c.req.json<{ query?: string }>();
  const hits = await searchCourse(c.env, course.id, query ?? '', { k: 8, ownerId: userId });
  return c.json({ hits });
});

// What the student understood, concept by concept, for the graph view.
// Lectures without a knowledge graph yet start building one.
app.get('/api/courses/:id/graph', async (c) => {
  const graph = await courseGraph(c.env, c.var.caller.userId, c.req.param('id'));
  c.executionCtx.waitUntil(
    buildMissingGraphs(c.env, c.var.caller.userId, graph.course).catch((err) => console.error('[memory] graph builds', err)),
  );
  return c.json(graph);
});

// ── Homework (answers.ts) ─────────────────────────────────────────────────

// An image pasted into one of the user's answers; the body is the image.
app.post('/api/answers/:id/images', async (c) => {
  const id = await storeAnswerImage(c.env, c.var.caller.userId, 'answer', c.req.param('id'), c.req.raw.body);
  return c.json({ id });
});

// The same for an image pasted into one of the user's documents.
app.post('/api/documents/:id/images', async (c) => {
  const id = await storeAnswerImage(c.env, c.var.caller.userId, 'document', c.req.param('id'), c.req.raw.body);
  return c.json({ id });
});

// A version of one of the user's documents, with the text before it, to preview and compare.
app.get('/api/documents/:id/versions/:n', async (c) => {
  return c.json(await readVersion(Surreal.as(c.env, c.var.caller.token), c.req.param('id'), Number(c.req.param('n'))));
});

// A document's text as it stands now, for the editor to merge with after a save was refused.
app.get('/api/documents/:id/text', async (c) => {
  const id = recordId('document', c.req.param('id'));
  const content = await Surreal.as(c.env, c.var.caller.token).query<string>('RETURN (SELECT VALUE content FROM ONLY type::record($id));', { id });
  return c.json({ content });
});

// Goes back to a version, as a new version.
app.post('/api/documents/:id/versions/:n/restore', async (c) => {
  const { userId, token } = c.var.caller;
  return c.json(await restoreVersion(Surreal.as(c.env, token), Surreal.system(c.env), userId, 'student', c.req.param('id'), Number(c.req.param('n'))));
});

// Public by unguessable id, like lecture images: <img> cannot send the token.
app.get('/answer-images/:id', async (c) => {
  const row = await loadAnswerImage(c.env, c.req.param('id'));
  if (!row) return c.notFound();
  return new Response(fromBase64(row.b64), {
    headers: { 'content-type': row.mime, 'cache-control': 'private, max-age=86400', 'access-control-allow-origin': '*' },
  });
});

// ── Sites (sites.ts) ──────────────────────────────────────────────────────

// One file of a site being uploaded; the body is the file.
app.post('/api/sites/:id/files', async (c) => {
  await storeSiteFile(c.env, c.var.caller.userId, c.req.param('id'), c.req.query('path') ?? '', c.req.raw.body);
  return c.json({ ok: true });
});

// Every file is up: the site opens.
app.post('/api/sites/:id/finish', async (c) => {
  return c.json(await finishSite(c.env, c.var.caller.userId, c.req.param('id')));
});

// Public by unguessable id, like lecture images: a page cannot send the token.
app.get('/site-files/:site/*', (c) => serveSiteFile(c.env, c.req.param('site'), new URL(c.req.url).pathname, c.req.header('range')));

// ── Sources (fetch/routes.ts) ─────────────────────────────────────────────

app.route('/api/fetchers', fetcherRoutes);

// ── API keys (for the MCP server) ─────────────────────────────────────────

app.post('/api/keys', async (c) => {
  const { name } = await c.req.json<{ name?: string }>();
  return c.json(await createApiKey(c.env, c.var.caller.userId, name ?? ''));
});

// ── Images extracted from PDFs ────────────────────────────────────────────

/**
 * Public by unguessable id (128 random bits), like a capability URL: <img>
 * tags cannot send the session token, and the ids only appear inside
 * material the owner can read.
 */
// Keep the old endpoint for existing links. The new path avoids colliding
// with Vite's /assets directory when the API shares the app's origin.
app.on('GET', ['/assets/:id', '/material-assets/:id'], async (c) => {
  const id = recordId('asset', c.req.param('id'));
  await ensureServerSchema(c.env);
  const row = await Surreal.system(c.env).query<{ b64: string; mime: string } | null>(
    `LET $row = SELECT mime, data FROM ONLY type::record($id);
     LET $png = type::file('materials', '/assets/' + <string>record::id(type::record($id))).get() ?? $row.data;
     RETURN IF $row AND $png { { mime: $row.mime, b64: encoding::base64::encode($png) } } ELSE { NONE };`,
    { id },
  );
  if (!row) return c.notFound();
  return new Response(fromBase64(row.b64), {
    headers: {
      'content-type': row.mime,
      // An id never changes content; a day bounds how long a deleted image lingers.
      'cache-control': 'public, max-age=86400',
      'access-control-allow-origin': '*',
    },
  });
});

// ── MCP ───────────────────────────────────────────────────────────────────

app.all('/mcp', (c) => handleMcp(c.req.raw, c.env, c.executionCtx));

// ── Jobs ──────────────────────────────────────────────────────────────────

// Indexing and lecture progress run as sp00ky jobs (jobs.ts).
app.route('/jobs', jobs);

// Material ingestion and source runs are durable workflows on sp00ky's job
// engine (the `job` outbox): the same code on Cloudflare, a VM and `spky dev`.
export default withWorkflows(app, { INGEST: IngestMaterial, FETCH: FetchSource });
