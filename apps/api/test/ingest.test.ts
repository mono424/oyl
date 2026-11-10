import assert from 'node:assert/strict';
import { registerHooks } from 'node:module';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import type { Env } from '../src/env.ts';
import { Surreal } from '../src/surreal.ts';

test('ingestion replays a committed embed step without losing chunk timestamps', {
  skip: !process.env.BOLO_TEST_SURREAL,
}, async () => {
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
  const { IngestMaterial } = await import('../src/ingest.ts');
  const namespace = `bolo_ingest_${crypto.randomUUID().replaceAll('-', '')}`;
  const vector = Array.from({ length: 1024 }, (_, index) => index === 0 ? 1 : 0);
  const env = {
    SPKY_DB_URL: 'http://127.0.0.1:8666', SPKY_DB_NS: namespace, SPKY_DB_NAME: 'main',
    SPKY_DB_USER: 'root', SPKY_DB_PASS: 'root', EMBEDDING_MODEL: 'test',
    AI: { run: async (_model: string, input: { text: string[] }) => ({ data: input.text.map(() => vector) }) },
  } as unknown as Env;
  const db = Surreal.system(env);
  let createdAt: string | undefined;
  let replayed = false;
  try {
    await db.query(`DEFINE NAMESPACE ${namespace}; DEFINE DATABASE main;`);
    await db.queryAll(await readFile(new URL('../../../schema/src/schema.surql', import.meta.url), 'utf8'));
    await db.query(`CREATE material:k1 CONTENT {
      owner:user:u1, course:course:c1, title:'Lecture', kind:'markdown', index_rev:1, status:'processing',
      content:'# Introduction\n\nA sufficiently long section about cryptographic definitions and reductions.'
    };`);
    const step = {
      async do(name: string, config: unknown, callback?: () => Promise<unknown>) {
        const run = typeof config === 'function' ? config as () => Promise<unknown> : callback!;
        const result = await run();
        if (name.startsWith('embed ') && !replayed) {
          replayed = true;
          createdAt = await db.query<string>('RETURN (SELECT VALUE created_at FROM ONLY chunk:k1_1_0);');
          // A write can commit before a Workflow records the step's success.
          return run();
        }
        return result;
      },
    };
    const workflow = new IngestMaterial({} as never, env);
    const result = await workflow.run({ payload: { materialId: 'material:k1', rev: 1, convert: false } } as never, step as never);
    assert.deepEqual(result, { ok: true, chunks: 1 });
    assert.equal(replayed, true);
    assert.equal(typeof createdAt, 'string');
    assert.equal(await db.query('RETURN (SELECT VALUE created_at FROM ONLY chunk:k1_1_0);'), createdAt);
    assert.equal(await db.query('RETURN count(SELECT id FROM chunk);'), 1);
    assert.equal(await db.query('RETURN (SELECT VALUE status FROM ONLY material:k1);'), 'ready');
  } finally {
    await db.query(`REMOVE NAMESPACE ${namespace};`);
    hook.deregister();
  }
});
