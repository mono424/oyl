import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';
import type { Env } from '../src/env.ts';
import { ensureServerTable } from '../src/service.ts';

const env = { SPKY_DB_URL: 'http://db.example', SPKY_DB_NS: 'n', SPKY_DB_NAME: 'd', SPKY_DB_USER: 'root', SPKY_DB_PASS: 'root' } as Env;
const realFetch = globalThis.fetch;
afterEach(() => { globalThis.fetch = realFetch; });

/** A database holding `tables`; answers the statements it was sent. */
function database(tables: Record<string, string>) {
  const sent: string[] = [];
  globalThis.fetch = (async (_url: string, init: RequestInit) => {
    const [sql] = JSON.parse(String(init.body)).params as [string];
    sent.push(sql);
    return new Response(JSON.stringify({ result: [{ status: 'OK', result: tables }] }));
  }) as typeof fetch;
  return sent;
}

// In this order: applying the schema makes every later call in the isolate return at once.
test('a table that exists skips the server schema, and is asked for once per isolate', async () => {
  const sent = database({ answer_image: 'DEFINE TABLE answer_image' });
  await ensureServerTable(env, 'answer_image');
  await ensureServerTable(env, 'answer_image');
  assert.deepEqual(sent, ['RETURN (INFO FOR DB).tables;']);
});

test('a missing table applies the server schema', async () => {
  const sent = database({});
  await ensureServerTable(env, 'site_file');
  assert.equal(sent.length, 2);
  assert.match(sent[1], /DEFINE TABLE/);
});
