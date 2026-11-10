import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import type { Env } from '../src/env.ts';
import { createMaterial } from '../src/material-create.ts';
import { signinRecord, Surreal } from '../src/surreal.ts';

const create = `CREATE type::record($id) CONTENT {
  owner:$auth.id, course:type::record($course), title:'Lecture', kind:'markdown', content:'Study notes'
} RETURN id`;
const failure = (status: number) => new Error(`SurrealDB: _00_material_mutation: Problem with remote HTTP request: ${status} Bad Gateway`);

test('material creation adopts a committed row after the specific sync-hook gateway failures', async () => {
  for (const status of [502, 503, 504]) {
    const error = failure(status);
    const rows = [{ id: 'material:k1' }];
    const calls: { sql: string; vars: unknown }[] = [];
    const user = { query: async (sql: string, vars: unknown) => {
      calls.push({ sql, vars });
      if (sql === create) throw error;
      return rows;
    } } as unknown as Surreal;
    assert.deepEqual(await createMaterial(user, create, { id: 'material:k1', course: 'course:c1' }), rows);
    assert.equal(calls.length, 2);
    assert.equal(calls.filter((call) => call.sql === create).length, 1);
    assert.deepEqual(calls[1].vars, { id: 'material:k1' });
  }
});

test('material reconciliation tolerates delayed visibility and a transient read failure', async () => {
  for (const readFails of [false, true]) {
    const error = failure(502);
    const rows = [{ id: 'material:k1' }];
    let creates = 0;
    let reads = 0;
    const user = { query: async (sql: string) => {
      if (sql === create) { creates++; throw error; }
      reads++;
      if (reads === 1 && readFails) throw new Error('Read temporarily unavailable');
      return reads < 3 ? [] : rows;
    } } as unknown as Surreal;
    assert.deepEqual(await createMaterial(user, create, { id: 'material:k1' }), rows);
    assert.equal(creates, 1);
    assert.equal(reads, 3);
  }
});

test('material creation bounds reconciliation and preserves the original failure with safe diagnostics', async (t) => {
  const warnings: unknown[][] = [];
  t.mock.method(console, 'warn', (...args: unknown[]) => { warnings.push(args); });
  for (const readFails of [false, true]) {
    const error = failure(502);
    let calls = 0;
    const user = { query: async () => {
      calls++;
      if (calls === 1) throw error;
      if (readFails) throw new Error('Read unavailable: private query details');
      return [];
    } } as unknown as Surreal;
    await assert.rejects(createMaterial(user, create, { id: 'material:k1' }), (received) => received === error);
    assert.equal(calls, 5);
  }
  assert.deepEqual(warnings, [
    ['[mcp] material create reconciliation exhausted', { materialId: 'material:k1', outcomes: ['not_visible', 'not_visible', 'not_visible', 'not_visible'] }],
    ['[mcp] material create reconciliation exhausted', { materialId: 'material:k1', outcomes: ['read_failed', 'read_failed', 'read_failed', 'read_failed'] }],
  ]);
});

test('material creation does not reconcile unrelated failures or repeat successful creates', async () => {
  for (const error of [
    failure(500),
    new Error('_00_other_mutation: remote HTTP request: 502 Bad Gateway'),
    new Error('_00_material_mutation: network failed'),
    new Error('remote HTTP request: 502 Bad Gateway'),
  ]) {
    let calls = 0;
    const user = { query: async () => { calls++; throw error; } } as unknown as Surreal;
    await assert.rejects(createMaterial(user, create, { id: 'material:k1' }), (received) => received === error);
    assert.equal(calls, 1);
  }
  let calls = 0;
  const rows = [{ id: 'material:k1' }];
  const user = { query: async () => { calls++; return rows; } } as unknown as Surreal;
  assert.deepEqual(await createMaterial(user, create, { id: 'material:k1' }), rows);
  assert.equal(calls, 1);
});

test('material reconciliation uses the same record session and cannot adopt foreign rows', {
  skip: !process.env.BOLO_TEST_SURREAL,
}, async (t) => {
  const namespace = `bolo_create_${crypto.randomUUID().replaceAll('-', '')}`;
  const env = {
    SPKY_DB_URL: 'http://127.0.0.1:8666', SPKY_DB_NS: namespace, SPKY_DB_NAME: 'main',
    SPKY_DB_USER: 'root', SPKY_DB_PASS: 'root',
  } as Env;
  const db = Surreal.system(env);
  try {
    await db.query(`DEFINE NAMESPACE ${namespace}; DEFINE DATABASE main;`);
    await db.queryAll(await readFile(new URL('../../../schema/src/schema.surql', import.meta.url), 'utf8'));
    await db.query(`
      CREATE user:u1 CONTENT {email:'student@example.test',name:'Student',password:'test'};
      CREATE user:u2 CONTENT {email:'other@example.test',name:'Other',password:'test'};
      CREATE course:c1 CONTENT {owner:user:u1,title:'My course'};
      CREATE course:c2 CONTENT {owner:user:u2,title:'Other course'};
      CREATE api_key:k1 CONTENT {owner:user:u1,name:'Test',prefix:'sc_test',key_hash:crypto::sha256('sc_test')};
      CREATE material:foreign CONTENT {owner:user:u2,course:course:c2,title:'Private',kind:'markdown'};
    `);
    const token = await signinRecord(env, 'agent', { key: 'sc_test' });
    assert.ok(token);
    const user = Surreal.as(env, token);
    const originalQuery = user.query.bind(user);
    const error = failure(502);
    let commit = true;
    let creates = 0;
    t.mock.method(user, 'query', async (sql: string, vars: Record<string, unknown>) => {
      if (sql === create) {
        creates++;
        if (commit) await originalQuery(sql, vars);
        throw error;
      }
      return originalQuery(sql, vars);
    });
    assert.deepEqual(await createMaterial(user, create, { id: 'material:owned', course: 'course:c1' }), [{ id: 'material:owned' }]);
    commit = false;
    for (const id of ['material:foreign', 'material:missing']) {
      await assert.rejects(createMaterial(user, create, { id, course: 'course:c1' }), (received) => received === error);
    }
    assert.equal(creates, 3);
    assert.equal(await db.query('RETURN count(SELECT id FROM material);'), 2);
  } finally {
    await db.query(`REMOVE NAMESPACE ${namespace};`);
  }
});
