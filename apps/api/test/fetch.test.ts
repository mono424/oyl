import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { afterEach, test } from 'node:test';
import type { Env } from '../src/env.ts';
import { CanvasAuthError, CanvasClient, canvasBase, canvasCourseId } from '../src/fetch/canvas.ts';
import { htmlToMarkdown, parseHtml } from '../src/fetch/html.ts';
import { compileMapping, runMapping, type SourceRow, templateMapping } from '../src/fetch/mapping.ts';
import { publicUrl, safeFetch } from '../src/fetch/net.ts';
import { createSource } from '../src/fetch/routes.ts';
import { finishRun, getFetcher, importItem, loadToken, normalizeSettings, planRun, saveToken, startDueFetches, startFetch } from '../src/fetch/run.ts';
import { openToken, sealToken } from '../src/fetch/secret.ts';
import { pageRows } from '../src/fetch/web.ts';
import { Surreal } from '../src/surreal.ts';

const realFetch = globalThis.fetch;
afterEach(() => {
  globalThis.fetch = realFetch;
});

/** Answers requests to hosts in `routes`; everything else (the test database) goes out. */
function serve(routes: Record<string, (req: Request) => Response | Promise<Response>>, seen: Request[] = []) {
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const req = new Request(input, init);
    const host = new URL(req.url).host;
    if (!(host in routes)) return realFetch(input, init);
    seen.push(req);
    return routes[host](req);
  }) as typeof fetch;
  return seen;
}

const row = (o: Partial<SourceRow>): SourceRow => ({
  id: 'x', name: 'x', kind: 'file', content_type: null, ext: null, url: 'https://a.example/x', link: null,
  updated_at: null, module: null, size: null, ...o,
});

const PDF = (marker: string) => new TextEncoder().encode(`%PDF-1.4\n% ${marker}\n%%EOF\n`);

// ── Pure parts ──────────────────────────────────────────────────────────────

test('only public https addresses by name may be fetched', () => {
  assert.equal(publicUrl('https://bcourses.berkeley.edu/courses/1#x').href, 'https://bcourses.berkeley.edu/courses/1');
  for (const bad of [
    'http://example.edu/', 'https://127.0.0.1/', 'https://0x7f.1/', 'https://[::1]/', 'https://localhost/',
    'https://printer.local/', 'https://intranet/', 'https://a:b@example.edu/', 'https://example.edu:8443/', 'not a url',
  ]) assert.throws(() => publicUrl(bad), undefined, bad);
});

test('redirects are checked hop by hop and never carry the token elsewhere', async () => {
  const seen = serve({
    'canvas.example.edu': () => new Response(null, { status: 302, headers: { location: 'https://files.example.com/f.pdf' } }),
    'files.example.com': () => new Response('ok'),
    'evil.example.edu': () => new Response(null, { status: 302, headers: { location: 'https://127.0.0.1/admin' } }),
  });
  const { res, url } = await safeFetch('https://canvas.example.edu/files/1/download', { authorization: 'Bearer secret' });
  assert.equal(await res.text(), 'ok');
  assert.equal(url.host, 'files.example.com');
  assert.equal(seen[0].headers.get('authorization'), 'Bearer secret');
  assert.equal(seen[1].headers.get('authorization'), null);
  await assert.rejects(safeFetch('https://evil.example.edu/'), /IP address/);
});

test('course pages yield their links, headings and text', () => {
  const page = parseHtml(
    `<html><head><title>CS 61A &amp; co</title><script>"<a href='x.pdf'>"</script></head><body>
     <nav><a href="/">Home</a></nav><main><h2>Week 1</h2><p>Intro &mdash; <a href="slides/lec01.pdf#p2">Lecture 1</a></p>
     <ul><li>One</li><li>Two</li></ul><pre>  def f():\n      return 1</pre><h3>Week 2</h3><a href="lec02.pdf">Slides</a></main>`,
    'https://cs61a.example.edu/fa26/',
  );
  assert.equal(page.title, 'CS 61A & co');
  assert.deepEqual(page.links.map((l) => [l.url, l.text, l.heading]), [
    ['https://cs61a.example.edu/', 'Home', null],
    ['https://cs61a.example.edu/fa26/slides/lec01.pdf', 'Lecture 1', 'Week 1'],
    ['https://cs61a.example.edu/fa26/lec02.pdf', 'Slides', 'Week 2'],
  ]);
  assert.match(page.markdown, /^## Week 1\n\nIntro - \[Lecture 1\]\(https:\/\/cs61a\.example\.edu\/fa26\/slides\/lec01\.pdf\)/);
  assert.match(page.markdown, /- One\n- Two/);
  assert.match(page.markdown, /```\n {2}def f\(\):\n {6}return 1\n```/);
  assert.doesNotMatch(page.markdown, /Home|x\.pdf/);
  assert.equal(htmlToMarkdown('<p>a<br>b</p><img src="t.png" alt="Tree">', 'https://x.example/'), 'a\nb\n\n[Image: Tree]');
});

test('vague link texts are named after their heading', () => {
  const rows = pageRows({
    url: 'https://c.example.edu/',
    title: 'Course',
    markdown: '',
    links: [
      { url: 'https://c.example.edu/w1.pdf', text: 'Slides', heading: 'Week 1: Intro' },
      { url: 'https://c.example.edu/w2.pdf', text: 'Slides', heading: 'Week 2: Recursion' },
      { url: 'https://c.example.edu/syllabus.pdf', text: 'Syllabus', heading: null },
      { url: 'http://old.example.edu/a.pdf', text: 'Old notes', heading: null },
      { url: 'https://10.0.0.1/b.pdf', text: 'Internal', heading: null },
    ],
  }, 'sha256:abc');
  assert.deepEqual(rows.map((r) => [r.kind, r.name, r.ext, r.updated_at]), [
    ['page', 'Course', null, 'sha256:abc'],
    ['link', 'Week 1: Intro: w1', 'pdf', null],
    ['link', 'Week 2: Recursion: w2', 'pdf', null],
    ['link', 'Syllabus', 'pdf', null],
    ['link', 'Old notes', 'pdf', null],
  ]);
  assert.equal(rows[4].url, 'https://old.example.edu/a.pdf');
});

test('the template mapping keeps PDFs (and pages), filtered by name', async () => {
  const rows = [
    row({ id: 'file:1', name: 'Lecture 01.PDF', content_type: 'application/pdf', updated_at: '2026-10-01T00:00:00Z' }),
    row({ id: 'file:2', name: 'syllabus.docx', content_type: 'application/msword', ext: 'docx' }),
    row({ id: 'page:3', name: 'Week 1 notes', kind: 'page', content_type: 'text/html' }),
    row({ id: 'https://a.example/hw1.pdf', name: 'Homework 1', kind: 'link', ext: 'pdf', url: 'https://a.example/hw1.pdf' }),
  ];
  const ids = async (opts: Parameters<typeof templateMapping>[0]) =>
    (await runMapping(compileMapping(templateMapping(opts)), rows)).imports.map((i) => `${i.external_id} ${i.title} ${i.kind}`);
  assert.deepEqual(await ids({ system: 'canvas' }), ['file:1 Lecture 01 pdf', 'https://a.example/hw1.pdf Homework 1 pdf']);
  assert.deepEqual((await ids({ system: 'canvas', importPages: true })).length, 3);
  assert.deepEqual(await ids({ system: 'canvas', include: 'NOTES, lecture', importPages: true }), ['file:1 Lecture 01 pdf', 'page:3 Week 1 notes markdown']);
  // Regex characters in the words are taken literally.
  assert.deepEqual(await ids({ system: 'web', include: 'hw1.pdf' }), ['https://a.example/hw1.pdf Homework 1 pdf']);
  const imports = (await runMapping(compileMapping(templateMapping({ system: 'canvas' })), rows)).imports;
  assert.equal(imports[0].version, '2026-10-01T00:00:00Z');
});

test('LaTeX sources are imported as tex when asked', async () => {
  const rows = [
    row({ id: 'https://a.example/hw-1.pdf', name: 'Homework 1', kind: 'link', ext: 'pdf', url: 'https://a.example/hw-1.pdf' }),
    row({ id: 'https://a.example/hw-1.tex', name: 'hw-1.tex', kind: 'link', ext: 'tex', url: 'https://a.example/hw-1.tex' }),
  ];
  const kinds = async (importTex: boolean) =>
    (await runMapping(compileMapping(templateMapping({ system: 'web', importTex })), rows)).imports.map((i) => `${i.title} ${i.kind}`);
  assert.deepEqual(await kinds(false), ['Homework 1 pdf']);
  assert.deepEqual(await kinds(true), ['Homework 1 pdf', 'hw-1 tex']);
});

test('a custom mapping must compile against the import target', async () => {
  assert.throws(() => compileMapping('{'), /not a valid hockeymask document/);
  assert.throws(
    () => compileMapping({ id: 'x', nodes: [{ id: 'a', op: 'source', system: 'web', entity: 'items' }, { id: 'o', op: 'sink', in: 'a', target: 'nope', key: ['id'] }] }),
    /does not compile/,
  );
  // Every row as markdown, titled in capitals: a custom shape works.
  const plan = compileMapping({
    id: 'custom',
    nodes: [
      { id: 'items', op: 'source', system: 'web', entity: 'items' },
      {
        id: 'm', op: 'map', in: 'items', fields: [
          { key: 'import.external_id', pipeline: [{ op: 'extract', path: 'id' }] },
          { key: 'import.title', pipeline: [{ op: 'extract', path: 'name' }, { op: 'upper' }] },
          { key: 'import.kind', pipeline: [{ op: 'const', value: 'markdown' }] },
          { key: 'import.url', pipeline: [{ op: 'extract', path: 'url' }] },
          { key: 'import.link', pipeline: [{ op: 'const', value: null }] },
          { key: 'import.version', pipeline: [{ op: 'extract', path: 'updated_at' }] },
        ],
      },
      { id: 'out', op: 'sink', in: 'm', target: 'import', key: ['import.external_id'] },
    ],
  });
  const { imports } = await runMapping(plan, [row({ id: 'p', name: 'notes' })]);
  assert.deepEqual(imports.map((i) => [i.title, i.kind]), [['NOTES', 'markdown']]);
});

test('settings are checked and normalized', () => {
  assert.deepEqual(normalizeSettings({ kind: 'canvas', canvas_course: 'https://bcourses.berkeley.edu/courses/1234567/modules' }), {
    kind: 'canvas', url: 'https://bcourses.berkeley.edu', canvas_course: '1234567', include: null, import_pages: false, import_tex: false, mapping: null,
  });
  assert.equal(normalizeSettings({ kind: 'web', url: 'cs61a.org/fa26' }).url, 'https://cs61a.org/fa26');
  assert.throws(() => normalizeSettings({ kind: 'canvas', canvas_course: 'abc' }), /Pick a bCourses course/);
  assert.throws(() => normalizeSettings({ kind: 'web', url: 'https://10.0.0.1/' }), /IP address/);
  assert.throws(() => normalizeSettings({ kind: 'ftp' }), /Choose/);
  assert.equal(canvasBase('bcourses.berkeley.edu/courses/1'), 'https://bcourses.berkeley.edu');
  assert.equal(canvasCourseId(' 42 '), '42');
});

test('tokens are sealed per source and need FETCHER_KEY', async () => {
  const env = { FETCHER_KEY: 'k1' } as Env;
  const sealed = await sealToken(env, 'fetcher:ka', 'canvas-token');
  assert.doesNotMatch(sealed, /canvas-token/);
  assert.equal(await openToken(env, 'fetcher:ka', sealed), 'canvas-token');
  await assert.rejects(openToken(env, 'fetcher:kb', sealed), /Paste a new one/);
  await assert.rejects(openToken({ FETCHER_KEY: 'k2' } as Env, 'fetcher:ka', sealed), /Paste a new one/);
  await assert.rejects(sealToken({} as Env, 'fetcher:ka', 't'), /FETCHER_KEY/);
});

test('Canvas: files, modules and pages, with pagination and fallbacks', async () => {
  const base = 'https://bcourses.berkeley.edu';
  const json = (data: unknown, headers: Record<string, string> = {}, status = 200) =>
    new Response(JSON.stringify(data), { status, headers: { 'content-type': 'application/json', ...headers } });
  const seen = serve({
    'bcourses.berkeley.edu': (req) => {
      const u = new URL(req.url);
      assert.equal(req.headers.get('authorization'), 'Bearer tok');
      const p = u.pathname + u.search;
      // The Files tab is hidden: a 401 without WWW-Authenticate.
      if (u.pathname === '/api/v1/courses/7/files') return json({ status: 'unauthorized' }, {}, 401);
      if (u.pathname === '/api/v1/courses/7/pages' && !u.searchParams.has('page')) {
        return json([{ page_id: 1, url: 'week-1', title: 'Week 1', updated_at: '2026-09-01T00:00:00Z' }], {
          link: `<${base}/api/v1/courses/7/pages?page=2&per_page=100>; rel="next"`,
        });
      }
      if (u.pathname === '/api/v1/courses/7/pages') return json([{ page_id: 2, url: 'week-2', title: 'Week 2', published: false }]);
      if (u.pathname === '/api/v1/courses/7/modules') {
        return json([{ id: 1, name: 'Week 1', items: [
          { id: 10, title: 'Lecture 1', type: 'File', content_id: 55, url: `${base}/api/v1/courses/7/files/55` },
          { id: 11, title: 'Week 1', type: 'Page', page_url: 'week-1' },
          { id: 12, title: 'Locked', type: 'File', content_id: 56, url: `${base}/api/v1/courses/7/files/56` },
        ] }]);
      }
      if (u.pathname === '/api/v1/courses/7/files/55') {
        return json({ id: 55, display_name: 'Lecture 1.pdf', filename: 'lec1.pdf', 'content-type': 'application/pdf', size: 10, url: `${base}/files/55/download?verifier=v`, updated_at: '2026-09-02T00:00:00Z' });
      }
      if (u.pathname === '/api/v1/courses/7/files/56') return json({ id: 56, display_name: 'Locked.pdf', filename: 'l.pdf', url: `${base}/x`, locked_for_user: true });
      throw new Error(`unexpected ${p}`);
    },
  });
  const rows = await new CanvasClient(base, 'tok').items('7', { pages: true });
  assert.deepEqual(rows.map((r) => [r.id, r.name, r.kind, r.module]), [
    ['page:1', 'Week 1', 'page', 'Week 1'],
    ['file:55', 'Lecture 1.pdf', 'file', 'Week 1'],
  ]);
  assert.ok(seen.every((r) => new URL(r.url).origin === base));

  serve({ 'bcourses.berkeley.edu': () => new Response('{}', { status: 401, headers: { 'www-authenticate': 'Bearer realm="canvas-lms"' } }) });
  await assert.rejects(new CanvasClient(base, 'old').items('7', { pages: false }), CanvasAuthError);
  // A listing that points elsewhere never gets the token.
  serve({ 'bcourses.berkeley.edu': () => json([], { link: '<https://evil.example.com/steal>; rel="next"' }) });
  await assert.rejects(new CanvasClient(base, 'tok').courses(), /outside/);
});

// ── End to end, against a database ─────────────────────────────────────────

test('a web source imports new PDFs, replaces changed ones and respects deletions', {
  skip: !process.env.BOLO_TEST_SURREAL,
}, async () => {
  const namespace = `bolo_fetch_${crypto.randomUUID().replaceAll('-', '')}`;
  const runs: { id: string; params: unknown }[] = [];
  const ingests: unknown[] = [];
  const env = {
    SPKY_DB_URL: 'http://127.0.0.1:8666', SPKY_DB_NS: namespace, SPKY_DB_NAME: 'main', SPKY_DB_USER: 'root', SPKY_DB_PASS: 'root',
    FETCHER_KEY: 'test-key',
    FETCH: { create: async (r: { id: string; params: unknown }) => void runs.push(r) },
    INGEST: { create: async (r: unknown) => void ingests.push(r) },
  } as unknown as Env;
  const root = Surreal.system(env);
  const read = (file: string) => readFileSync(new URL(file, import.meta.url), 'utf8');
  const signup = async (email: string) => {
    const res = await realFetch(`${env.SPKY_DB_URL}/signup`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', accept: 'application/json' },
      body: JSON.stringify({ ns: namespace, db: 'main', ac: 'account', email, name: 'Student', password: 'pw-123456' }),
    });
    assert.ok(res.ok, await res.clone().text());
    const user = Surreal.as(env, ((await res.json()) as { token: string }).token);
    return { user, id: await user.query<string>('RETURN $auth.id') };
  };

  let lecture1 = 'v1';
  const site = {
    'course.example.edu': (req: Request) => {
      const path = new URL(req.url).pathname;
      if (path === '/') {
        return new Response(
          `<title>Course</title><h2>Week 1</h2><a href="/l1.pdf">Lecture 1</a> <a href="/hw.pdf">Homework</a>
           <h2>Week 2</h2><a href="/l2.pdf">Lecture 2</a> <a href="/notes.docx">Lecture notes</a> <a href="/hw-1.tex">LaTeX template</a>`,
          { headers: { 'content-type': 'text/html' } },
        );
      }
      if (path === '/hw-1.tex') {
        return new Response('\\documentclass{article}\\begin{document}\\section*{Problem 1}Show $x^2 \\ge 0$.\\end{document}', {
          headers: { 'content-type': 'text/x-tex', etag: '"tex1"' },
        });
      }
      const etag = path === '/l1.pdf' ? `"${lecture1}"` : '"same"';
      if (req.method === 'HEAD') return new Response(null, { headers: { etag } });
      return new Response(PDF(`${path} ${lecture1}`), { headers: { 'content-type': 'application/pdf', etag } });
    },
  };

  try {
    await root.query(`DEFINE NAMESPACE ${namespace}; DEFINE DATABASE main;`);
    await root.queryAll(read('../../../schema/src/schema.surql'));
    await root.queryAll(read('../../../schema/src/outbox/api.surql'));
    await root.query(`DEFINE BUCKET materials BACKEND 'memory' PERMISSIONS NONE;`);
    await root.queryAll(read('../src/server-schema.surql'));
    const a = await signup('a@example.com');
    const b = await signup('b@example.com');
    await a.user.query(`CREATE course:ka CONTENT { owner: $auth.id, title: 'Course' }`);
    serve(site);

    // Nobody adds a source to someone else's course.
    await assert.rejects(createSource(env, b.user, b.id, { course: 'course:ka', kind: 'web', url: 'https://course.example.edu/' }), /Course not found/);

    const { id } = await createSource(env, a.user, a.id, { course: 'course:ka', kind: 'web', url: 'https://course.example.edu/', include: 'lecture' });
    assert.equal(runs.length, 1);
    const runId = (runs[0].params as { runId: string }).runId;
    // A second start while the first runs does nothing.
    assert.deepEqual(await startFetch(env, id, { manual: true }), { started: false });
    assert.equal((await getFetcher(env, id))?.status, 'running');

    // What the Workflow does, step by step.
    const plan = (await planRun(env, id))!;
    assert.deepEqual(plan.items.map((i) => [i.title, i.status]), [['Lecture 1', 'new'], ['Lecture 2', 'new']]);
    for (const item of plan.items) assert.equal(await importItem(env, id, item), 'created');
    // A retried step does not import twice.
    assert.equal(await importItem(env, id, plan.items[0]), 'created');
    await finishRun(env, id, runId, { status: 'ok', found: plan.found, created: 2, updated: 0, failed: 0, deferred: 0 });

    const materials = await a.user.query<{ id: string; title: string; kind: string; fetcher: string; source_url: string; status: string }[]>(
      'SELECT id, title, kind, fetcher, source_url, status FROM material ORDER BY title',
    );
    assert.deepEqual(materials.map((m) => [m.title, m.kind, m.fetcher, m.source_url, m.status]), [
      ['Lecture 1', 'pdf', id, 'https://course.example.edu/l1.pdf', 'processing'],
      ['Lecture 2', 'pdf', id, 'https://course.example.edu/l2.pdf', 'processing'],
    ]);
    assert.equal(ingests.length, 2);
    const f = await a.user.query<{ status: string; imported_count: number; next_run_at: string }>('SELECT * FROM ONLY type::record($id)', { id });
    assert.equal(f.status, 'ok');
    assert.equal(f.imported_count, 2);
    assert.ok(new Date(f.next_run_at).getTime() > Date.now() + 5 * 3600_000);
    const run = await a.user.query<{ status: string; created: number }>('SELECT * FROM ONLY type::record($id)', { id: runId });
    assert.deepEqual([run.status, run.created], ['ok', 2]);
    // The ledger is server-only.
    assert.deepEqual(await a.user.query('SELECT * FROM fetched_item'), []);

    // Lecture 1 changes upstream; the student deletes Lecture 2.
    lecture1 = 'v2';
    await a.user.query('DELETE type::record($id)', { id: materials[1].id });
    const again = (await planRun(env, id))!;
    assert.deepEqual(again.items.map((i) => [i.title, i.status, i.material]), [['Lecture 1', 'changed', materials[0].id]]);
    assert.equal(await importItem(env, id, again.items[0]), 'updated');
    assert.equal(ingests.length, 3);
    assert.deepEqual(((await planRun(env, id))!).items, []);

    // The schedule starts it again once due, and skips disabled sources.
    assert.equal(await startDueFetches(env), 0);
    await root.query('UPDATE type::record($id) SET next_run_at = time::now() - 1m', { id });
    assert.equal(await startDueFetches(env), 1);
    assert.equal(runs.length, 2);
    await finishRun(env, id, (runs[1].params as { runId: string }).runId, { status: 'ok', found: 0, created: 0, updated: 0, failed: 0, deferred: 0 });
    await a.user.query('UPDATE type::record($id) SET enabled = false', { id });
    await root.query('UPDATE type::record($id) SET next_run_at = time::now() - 1m', { id });
    assert.equal(await startDueFetches(env), 0);
    // A refused token stops scheduled checks until the student fixes it.
    await a.user.query('UPDATE type::record($id) SET enabled = true', { id });
    await finishRun(env, id, (runs[1].params as { runId: string }).runId, {
      status: 'failed', found: 0, created: 0, updated: 0, failed: 0, deferred: 0, error: 'Token refused', disable: true,
    });
    const stopped = await a.user.query<{ enabled: boolean; last_error: string }>('SELECT enabled, last_error FROM ONLY type::record($id)', { id });
    assert.deepEqual([stopped.enabled, stopped.last_error], [false, 'Token refused']);

    // Tokens: sealed at rest, never readable by the student.
    await saveToken(env, id, 'canvas-secret');
    assert.equal(await loadToken(env, id), 'canvas-secret');
    assert.deepEqual(await a.user.query('SELECT * FROM fetcher_secret'), []);
    assert.doesNotMatch(JSON.stringify(await root.query('SELECT * FROM fetcher_secret')), /canvas-secret/);

    // Deleting the source keeps its material.
    await a.user.query('DELETE type::record($id)', { id });
    assert.deepEqual(await root.query('SELECT VALUE id FROM fetch_run'), []);
    assert.deepEqual(await root.query('SELECT VALUE id FROM fetched_item'), []);
    assert.deepEqual(await root.query('SELECT VALUE id FROM fetcher_secret'), []);
    const kept = await a.user.query<{ fetcher?: string }[]>('SELECT fetcher FROM material');
    assert.deepEqual(kept.map((m) => m.fetcher ?? null), [null]);

    // LaTeX sources come in as tex material, kept as written.
    const tex = await createSource(env, a.user, a.id, {
      course: 'course:ka', kind: 'web', url: 'https://course.example.edu/', include: 'template', import_tex: true,
    });
    const texPlan = (await planRun(env, tex.id))!;
    assert.deepEqual(texPlan.items.map((i) => [i.title, i.kind]), [['LaTeX template', 'tex']]);
    assert.equal(await importItem(env, tex.id, texPlan.items[0]), 'created');
    const texMaterial = await a.user.query<{ kind: string; content: string }>(
      'SELECT kind, content FROM ONLY material WHERE fetcher = type::record($f) LIMIT 1', { f: tex.id },
    );
    assert.equal(texMaterial.kind, 'tex');
    assert.match(texMaterial.content, /\\section\*\{Problem 1\}/);
  } finally {
    await root.query(`REMOVE NAMESPACE ${namespace};`).catch(() => {});
  }
});
