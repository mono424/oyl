import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import type { Env } from '../src/env.ts';
import { jobs } from '../src/jobs.ts';
import { Surreal } from '../src/surreal.ts';

const post = (env: Partial<Env>, authorization?: string) =>
  jobs.request('/materials/index', {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...(authorization ? { authorization } : {}) },
    body: JSON.stringify({ id: 'material:kabc' }),
  }, env as Env);

test('job routes refuse requests without the job token', async () => {
  assert.equal((await post({ JOB_TOKEN: 'right' })).status, 401);
  assert.equal((await post({ JOB_TOKEN: 'right' }, 'Bearer wrong')).status, 401);
  // No token configured refuses everything, even an empty bearer match.
  assert.equal((await post({}, 'Bearer ')).status, 401);
  assert.equal((await post({}, 'Bearer undefined')).status, 401);
});

test('the job outbox only takes jobs about the student\'s own records', {
  skip: !process.env.BOLO_TEST_SURREAL,
}, async () => {
  const namespace = `bolo_jobs_${crypto.randomUUID().replaceAll('-', '')}`;
  const env = {
    SPKY_DB_URL: 'http://127.0.0.1:8666', SPKY_DB_NS: namespace, SPKY_DB_NAME: 'main',
    SPKY_DB_USER: 'root', SPKY_DB_PASS: 'root',
  } as Env;
  const root = Surreal.system(env);
  const schema = (file: string) => readFileSync(new URL(`../../../schema/src/${file}`, import.meta.url), 'utf8');
  const signup = async (email: string) => {
    const res = await fetch(`${env.SPKY_DB_URL}/signup`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', accept: 'application/json' },
      body: JSON.stringify({ ns: namespace, db: 'main', ac: 'account', email, name: 'Student', password: 'pw-123456' }),
    });
    assert.ok(res.ok, await res.clone().text());
    const token = ((await res.json()) as { token: string }).token;
    const user = Surreal.as(env, token);
    return { user, id: await user.query<string>('RETURN $auth.id') };
  };
  const job = (assigned: string, payload: unknown) => ({
    id: `job:k${crypto.randomUUID().replaceAll('-', '')}`, assigned, payload,
  });
  const create = 'CREATE type::record($id) CONTENT { path: "/jobs/materials/index", assigned_to: type::record($assigned), payload: $payload } RETURN id';
  try {
    await root.query(`DEFINE NAMESPACE ${namespace}; DEFINE DATABASE main;`);
    await root.queryAll(schema('schema.surql'));
    await root.queryAll(schema('outbox/api.surql'));
    const a = await signup('a@example.com');
    const b = await signup('b@example.com');
    const material = async (owner: { user: Surreal; id: string }, key: string) => {
      await owner.user.query(
        `CREATE type::record($course) CONTENT { owner: $auth.id, title: 'Course' };
         CREATE type::record($material) CONTENT { owner: $auth.id, course: type::record($course), title: 'Lecture', kind: 'text' };`,
        { course: `course:${key}`, material: `material:${key}` },
      );
      return `material:${key}`;
    };
    const mine = await material(a, 'ka');
    const theirs = await material(b, 'kb');

    const ok = job(mine, { id: mine });
    assert.equal((await a.user.query<{ id: string }[]>(create, ok)).length, 1);
    // Somebody else's material, as the target or only in the payload.
    // A refused CREATE creates nothing (SurrealDB answers it with no rows).
    const refused = async (j: ReturnType<typeof job>) => assert.deepEqual(await a.user.query(create, j), []);
    // Somebody else's material, as the target or only in the payload.
    await refused(job(theirs, { id: theirs }));
    await refused(job(mine, { id: theirs }));
    // A JSON string payload (`db.run`) has no readable id.
    await refused(job(mine, JSON.stringify({ id: mine })));
    assert.deepEqual(await root.query<string[]>('SELECT VALUE <string> id FROM job'), [ok.id]);
    // Jobs are visible to their owner only, and nobody but root changes them.
    assert.equal((await b.user.query<unknown[]>('SELECT id FROM job')).length, 0);
    assert.equal((await a.user.query<unknown[]>('SELECT id FROM job')).length, 1);
    await a.user.query(`UPDATE type::record($id) SET status = 'success'`, { id: ok.id });
    assert.equal(await root.query<string>('RETURN (SELECT VALUE status FROM ONLY type::record($id))', { id: ok.id }), 'pending');
  } finally {
    await root.query(`REMOVE NAMESPACE ${namespace};`);
  }
});
