import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { registerHooks } from 'node:module';
import { test } from 'node:test';
import type { Env } from '../src/env.ts';
import { loadPdf } from '../src/pdf.ts';
import { ensureServerSchema } from '../src/service.ts';
import { Surreal, UserError } from '../src/surreal.ts';

test('file storage remains readable while PDF and image bytes move to buckets', {
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
  const namespace = `bolo_files_${crypto.randomUUID().replaceAll('-', '')}`;
  const env = {
    SPKY_DB_URL: 'http://127.0.0.1:8666', SPKY_DB_NS: namespace, SPKY_DB_NAME: 'main',
    SPKY_DB_USER: 'root', SPKY_DB_PASS: 'root', APP_ORIGIN: 'https://bolo.example',
  } as Env;
  const db = Surreal.system(env);
  const bytes = (text: string) => Buffer.from(text).toString('base64');
  const imageId = `k${crypto.randomUUID().replaceAll('-', '')}`;
  const missingImageId = `k${crypto.randomUUID().replaceAll('-', '')}`;
  const getImage = (id = imageId) => app.fetch(new Request(`https://bolo.example/material-assets/${id}`), env);
  try {
    await db.query(`DEFINE NAMESPACE ${namespace}; DEFINE DATABASE main;
      DEFINE BUCKET materials BACKEND 'memory' PERMISSIONS NONE;
      DEFINE TABLE source_part SCHEMAFULL;
      DEFINE FIELD owner ON source_part TYPE record<user>;
      DEFINE FIELD material ON source_part TYPE record<material>;
      DEFINE FIELD idx ON source_part TYPE int;
      DEFINE FIELD data ON source_part TYPE bytes;
      DEFINE TABLE asset SCHEMAFULL;
      DEFINE FIELD owner ON asset TYPE record<user>;
      DEFINE FIELD material ON asset TYPE record<material>;
      DEFINE FIELD mime ON asset TYPE string;
      DEFINE FIELD data ON asset TYPE bytes;
      DEFINE FIELD created_at ON asset TYPE datetime DEFAULT time::now();
      CREATE source_part:legacy CONTENT {owner:user:u1,material:material:legacy,idx:0,data:encoding::base64::decode($pdf)};
      CREATE type::record($asset) CONTENT {owner:user:u1,material:material:legacy,mime:'image/png',data:encoding::base64::decode($image)};`,
      { pdf: bytes('legacy PDF'), asset: `asset:${imageId}`, image: bytes('legacy image') });

    await ensureServerSchema(env);
    // A second cold isolate may apply the schema after legacy rows exist.
    await db.queryAll(await readFile(new URL('../src/server-schema.surql', import.meta.url), 'utf8'));

    await t.test('schema application preserves legacy bytes and legacy-only PDFs load', async () => {
      const stored = await db.query<string>('RETURN encoding::base64::encode(source_part:legacy.data);');
      assert.equal(Buffer.from(stored, 'base64').toString(), 'legacy PDF');
      assert.equal(Buffer.from(await loadPdf(env, 'material:legacy')).toString(), 'legacy PDF');
    });

    await t.test('mixed parts load in order across batches and bucket bytes take precedence', async () => {
      for (let idx = 0; idx < 5; idx++) {
        const id = `source_part:mixed${idx}`;
        await db.query(`CREATE type::record($id) CONTENT {
          owner:user:u1, material:material:mixed, idx:$idx,
          data: IF $idx % 2 = 0 { encoding::base64::decode($legacy) } ELSE { NONE }
        };
        IF $idx > 0 { type::file('materials', '/sources/' + <string>record::id(type::record($id))).put(encoding::base64::decode($part)); };`,
        { id, idx, legacy: bytes(idx === 0 ? 'part 0;' : 'stale legacy'), part: bytes(`part ${idx};`) });
      }
      assert.equal(Buffer.from(await loadPdf(env, 'material:mixed')).toString(), 'part 0;part 1;part 2;part 3;part 4;');
    });

    await t.test('missing bytes and missing part indexes give an actionable user error', async () => {
      await db.query(`CREATE source_part:empty CONTENT {owner:user:u1,material:material:empty,idx:0};
        CREATE source_part:gap0 CONTENT {owner:user:u1,material:material:gap,idx:0,data:encoding::base64::decode($part)};
        CREATE source_part:gap2 CONTENT {owner:user:u1,material:material:gap,idx:2,data:encoding::base64::decode($part)};`,
      { part: bytes('part') });
      for (const material of ['empty', 'gap', 'absent']) {
        await assert.rejects(loadPdf(env, `material:${material}`), (error: unknown) =>
          error instanceof UserError && /uploaded PDF.*missing.*Upload it again/i.test(error.message));
      }
    });

    await t.test('image HTTP reads prefer the file and fall back only when it is missing', async () => {
      let response = await getImage();
      assert.equal(response.status, 200);
      assert.equal(response.headers.get('content-type'), 'image/png');
      assert.equal(await response.text(), 'legacy image');
      await db.query(`type::file('materials', '/assets/' + <string>record::id(type::record($id))).put(encoding::base64::decode($image));`,
        { id: `asset:${imageId}`, image: bytes('bucket image') });
      response = await getImage();
      assert.equal(response.status, 200);
      assert.equal(await response.text(), 'bucket image');
      await db.query('UPDATE type::record($id) SET data = NONE;', { id: `asset:${imageId}` });
      response = await getImage();
      assert.equal(response.status, 200);
      assert.equal(await response.text(), 'bucket image');
      await db.query(`CREATE type::record($id) CONTENT {owner:user:u1,material:material:legacy,mime:'image/png'};`,
        { id: `asset:${missingImageId}` });
      assert.equal((await getImage(missingImageId)).status, 404);
      assert.equal((await getImage(`k${crypto.randomUUID().replaceAll('-', '')}`)).status, 404);
    });

    await t.test('bucket configuration failures are not hidden by legacy fallback', async () => {
      await db.query('REMOVE BUCKET materials;');
      await assert.rejects(loadPdf(env, 'material:legacy'), (error: unknown) =>
        error instanceof Error && !(error instanceof UserError) && /bucket/i.test(error.message));
    });
  } finally {
    await db.query(`REMOVE NAMESPACE ${namespace};`);
    hook.deregister();
  }
});
