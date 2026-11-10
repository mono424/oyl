import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { Env } from '../src/env.ts';
import { ensureServerSchema } from '../src/service.ts';
import { Surreal } from '../src/surreal.ts';

test('concurrent cold requests finish server schema initialization', {
  skip: !process.env.BOLO_TEST_SURREAL,
}, async () => {
  const namespace = `bolo_cold_${crypto.randomUUID().replaceAll('-', '')}`;
  const env = {
    SPKY_DB_URL: 'http://127.0.0.1:8666', SPKY_DB_NS: namespace, SPKY_DB_NAME: 'main',
    SPKY_DB_USER: 'root', SPKY_DB_PASS: 'root',
  } as Env;
  const db = Surreal.system(env);
  try {
    await db.query(`DEFINE NAMESPACE ${namespace}; DEFINE DATABASE main;`);
    await Promise.all(Array.from({ length: 6 }, () => ensureServerSchema(env)));
    const info = await db.query<{ accesses: Record<string, string>; tables: Record<string, string> }>('INFO FOR DB');
    assert.ok(info.accesses.oauth_agent);
    assert.ok(info.tables.oauth_request);
    assert.ok(info.tables.chunk);
  } finally {
    await db.query(`REMOVE NAMESPACE ${namespace};`);
  }
});
