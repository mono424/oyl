import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { registerHooks } from 'node:module';
import { test } from 'node:test';
import type { Env } from '../src/env.ts';
import { ensureServerSchema, storePdfAndIngest } from '../src/service.ts';
import { Conflict, Surreal, signinRecord } from '../src/surreal.ts';

test('the original PDF is kept, served to its owner only, and never replaced once working', {
  skip: !process.env.BOLO_TEST_SURREAL,
}, async (t) => {
  const hook = registerHooks({
    resolve(specifier, context, nextResolve) {
      if (specifier === 'cloudflare:workers') {
        return {
          url: `data:text/javascript,${encodeURIComponent('export class WorkflowEntrypoint { constructor(ctx, env) { this.env = env; } }')}`,
          shortCircuit: true,
        };
      }
      return nextResolve(specifier, context);
    },
  });
  const { default: app } = await import('../src/index.ts');
  const namespace = `bolo_pdf_${crypto.randomUUID().replaceAll('-', '')}`;
  const launched: string[] = [];
  const env = {
    SPKY_DB_URL: 'http://127.0.0.1:8666', SPKY_DB_NS: namespace, SPKY_DB_NAME: 'main',
    SPKY_DB_USER: 'root', SPKY_DB_PASS: 'root', APP_ORIGIN: 'https://bolo.example',
    INGEST: { create: async ({ id }: { id: string }) => { launched.push(id); } },
  } as unknown as Env;
  const db = Surreal.system(env);
  // Two parts: the first 1 MiB and the rest.
  const pdf = new Uint8Array(1024 * 1024 + 4096);
  pdf.set(new TextEncoder().encode('%PDF-1.7\n'));
  for (let i = 9; i < pdf.length; i++) pdf[i] = i % 251;

  try {
    await db.query(`DEFINE NAMESPACE ${namespace}; DEFINE DATABASE main;`);
    await db.queryAll(await readFile(new URL('../../../schema/src/schema.surql', import.meta.url), 'utf8'));
    await db.query(`DEFINE BUCKET materials BACKEND 'memory' PERMISSIONS NONE;
      CREATE user:u1 SET email = 'one@example.com', name = 'One', password = crypto::argon2::generate('pw-one');
      CREATE user:u2 SET email = 'two@example.com', name = 'Two', password = crypto::argon2::generate('pw-two');
      CREATE course:c1 SET owner = user:u1, title = 'Algebra';
      CREATE material:m1 SET owner = user:u1, course = course:c1, title = 'Slides', kind = 'pdf', status = 'pending';
      CREATE material:md SET owner = user:u1, course = course:c1, title = 'Notes', kind = 'markdown', content = '# Notes';`);
    await ensureServerSchema(env);
    const one = await signinRecord(env, 'account', { email: 'one@example.com', password: 'pw-one' });
    const two = await signinRecord(env, 'account', { email: 'two@example.com', password: 'pw-two' });
    assert.ok(one && two);
    const get = (id: string, token: string) =>
      app.fetch(new Request(`https://bolo.example/api/materials/${id}/pdf`, { headers: { authorization: `Bearer ${token}` } }), env);

    await t.test('an upload is stored in parts and streamed back whole', async () => {
      await storePdfAndIngest(env, { id: 'material:m1', owner: 'user:u1' }, pdf, 'Slides 1.pdf');
      assert.equal(launched.length, 1);
      const parts = await db.query<number>('RETURN count(SELECT id FROM source_part WHERE material = material:m1)');
      assert.equal(parts, 2);
      const res = await get('m1', one);
      assert.equal(res.status, 200);
      assert.equal(res.headers.get('content-type'), 'application/pdf');
      assert.match(res.headers.get('content-disposition') ?? '', /filename\*=UTF-8''Slides%201\.pdf/);
      assert.deepEqual(new Uint8Array(await res.arrayBuffer()), pdf);
    });

    await t.test('only the owner gets it, and only for a PDF', async () => {
      assert.equal((await get('m1', two)).status, 404);
      assert.equal((await get('md', one)).status, 404);
      assert.equal((await get('m1', 'not-a-token')).status, 401);
    });

    await t.test('a working material keeps its PDF; a failed upload may be replaced', async () => {
      await db.query(`UPDATE material:m1 SET status = 'ready'`);
      await assert.rejects(storePdfAndIngest(env, { id: 'material:m1', owner: 'user:u1' }, pdf, 'Other.pdf'), Conflict);
      const res = await app.fetch(new Request('https://bolo.example/api/materials/m1/pdf', {
        method: 'POST', body: pdf, headers: { authorization: `Bearer ${one}`, 'content-type': 'application/pdf' },
      }), env);
      assert.equal(res.status, 409);
      const after = await db.query<{ status: string; source_name: string }>('SELECT status, source_name FROM ONLY material:m1');
      assert.deepEqual(after, { status: 'ready', source_name: 'Slides 1.pdf' });

      await db.query(`UPDATE material:m1 SET status = 'failed'`);
      await storePdfAndIngest(env, { id: 'material:m1', owner: 'user:u1' }, pdf.subarray(0, 2048), 'Retry.pdf');
      assert.equal((await get('m1', one)).headers.get('content-disposition'), "inline; filename*=UTF-8''Retry.pdf");
      assert.equal(await db.query<number>('RETURN count(SELECT id FROM source_part WHERE material = material:m1)'), 1);
    });

    await t.test('deleting the material deletes the PDF', async () => {
      const ids = await db.query<string[]>('SELECT VALUE record::id(id) FROM source_part WHERE material = material:m1');
      await db.query('DELETE material:m1');
      assert.equal(await db.query<number>('RETURN count(SELECT id FROM source_part WHERE material = material:m1)'), 0);
      for (const id of ids) {
        assert.equal(await db.query<boolean>(`RETURN type::file('materials', '/sources/' + $id).exists()`, { id }), false);
      }
    });
  } finally {
    await db.query(`REMOVE NAMESPACE ${namespace};`);
    hook.deregister();
  }
});
