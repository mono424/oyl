import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { registerHooks } from 'node:module';
import { test } from 'node:test';
import type { Env } from '../src/env.ts';
import { Sha256 } from '../src/sha256.ts';
import { MAX_BLOB_BYTES, MAX_FILES, parseFiles, publicDatabase, syncPath } from '../src/sync.ts';
import { connectOrigin, formatUserCode, newUserCode, parseUserCode } from '../src/sync-device.ts';
import { PART_BYTES } from '../src/service.ts';
import { Surreal, signinRecord } from '../src/surreal.ts';

const hex = (bytes: Uint8Array) => [...bytes].map((b) => b.toString(16).padStart(2, '0')).join('');
const sha = async (bytes: Uint8Array) => hex(new Uint8Array(await crypto.subtle.digest('SHA-256', bytes)));
const text = (s: string) => new TextEncoder().encode(s);

// index.ts pulls in sp00ky's workflow runtime, which imports cloudflare:workers.
const workersStub = registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier === 'cloudflare:workers') {
      return { url: `data:text/javascript,${encodeURIComponent('export class WorkflowEntrypoint { constructor(ctx, env) { this.env = env; } }')}`, shortCircuit: true };
    }
    return nextResolve(specifier, context);
  },
});
const { default: app } = await import('../src/index.ts');
workersStub.deregister();

test('the streaming SHA-256 matches WebCrypto across block and chunk boundaries', async () => {
  for (const n of [0, 1, 55, 56, 63, 64, 65, 119, 120, 1000, 70_001]) {
    const data = new Uint8Array(n);
    // getRandomValues fills at most 64 KiB at a time.
    for (let off = 0; off < n; off += 65536) crypto.getRandomValues(data.subarray(off, off + 65536));
    const hash = new Sha256();
    for (let off = 0; off < n; ) {
      const step = 1 + ((off * 7919) % 97);
      hash.update(data.subarray(off, off + step));
      off += step;
    }
    assert.equal(hash.hex(), await sha(data), `${n} bytes`);
  }
  assert.equal(new Sha256().update(text('abc')).hex(), 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
});

test('syncPath keeps paper paths and refuses ones that climb out, hide in .oyl or are not NFC', () => {
  for (const ok of ['main.typ', 'figures/a b.png', 'sources/Ünïcode.pdf', '.latexmkrc', 'a/.gitignore']) assert.equal(syncPath(ok), ok);
  for (const bad of ['', '/abs', 'a/../b', '..', './a', 'a//b', 'a/', 'a\\b', 'a\nb', '.oyl/history', '.git/HEAD', 'x'.repeat(513), 'Cafe\u0301.typ', 7]) {
    assert.equal(syncPath(bad), null, String(bad));
  }
});

test('a file map is checked and stored with its paths sorted', () => {
  const a = 'a'.repeat(64);
  const b = 'b'.repeat(64);
  const map = parseFiles({ 'z.typ': { sha256: a, size: 3 }, 'figures/1.png': { sha256: b, size: 10, extra: true }, '1': { sha256: a, size: 3 } });
  assert.deepEqual(map.entries.map(([p]) => p), ['1', 'figures/1.png', 'z.typ']);
  assert.equal(map.json, `{"1":{"sha256":"${a}","size":3},"figures/1.png":{"sha256":"${b}","size":10},"z.typ":{"sha256":"${a}","size":3}}`);
  assert.equal(map.size, 16);
  assert.deepEqual(JSON.parse(map.json)['z.typ'], { sha256: a, size: 3 });
  for (const bad of [
    null, [], { 'a.typ': { sha256: 'A'.repeat(64), size: 1 } }, { 'a.typ': { sha256: a, size: -1 } },
    { 'a.typ': { sha256: a, size: MAX_BLOB_BYTES + 1 } }, { 'a.typ': { sha256: a, size: 1.5 } }, { '../a.typ': { sha256: a, size: 1 } },
    { 'A.typ': { sha256: a, size: 1 }, 'a.typ': { sha256: a, size: 1 } },
    Object.fromEntries(Array.from({ length: MAX_FILES + 1 }, (_, i) => [`f${i}`, { sha256: a, size: 1 }])),
  ]) assert.throws(() => parseFiles(bad), /files|path|sha256|size|case|at most/i);
});

test('user codes are eight consonants, read back in any case, with or without the dash', () => {
  for (let i = 0; i < 50; i++) {
    const code = newUserCode();
    assert.match(code, /^[BCDFGHJKLMNPQRSTVWXZ]{8}$/);
    assert.equal(parseUserCode(formatUserCode(code).toLowerCase()), code);
  }
  assert.equal(formatUserCode('BCDFGHJK'), 'BCDF-GHJK');
  for (const bad of ['', 'BCDF-GHJ', 'ABCD-EFGH', 'BCDF-GHJKL', 42]) assert.equal(parseUserCode(bad), null);
});

test('the approval page is the app origin serving the API, or a local one for a local API', () => {
  const env = (APP_ORIGIN: string) => ({ APP_ORIGIN }) as Env;
  assert.equal(connectOrigin(new Request('https://oyl.example/api/sync/device'), env('https://oyl.example')), 'https://oyl.example');
  assert.equal(connectOrigin(new Request('http://localhost:8787/api/sync/device'), env('http://localhost:5180,https://oyl.example')), 'http://localhost:5180');
  assert.equal(connectOrigin(new Request('https://api.oyl.example/api/sync/device'), env('http://localhost:5180,https://oyl.example')), 'https://oyl.example');
});

test('the database clients reach is the Worker\'s own as a WebSocket, unless set', () => {
  const env = { SPKY_DB_URL: 'https://talktobolo-db.stg.spky.cloud/', SPKY_DB_NS: 'talktobolo', SPKY_DB_NAME: 'main' } as Env;
  assert.deepEqual(publicDatabase(env), { endpoint: 'wss://talktobolo-db.stg.spky.cloud/rpc', namespace: 'talktobolo', database: 'main' });
  assert.equal(publicDatabase({ ...env, SPKY_DB_URL: 'http://127.0.0.1:8666' }).endpoint, 'ws://127.0.0.1:8666/rpc');
  assert.equal(publicDatabase({ ...env, DB_PUBLIC_URL: 'wss://db.example/rpc' }).endpoint, 'wss://db.example/rpc');
  assert.equal(publicDatabase({ ...env, DB_PUBLIC_URL: 'https://db.example' }).endpoint, 'wss://db.example/rpc');
});

test('every sync endpoint but the device flow needs a token', async () => {
  const env = { APP_ORIGIN: 'https://oyl.example' } as Env;
  const routes: [string, string][] = [
    ['GET', '/api/sync/database'], ['GET', '/api/sync/papers'], ['POST', '/api/sync/papers'], ['PATCH', '/api/sync/papers/paper_sync:k1'],
    ['GET', '/api/sync/papers/paper_sync:k1/head'], ['GET', '/api/sync/papers/paper_sync:k1/revisions'],
    ['GET', '/api/sync/papers/paper_sync:k1/revisions/paper_revision:k1'], ['POST', '/api/sync/papers/paper_sync:k1/revisions'],
    ['POST', '/api/sync/blobs/missing'], ['PUT', `/api/sync/blobs/${'a'.repeat(64)}`], ['GET', `/api/sync/blobs/${'a'.repeat(64)}`],
    ['POST', '/api/sync/device/request'], ['POST', '/api/sync/device/approve'],
  ];
  for (const [method, path] of routes) {
    const res = await app.fetch(new Request(`https://oyl.example${path}`, { method }), env);
    assert.equal(res.status, 401, `${method} ${path}`);
  }
  const poll = await app.fetch(new Request('https://oyl.example/api/sync/device/token', { method: 'POST', body: '{"device_code":"short"}' }), env);
  assert.equal(poll.status, 400);
});

test('paper sync end to end: device sign-in, papers, blobs, commits and permissions', { skip: !process.env.OYL_TEST_SURREAL }, async (t) => {
  const namespace = `oyl_sync_${crypto.randomUUID().replaceAll('-', '')}`;
  const origin = 'https://oyl.example';
  const env = {
    SPKY_DB_URL: 'http://127.0.0.1:8666', SPKY_DB_NS: namespace, SPKY_DB_NAME: 'main', SPKY_DB_USER: 'root', SPKY_DB_PASS: 'root',
    APP_ORIGIN: origin,
  } as Env;
  const db = Surreal.system(env);

  interface Reply { status: number; headers: Headers; body: any }
  const call = async (method: string, path: string, opts: { token?: string; json?: unknown; body?: BodyInit; headers?: Record<string, string> } = {}): Promise<Reply> => {
    const headers = new Headers(opts.headers);
    if (opts.token) headers.set('authorization', `Bearer ${opts.token}`);
    if (opts.json !== undefined) headers.set('content-type', 'application/json');
    const res = await app.fetch(new Request(`${origin}/api/sync${path}`, {
      method, headers, body: opts.json !== undefined ? JSON.stringify(opts.json) : opts.body, ...(opts.body instanceof ReadableStream ? { duplex: 'half' } : {}),
    } as RequestInit), env);
    const type = res.headers.get('content-type') ?? '';
    return { status: res.status, headers: res.headers, body: type.includes('json') ? await res.json() : new Uint8Array(await res.arrayBuffer()) };
  };
  const enc = (id: string) => encodeURIComponent(id);
  const stagingFiles = async () => (await db.query<unknown[]>(`RETURN file::list('materials', { prefix: '/papers/up' });`)).length;

  try {
    await db.query(`DEFINE NAMESPACE ${namespace}; USE NS ${namespace}; DEFINE DATABASE main; USE DB main;
      DEFINE BUCKET materials BACKEND 'memory' PERMISSIONS NONE;`);
    await db.queryAll(await readFile(new URL('../../../schema/src/schema.surql', import.meta.url), 'utf8'));
    await db.query(`
      CREATE user:u1 CONTENT { email: 'student@example.test', name: 'Student', password: crypto::argon2::generate('pw') };
      CREATE user:u2 CONTENT { email: 'other@example.test', name: 'Other', password: crypto::argon2::generate('pw') };`);
    const web1 = (await signinRecord(env, 'account', { email: 'student@example.test', password: 'pw' }))!;
    const web2 = (await signinRecord(env, 'account', { email: 'other@example.test', password: 'pw' }))!;
    assert.ok(web1 && web2);

    const start = async (name = 'MacBook Pro') => {
      const res = await call('POST', '/device', { json: { device_name: name } });
      assert.equal(res.status, 200);
      return res.body as { device_code: string; user_code: string; verify_url: string; interval: number; expires_in: number };
    };
    const poll = (deviceCode: string) => call('POST', '/device/token', { json: { device_code: deviceCode } });
    const pollNow = async (deviceCode: string) => {
      await db.query('UPDATE sync_device_code SET polled_at = time::now() - 1m WHERE polled_at != NONE');
      return poll(deviceCode);
    };
    let device = '';

    await t.test('a device signs in through the browser and gets its key once', async () => {
      const flow = await start('MacBook Pro\u0007 ');
      assert.match(flow.user_code, /^[BCDFGHJKLMNPQRSTVWXZ]{4}-[BCDFGHJKLMNPQRSTVWXZ]{4}$/);
      assert.equal(flow.verify_url, `${origin}/connect?code=${flow.user_code}`);
      assert.equal(flow.interval, 5);
      assert.equal(flow.expires_in, 600);

      const pending = await poll(flow.device_code);
      assert.equal(pending.status, 202);
      assert.equal(pending.body.status, 'pending');
      assert.equal(pending.headers.get('retry-after'), '5');
      const fast = await poll(flow.device_code);
      assert.equal(fast.status, 429);
      assert.equal(fast.body.code, 'slow_down');

      assert.equal((await call('POST', '/device/request', { json: { user_code: flow.user_code } })).status, 401);
      assert.equal((await call('POST', '/device/request', { token: web1, json: { user_code: 'nope' } })).status, 400);
      assert.equal((await call('POST', '/device/request', { token: web1, json: { user_code: 'BBBB-BBBB' } })).status, 404);
      const shown = await call('POST', '/device/request', { token: web1, json: { user_code: flow.user_code.toLowerCase().replace('-', '') } });
      assert.equal(shown.status, 200);
      assert.equal(shown.body.device_name, 'MacBook Pro');
      assert.equal(shown.body.status, 'pending');

      const approved = await call('POST', '/device/approve', { token: web1, json: { user_code: flow.user_code, allow: true } });
      assert.deepEqual(approved.body, { status: 'approved', device_name: 'MacBook Pro' });
      assert.equal((await call('POST', '/device/approve', { token: web1, json: { user_code: flow.user_code, allow: false } })).status, 409);

      const granted = await pollNow(flow.device_code);
      assert.equal(granted.status, 200);
      assert.match(granted.body.token, /^oyl_sync_[A-Za-z0-9_-]{43}$/);
      assert.deepEqual(granted.body.account, { id: 'user:u1', name: 'Student', email: 'student@example.test' });
      device = granted.body.token;
      assert.equal((await pollNow(flow.device_code)).status, 410);

      const keys = await db.query<{ name: string; prefix: string; owner: string }[]>('SELECT name, prefix, owner FROM api_key');
      assert.deepEqual(keys, [{ name: 'Oyl on MacBook Pro', prefix: device.slice(0, 13), owner: 'user:u1' }]);
    });

    await t.test('the sync key opens /api/sync and the user\'s database session, nothing else', async () => {
      assert.equal(await signinRecord(env, 'agent', { key: device }), null);
      const session = await signinRecord(env, 'device', { key: device });
      assert.ok(session, 'the device signs in to SurrealDB with its key');
      assert.equal(await Surreal.as(env, session!).query<string>('RETURN $auth.id'), 'user:u1');
      assert.equal(await signinRecord(env, 'device', { key: `${device.slice(0, -1)}x` }), null);
      assert.deepEqual((await call('GET', '/database', { token: device })).body, {
        endpoint: 'ws://127.0.0.1:8666/rpc', namespace, database: 'main',
      });
      const keys = await app.fetch(new Request(`${origin}/api/keys`, { method: 'POST', headers: { authorization: `Bearer ${device}` }, body: '{}' }), env);
      assert.equal(keys.status, 401);
      const ctx = { waitUntil() {}, passThroughOnException() {}, props: {} } as unknown as ExecutionContext;
      const mcp = await app.fetch(new Request(`${origin}/mcp`, { method: 'POST', headers: { authorization: `Bearer ${device}` } }), env, ctx);
      assert.equal(mcp.status, 401);
      assert.equal((await call('GET', '/papers', { token: `${device.slice(0, -1)}x` })).status, 401);
      assert.equal((await call('GET', '/papers', { token: device })).status, 200);
    });

    await t.test('a declined or expired sign-in gives no key', async () => {
      const declined = await start('Work Mac');
      await call('POST', '/device/approve', { token: web2, json: { user_code: declined.user_code, allow: false } });
      const refused = await poll(declined.device_code);
      assert.equal(refused.status, 403);
      assert.equal(refused.body.code, 'access_denied');
      assert.equal((await pollNow(declined.device_code)).status, 410);

      const late = await start('Old Mac');
      await db.query(`UPDATE sync_device_code SET expires_at = time::now() - 1s WHERE device_name = 'Old Mac'`);
      assert.equal((await call('POST', '/device/approve', { token: web1, json: { user_code: late.user_code, allow: true } })).status, 404);
      const gone = await poll(late.device_code);
      assert.equal(gone.status, 410);
      assert.equal(gone.body.code, 'expired_token');
      assert.equal(await db.query<number>('RETURN count(SELECT id FROM api_key)'), 1);
    });

    let paper = '';
    let otherPaper = '';
    await t.test('papers are created, listed and changed by their owner only', async () => {
      assert.equal((await call('POST', '/papers', { token: device, json: { name: 'Thesis', engine: 'word', main: 'main.typ' } })).status, 400);
      assert.equal((await call('POST', '/papers', { token: device, json: { name: 'Thesis', engine: 'typst', main: '../main.typ' } })).status, 400);
      assert.equal((await call('POST', '/papers', { token: device, json: { name: '  ', engine: 'typst', main: 'main.typ' } })).status, 400);
      const made = await call('POST', '/papers', { token: device, json: { name: 'Thesis', engine: 'typst', main: 'main.typ' } });
      assert.equal(made.status, 201);
      assert.match(made.body.id, /^paper_sync:k[0-9a-f]{32}$/);
      assert.equal(made.body.head, null);
      assert.equal(made.body.deleted, false);
      paper = made.body.id;

      const id = `paper_sync:k${'1'.repeat(32)}`;
      assert.equal((await call('POST', '/papers', { token: device, json: { id, name: 'Notes', engine: 'latex', main: 'main.tex' } })).status, 201);
      const again = await call('POST', '/papers', { token: device, json: { id, name: 'Notes', engine: 'latex', main: 'main.tex' } });
      assert.equal(again.status, 200);
      assert.equal(again.body.id, id);
      assert.equal((await call('POST', '/papers', { token: web2, json: { id, name: 'Mine', engine: 'latex', main: 'main.tex' } })).status, 409);
      otherPaper = (await call('POST', '/papers', { token: web2, json: { name: 'Other paper', engine: 'latex', main: 'main.tex' } })).body.id;

      const mine = await call('GET', '/papers', { token: device });
      assert.deepEqual(mine.body.map((p: { name: string }) => p.name).sort(), ['Notes', 'Thesis']);
      const theirs = await call('GET', '/papers', { token: web2 });
      assert.deepEqual(theirs.body.map((p: { id: string }) => p.id), [otherPaper]);

      assert.equal((await call('PATCH', `/papers/${enc(otherPaper)}`, { token: device, json: { name: 'Taken' } })).status, 404);
      assert.equal((await call('PATCH', `/papers/${enc(paper)}`, { token: device, json: {} })).status, 400);
      const renamed = await call('PATCH', `/papers/${enc(paper)}`, { token: device, json: { name: 'PhD thesis', main: 'thesis.typ' } });
      assert.equal(renamed.body.name, 'PhD thesis');
      assert.equal(renamed.body.main, 'thesis.typ');
      assert.equal((await call('GET', `/papers/${enc(paper)}/head`, { token: device })).status, 404);
    });

    const small = text('#set page(paper: "a4")\n= Hello\n');
    const smallSha = await sha(small);
    // Two parts and a bit, each byte telling where it is.
    const big = Uint8Array.from({ length: PART_BYTES * 2 + 100 }, (_, i) => (i ^ (i >> 8) ^ (i >> 16)) & 255);
    const bigSha = await sha(big);

    await t.test('blobs upload once per owner, are checked, and download with immutable caching', async () => {
      const asked = await call('POST', '/blobs/missing', { token: device, json: { hashes: [smallSha, bigSha, smallSha] } });
      assert.deepEqual(asked.body, { missing: [smallSha, bigSha] });
      assert.equal((await call('POST', '/blobs/missing', { token: device, json: { hashes: ['xyz'] } })).status, 400);

      const put = await call('PUT', `/blobs/${smallSha}`, { token: device, body: small });
      assert.equal(put.status, 201);
      assert.deepEqual(put.body, { sha256: smallSha, size: small.length });
      assert.equal((await call('PUT', `/blobs/${smallSha}`, { token: device, body: small })).status, 200);
      assert.equal((await call('PUT', `/blobs/${bigSha}`, { token: device, body: big })).status, 201);

      const wrong = await call('PUT', `/blobs/${'0'.repeat(64)}`, { token: device, body: small });
      assert.equal(wrong.status, 400);
      assert.equal(wrong.body.sha256, smallSha);
      assert.equal((await call('PUT', `/blobs/${'0'.repeat(64)}`, { token: device, body: big })).status, 400);
      assert.equal(await stagingFiles(), 0);
      assert.equal((await call('PUT', '/blobs/ABC', { token: device, body: small })).status, 400);
      const declared = await call('PUT', `/blobs/${'1'.repeat(64)}`, { token: device, body: small, headers: { 'content-length': String(MAX_BLOB_BYTES + 1) } });
      assert.equal(declared.status, 413);

      assert.deepEqual((await call('POST', '/blobs/missing', { token: device, json: { hashes: [smallSha, bigSha] } })).body, { missing: [] });
      assert.deepEqual((await call('POST', '/blobs/missing', { token: web2, json: { hashes: [smallSha] } })).body, { missing: [smallSha] });

      const got = await call('GET', `/blobs/${bigSha}`, { token: device });
      assert.equal(got.status, 200);
      assert.deepEqual(got.body, big);
      assert.equal(got.headers.get('etag'), `"${bigSha}"`);
      assert.equal(got.headers.get('content-length'), String(big.length));
      assert.match(got.headers.get('cache-control') ?? '', /immutable/);
      assert.equal((await call('GET', `/blobs/${bigSha}`, { token: device, headers: { 'if-none-match': `"${bigSha}"` } })).status, 304);
      assert.deepEqual((await call('GET', `/blobs/${smallSha}`, { token: web1 })).body, small);
      assert.equal((await call('GET', `/blobs/${smallSha}`, { token: web2 })).status, 404);
    });

    await t.test('an upload past 50 MB stops and leaves nothing behind', async () => {
      let sent = 0;
      const endless = new ReadableStream<Uint8Array>({
        pull(controller) {
          if (sent > MAX_BLOB_BYTES) return controller.close();
          const chunk = new Uint8Array(4 * 1024 * 1024);
          sent += chunk.length;
          controller.enqueue(chunk);
        },
      });
      const res = await call('PUT', `/blobs/${'2'.repeat(64)}`, { token: device, body: endless });
      assert.equal(res.status, 413);
      assert.equal(await stagingFiles(), 0);
      assert.equal(await db.query<number>('RETURN count(SELECT id FROM sync_blob)'), 2);
    });

    let r1 = '';
    let r2 = '';
    const first = { 'thesis.typ': { sha256: smallSha, size: small.length }, 'figures/plot.png': { sha256: bigSha, size: big.length } };
    const second = { ...first, 'refs.bib': { sha256: smallSha, size: small.length } };
    await t.test('commits are compare-and-set on the head', async () => {
      const commit = (parent: string | null, files: unknown, token = device, extra: Record<string, unknown> = {}) =>
        call('POST', `/papers/${enc(paper)}/revisions`, { token, json: { parent, files, ...extra } });

      const missing = await commit(null, { ...first, 'x.png': { sha256: '3'.repeat(64), size: 4 } });
      assert.equal(missing.status, 422);
      assert.deepEqual(missing.body.missing, ['3'.repeat(64)]);
      assert.equal((await commit(null, { 'thesis.typ': { sha256: smallSha, size: 1 } })).status, 400);
      assert.equal((await commit(null, { '.oyl/state.json': { sha256: smallSha, size: small.length } })).status, 400);
      assert.equal((await commit(null, first, device, { author: 'robot' })).status, 400);

      const made = await commit(null, first, device, { message: 'First sync' });
      assert.equal(made.status, 201);
      assert.equal(made.body.n, 1);
      r1 = made.body.revision;
      const retried = await commit(null, first);
      assert.deepEqual([retried.status, retried.body], [200, { revision: r1, n: 1, unchanged: true }]);

      const stale = await commit(null, second);
      assert.equal(stale.status, 409);
      assert.equal(stale.body.head.revision, r1);
      assert.deepEqual(stale.body.head.files, first);

      const next = await commit(r1, second);
      assert.equal(next.status, 201);
      assert.equal(next.body.n, 2);
      r2 = next.body.revision;
      assert.deepEqual((await commit(r2, second)).body, { revision: r2, n: 2, unchanged: true });

      const head = await call('GET', `/papers/${enc(paper)}/head`, { token: device });
      assert.equal(head.body.revision, r2);
      assert.equal(head.body.parent, r1);
      assert.equal(head.body.author, 'device:MacBook Pro');
      assert.deepEqual(Object.keys(head.body.files), ['figures/plot.png', 'refs.bib', 'thesis.typ']);

      const listed = await call('GET', `/papers/${enc(paper)}/revisions`, { token: device });
      assert.deepEqual(listed.body.map((r: { revision: string }) => r.revision), [r2, r1]);
      assert.equal(listed.body[1].message, 'First sync');
      assert.equal(listed.body[0].files, undefined);
      assert.equal(listed.body[0].file_count, 3);
      assert.equal(listed.body[0].size, small.length * 2 + big.length);
      assert.deepEqual((await call('GET', `/papers/${enc(paper)}/revisions?limit=1`, { token: device })).body.map((r: { n: number }) => r.n), [2]);
      assert.deepEqual((await call('GET', `/papers/${enc(paper)}/revisions?before=2`, { token: device })).body.map((r: { n: number }) => r.n), [1]);
      const one = await call('GET', `/papers/${enc(paper)}/revisions/${enc(r1)}`, { token: device });
      assert.deepEqual(one.body.files, first);
      assert.equal((await call('GET', `/papers/${enc(paper)}/revisions/${r1.split(':')[1]}`, { token: device })).body.revision, r1);

      const listedPaper = (await call('GET', '/papers', { token: device })).body.find((p: { id: string }) => p.id === paper);
      assert.equal(listedPaper.head, r2);
      assert.equal(listedPaper.updated_by, 'device:MacBook Pro');
    });

    await t.test('two commits on the same head: one wins, the other gets 409', async () => {
      const a = { ...second, 'a.typ': { sha256: smallSha, size: small.length } };
      const b = { ...second, 'b.typ': { sha256: smallSha, size: small.length } };
      const results = await Promise.all([a, b].map((files) => call('POST', `/papers/${enc(paper)}/revisions`, { token: device, json: { parent: r2, files } })));
      assert.deepEqual(results.map((r) => r.status).sort(), [201, 409]);
      const winner = results.find((r) => r.status === 201)!;
      assert.equal(winner.body.n, 3);
      assert.equal(results.find((r) => r.status === 409)!.body.head.revision, winner.body.revision);
      assert.equal(await db.query<number>('RETURN count(SELECT id FROM paper_revision WHERE paper = type::record($p))', { p: paper }), 3);
      r2 = winner.body.revision;
    });

    await t.test('nobody reaches another account\'s paper', async () => {
      for (const [method, path] of [
        ['GET', `/papers/${enc(paper)}/head`], ['GET', `/papers/${enc(paper)}/revisions`], ['GET', `/papers/${enc(paper)}/revisions/${enc(r1)}`],
      ]) assert.equal((await call(method, path, { token: web2 })).status, 404, path);
      const foreign = await call('POST', `/papers/${enc(paper)}/revisions`, { token: web2, json: { parent: r2, files: {} } });
      assert.equal(foreign.status, 404);
      // A revision of one paper is not found through another.
      const notes = `paper_sync:k${'1'.repeat(32)}`;
      assert.equal((await call('GET', `/papers/${enc(notes)}/revisions/${enc(r1)}`, { token: device })).status, 404);
    });

    await t.test('the web app reads with its session and commits as `web`', async () => {
      assert.equal((await call('GET', `/papers/${enc(paper)}/head`, { token: web1 })).body.revision, r2);
      const head = (await call('GET', `/papers/${enc(paper)}/head`, { token: web1 })).body;
      const edited = await call('POST', `/papers/${enc(paper)}/revisions`, { token: web1, json: { parent: r2, files: first } });
      assert.equal(edited.status, 201);
      const revision = (await call('GET', `/papers/${enc(paper)}/revisions/${enc(edited.body.revision)}`, { token: web1 })).body;
      assert.equal(revision.author, 'web');
      assert.equal(revision.parent, head.revision);
      r2 = edited.body.revision;
    });

    await t.test('a paper removed from the cloud takes no commits until restored', async () => {
      const removed = await call('PATCH', `/papers/${enc(paper)}`, { token: device, json: { deleted: true } });
      assert.equal(removed.body.deleted, true);
      const refused = await call('POST', `/papers/${enc(paper)}/revisions`, { token: device, json: { parent: r2, files: second } });
      assert.equal(refused.status, 410);
      assert.equal((await call('GET', `/papers/${enc(paper)}/head`, { token: device })).status, 200);
      assert.equal((await call('PATCH', `/papers/${enc(paper)}`, { token: device, json: { deleted: false } })).body.deleted, false);
      assert.equal((await call('POST', `/papers/${enc(paper)}/revisions`, { token: device, json: { parent: r2, files: second } })).status, 201);
    });

    await t.test('record users read their own papers and revisions and write nothing', async () => {
      const student = Surreal.as(env, web1);
      const papers = await student.query<{ id: string }[]>('SELECT id FROM paper_sync');
      assert.deepEqual(papers.map((p) => p.id).sort(), [paper, `paper_sync:k${'1'.repeat(32)}`].sort());
      const revisions = await student.query<{ files: string }[]>('SELECT files FROM paper_revision WHERE paper = type::record($p)', { p: paper });
      assert.equal(revisions.length, 5);
      assert.ok(JSON.parse(revisions[0].files));
      assert.deepEqual(await Surreal.as(env, web2).query('SELECT id FROM paper_revision'), []);
      await student.query(`UPDATE type::record($p) SET name = 'Hacked', head = NONE`, { p: paper }).catch(() => undefined);
      assert.equal((await db.query<{ name: string }>('SELECT name FROM ONLY type::record($p)', { p: paper })).name, 'PhD thesis');
      // Refused writes come back empty or as an error, depending on the version.
      await student.query(`CREATE paper_sync CONTENT { owner: $auth.id, name: 'x', engine: 'typst', main: 'main.typ' }`).catch(() => undefined);
      await student.query(`CREATE paper_revision CONTENT { owner: $auth.id, paper: type::record($p), n: 99, files: '{}', file_count: 0, size: 0, author: 'web' }`, { p: paper }).catch(() => undefined);
      await student.query(`DELETE paper_revision`).catch(() => undefined);
      assert.equal(await db.query<number>('RETURN count(SELECT id FROM paper_sync)'), 3);
      assert.equal(await db.query<number>('RETURN count(SELECT id FROM paper_revision)'), 5);
      for (const table of ['sync_blob', 'sync_device', 'sync_device_code']) {
        assert.deepEqual(await student.query(`SELECT * FROM ${table}`), [], table);
      }
    });

    await t.test('revoking the connection in Settings stops the key at once', async () => {
      const student = Surreal.as(env, web1);
      await student.query('UPDATE api_key SET revoked_at = time::now()');
      assert.equal((await call('GET', '/papers', { token: device })).status, 401);
      assert.equal(await signinRecord(env, 'device', { key: device }), null, 'nor a new database session');
      await student.query('DELETE api_key');
      assert.equal(await db.query<number>('RETURN count(SELECT id FROM sync_device)'), 0);
    });
  } finally {
    await db.query(`REMOVE NAMESPACE ${namespace};`).catch(() => undefined);
  }
});
