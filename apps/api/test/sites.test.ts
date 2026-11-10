import assert from 'node:assert/strict';
import { registerHooks } from 'node:module';
import { test } from 'node:test';
import type { Env } from '../src/env.ts';
import { MAX_SITE_FILE_BYTES, parseRange, siteMime, sitePath, storeSiteFile, withStorageShim } from '../src/sites.ts';
import { PART_BYTES } from '../src/service.ts';
import { Surreal } from '../src/surreal.ts';

test('sitePath keeps site paths and refuses ones that climb out or name nothing', () => {
  assert.equal(sitePath('audio/1i4tg6u.mp3'), 'audio/1i4tg6u.mp3');
  assert.equal(sitePath('a b/ü.html'), 'a b/ü.html');
  for (const bad of ['', '/etc/passwd', 'a/../b', '..', './a', 'a//b', 'a/', 'a\\b', 'a\0b', 'x'.repeat(513)]) assert.equal(sitePath(bad), null, bad);
});

test('siteMime gives audio its real type and refuses to guess', () => {
  assert.equal(siteMime('audio/a.MP3'), 'audio/mpeg');
  assert.equal(siteMime('index.html'), 'text/html; charset=utf-8');
  assert.equal(siteMime('tool.exe'), 'application/octet-stream');
  assert.equal(siteMime('noextension'), 'application/octet-stream');
});

test('parseRange reads the forms browsers send', () => {
  assert.deepEqual(parseRange('bytes=0-1', 100), { start: 0, end: 1 });
  assert.deepEqual(parseRange('bytes=10-', 100), { start: 10, end: 99 });
  assert.deepEqual(parseRange('bytes=-5', 100), { start: 95, end: 99 });
  assert.deepEqual(parseRange('bytes=90-500', 100), { start: 90, end: 99 });
  assert.equal(parseRange('bytes=100-', 100), 'invalid');
  assert.equal(parseRange('bytes=5-2', 100), 'invalid');
  assert.equal(parseRange(undefined, 100), null);
  assert.equal(parseRange('bytes=0-1,5-6', 100), null);
});

test('the storage shim comes first in the head and leaves the doctype first', () => {
  const page = (html: string) => new TextDecoder().decode(withStorageShim(new TextEncoder().encode(html)));
  const out = page('<!doctype html><html><head lang="x"><title>ü</title></head></html>');
  assert.match(out, /^<!doctype html><html><head lang="x"><script>/);
  assert.match(out, /<\/script><title>ü<\/title>/);
  assert.match(page('<!DOCTYPE html><p>no head'), /^<!DOCTYPE html><script>/);
  assert.match(page('<p>bare'), /^<script>/);
});

test('sites upload, serve in ranges and delete with their files', { skip: !process.env.BOLO_TEST_SURREAL }, async (t) => {
  const hook = registerHooks({
    resolve(specifier, context, nextResolve) {
      if (specifier === 'cloudflare:workers') {
        return { url: `data:text/javascript,${encodeURIComponent('export class WorkflowEntrypoint { constructor(ctx, env) { this.env = env; } }')}`, shortCircuit: true };
      }
      return nextResolve(specifier, context);
    },
  });
  const { default: app } = await import('../src/index.ts');
  const { finishSite } = await import('../src/sites.ts');
  const namespace = `bolo_sites_${crypto.randomUUID().replaceAll('-', '')}`;
  const env = { SPKY_DB_URL: 'http://127.0.0.1:8666', SPKY_DB_NS: namespace, SPKY_DB_NAME: 'main', SPKY_DB_USER: 'root', SPKY_DB_PASS: 'root', APP_ORIGIN: 'https://bolo.example' } as Env;
  const db = Surreal.system(env);
  const key = `k${crypto.randomUUID().replaceAll('-', '')}`;
  const site = `site:${key}`;
  const get = (path: string, headers: Record<string, string> = {}) => app.fetch(new Request(`https://bolo.example/site-files/${key}${path}`, { headers }), env);
  const stream = (bytes: Uint8Array) => new Response(bytes).body;
  // Two parts and a bit, each byte telling where it is.
  const audio = Uint8Array.from({ length: PART_BYTES * 2 + 100 }, (_, i) => (i ^ (i >> 8) ^ (i >> 16)) & 255);
  const count = (table: string) => db.query<number>(`RETURN count(SELECT id FROM ${table});`);
  try {
    await db.query(`DEFINE NAMESPACE ${namespace}; USE NS ${namespace}; DEFINE DATABASE main; USE DB main;
      DEFINE BUCKET materials BACKEND 'memory' PERMISSIONS NONE;
      DEFINE TABLE site SCHEMALESS;
      CREATE type::record($site) CONTENT { owner: user:a, status: 'uploading', title: 'S' };`, { site });

    await t.test('files go up only for their owner, and a climbing path is refused', async () => {
      await storeSiteFile(env, 'user:a', site, 'index.html', stream(new TextEncoder().encode('<!doctype html><head><title>x</title>')));
      await storeSiteFile(env, 'user:a', site, 'audio/a b.mp3', stream(audio));
      await storeSiteFile(env, 'user:a', site, 'sub/index.html', stream(new TextEncoder().encode('<p>sub')));
      await assert.rejects(storeSiteFile(env, 'user:b', site, 'x.html', stream(new Uint8Array(1))), /not found/i);
      await assert.rejects(storeSiteFile(env, 'user:a', site, '../x.html', stream(new Uint8Array(1))), /path/i);
      await assert.rejects(storeSiteFile(env, 'user:a', site, 'big.bin', stream(new Uint8Array(MAX_SITE_FILE_BYTES + 1))), /up to 30 MB/);
      assert.deepEqual(await finishSite(env, 'user:a', site), { entry: 'index.html' });
      const row = await db.query<{ status: string; pages: string; file_count: number; size: number }>('RETURN (SELECT * FROM type::record($site))[0]', { site });
      assert.equal(row.status, 'ready');
      assert.deepEqual(JSON.parse(row.pages), ['index.html', 'sub/index.html']);
      assert.equal(row.file_count, 3);
    });

    await t.test('a published site takes no more files', async () => {
      await assert.rejects(storeSiteFile(env, 'user:a', site, 'late.txt', stream(new Uint8Array(1))), /already published/);
    });

    await t.test('every answer is sandboxed, and pages get the shim', async () => {
      const page = await get('/');
      assert.equal(page.status, 200);
      const csp = page.headers.get('content-security-policy') ?? '';
      assert.match(csp, /sandbox allow-scripts/);
      assert.doesNotMatch(csp, /allow-same-origin/);
      assert.match(await page.text(), /<head><script>/);
      for (const res of [await get('/audio/a%20b.mp3', { range: 'bytes=0-1' }), await get('/missing.png')]) {
        assert.match(res.headers.get('content-security-policy') ?? '', /sandbox/);
      }
    });

    await t.test('the bare address and a folder redirect to their slash', async () => {
      for (const [path, location] of [['', `/site-files/${key}/`], ['/sub', `/site-files/${key}/sub/`]]) {
        const res = await get(path);
        assert.equal(res.status, 308, path);
        assert.equal(res.headers.get('location'), location);
      }
      assert.match(await (await get('/sub/')).text(), /<script>/);
    });

    await t.test('audio answers ranges across its parts', async () => {
      const whole = await get('/audio/a%20b.mp3');
      assert.equal(whole.status, 200);
      assert.equal(whole.headers.get('content-type'), 'audio/mpeg');
      assert.equal(whole.headers.get('accept-ranges'), 'bytes');
      assert.deepEqual(new Uint8Array(await whole.arrayBuffer()), audio);

      const probe = await get('/audio/a%20b.mp3', { range: 'bytes=0-1' });
      assert.equal(probe.status, 206);
      assert.equal(probe.headers.get('content-range'), `bytes 0-1/${audio.length}`);
      assert.equal(probe.headers.get('content-length'), '2');
      assert.deepEqual(new Uint8Array(await probe.arrayBuffer()), audio.subarray(0, 2));

      const across = await get('/audio/a%20b.mp3', { range: `bytes=${PART_BYTES - 3}-${PART_BYTES + 2}` });
      assert.equal(across.status, 206);
      assert.deepEqual(new Uint8Array(await across.arrayBuffer()), audio.subarray(PART_BYTES - 3, PART_BYTES + 3));

      const past = await get('/audio/a%20b.mp3', { range: `bytes=${audio.length}-` });
      assert.equal(past.status, 416);
      assert.equal(past.headers.get('content-range'), `bytes */${audio.length}`);
    });

    await t.test('deleting the site takes its rows and bucket files', async () => {
      const part = `/sites/${(await db.query<{ id: string }[]>('SELECT id FROM site_file WHERE parts = 3'))[0].id.split(':')[1]}_2`;
      assert.equal(await db.query<boolean>(`RETURN type::file('materials', $part).exists();`, { part }), true);
      await db.query('DELETE type::record($site);', { site });
      assert.equal(await count('site_file'), 0);
      assert.equal(await db.query<boolean>(`RETURN type::file('materials', $part).exists();`, { part }), false);
      assert.equal((await get('/audio/a%20b.mp3')).status, 404);
    });
  } finally {
    hook.deregister();
    await db.query(`REMOVE NAMESPACE ${namespace};`).catch(() => undefined);
  }
});
