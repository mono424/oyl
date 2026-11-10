import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { Env } from '../src/env.ts';
import { Surreal, SurrealAuthError, signinRecord } from '../src/surreal.ts';

test('Surreal JSON-RPC preserves arbitrary parameter strings and explicit record casts', {
  skip: !process.env.BOLO_TEST_SURREAL,
}, async () => {
  const namespace = `bolo_transport_${crypto.randomUUID().replaceAll('-', '')}`;
  const db = Surreal.system({
    SPKY_DB_URL: 'http://127.0.0.1:8666', SPKY_DB_NS: namespace, SPKY_DB_NAME: 'main',
    SPKY_DB_USER: 'root', SPKY_DB_PASS: 'root',
  } as Env);
  const strings = [
    'Cryptography: compact oral-exam definitions',
    'Ciphertext: (c1, c2). Preserve every word after the colon.',
    'Source: 01 source notes with more text',
    'material:k1',
    'Unicode: äöü 🐿️ 中文\nSecond line\tand tab',
    'Quotes: \'single\' and "double"; \\ backslash',
    "'; DELETE example; --",
    'NONE', 'NULL', '2026-10-21T17:00:00Z',
  ];
  try {
    await db.query(`DEFINE NAMESPACE ${namespace}; DEFINE DATABASE main;`);
    await db.query(`
      DEFINE TABLE example SCHEMAFULL;
      DEFINE FIELD text ON example TYPE string;
      DEFINE FIELD heading ON example TYPE option<string>;
      DEFINE FIELD owner ON example TYPE record<user>;
    `);
    const rows = strings.map((text, position) => ({
      id: `example:k${position}`, text, heading: position % 2 ? text : null,
    }));
    await db.query(`FOR $row IN $rows {
      CREATE type::record($row.id) CONTENT {
        text: $row.text, heading: $row.heading, owner: type::record($owner)
      };
    };`, { rows, owner: 'user:u1' });
    const stored = await db.query<{ text: string; heading?: string; owner_type: string }[]>(
      'SELECT id, text, heading, type::of(owner) AS owner_type FROM example ORDER BY id',
    );
    assert.deepEqual(stored.map((row) => row.text), strings);
    assert.ok(stored.every((row) => row.owner_type === 'record'));
    assert.ok(stored.every((row, index) => index % 2 ? row.heading === strings[index] : row.heading == null));

    assert.deepEqual(await db.queryAll(
      'RETURN $first; RETURN $second; RETURN $missing = NONE; RETURN $nested.omitted = NONE;',
      { first: strings[0], second: [strings[2], 12, false], missing: null, nested: { omitted: null } },
    ), [strings[0], [strings[2], 12, false], true, true]);
    assert.equal(await db.query('RETURN $first; RETURN $second;', { first: 'earlier', second: 'last' }), 'last');
    assert.deepEqual(await db.queryAll('RETURN 1; RETURN 2;'), [1, 2]);
    assert.equal(await db.query('RETURN $__surreal_transport;', { __surreal_transport: strings[0] }), strings[0]);
    await assert.rejects(db.query('THROW $error;', { error: 'Failure: full message survives.' }), /Failure: full message survives\./);
  } finally {
    await db.query(`REMOVE NAMESPACE ${namespace};`);
  }
});

test('Surreal preserves authentication and RPC errors', async (t) => {
  const db = Surreal.system({
    SPKY_DB_URL: 'http://surreal.example', SPKY_DB_NS: 'main', SPKY_DB_NAME: 'main', SPKY_DB_USER: 'root', SPKY_DB_PASS: 'root',
  } as Env);
  let response = () => new Response('unauthorized', { status: 401 });
  t.mock.method(globalThis, 'fetch', async () => response());
  await assert.rejects(db.query('RETURN $value;', { value: 'Source: 01' }), SurrealAuthError);
  response = () => Response.json({ error: { message: 'expired token' } });
  await assert.rejects(db.query('RETURN 1;'), SurrealAuthError);
  response = () => Response.json({ error: { message: 'query rejected' } });
  await assert.rejects(db.query('RETURN 1;'), /SurrealDB: query rejected/);
});

test('Surreal signs in with the login sp00ky injects, or falls back to SURREAL_*', async (t) => {
  const calls: { url: string; headers: Record<string, string>; body: unknown }[] = [];
  t.mock.method(globalThis, 'fetch', async (url: string | URL | Request, init?: RequestInit) => {
    calls.push({ url: String(url), headers: init?.headers as Record<string, string>, body: JSON.parse(String(init?.body)) });
    return Response.json(String(url).endsWith('/signin') ? { token: 't' } : { result: [{ status: 'OK', result: 1 }] });
  });
  const basic = (user: string, pass: string) => `Basic ${btoa(`${user}:${pass}`)}`;
  const last = () => calls[calls.length - 1];

  // Shared free database: the project's namespace owner, signed in at the namespace.
  const cloud = {
    SPKY_DB_URL: 'https://surreal.example', SPKY_DB_NS: 'proj', SPKY_DB_NAME: 'main',
    SPKY_DB_USER: 'owner', SPKY_DB_PASS: 'p1', SPKY_DB_LEVEL: 'namespace',
    // Left over from before: never mixed into the platform login.
    SURREAL_URL: 'https://old.example', SURREAL_USER: 'old', SURREAL_PASS: 'old', SURREAL_AUTH_NS: 'old',
  } as Env;
  await Surreal.system(cloud).query('RETURN 1;');
  assert.equal(last().url, 'https://surreal.example/rpc');
  assert.equal(last().headers.authorization, basic('owner', 'p1'));
  assert.equal(last().headers['surreal-ns'], 'proj');
  assert.equal(last().headers['surreal-db'], 'main');
  assert.equal(last().headers['surreal-auth-ns'], 'proj');
  assert.equal(last().headers['surreal-auth-db'], undefined);
  // A user's own token carries its scope: no sign-in level headers.
  await Surreal.as(cloud, 'tok').query('RETURN 1;');
  assert.equal(last().headers.authorization, 'Bearer tok');
  assert.equal(last().headers['surreal-auth-ns'], undefined);
  assert.equal(await signinRecord(cloud, 'agent', { key: 'k' }), 't');
  assert.equal(last().url, 'https://surreal.example/signin');
  assert.deepEqual(last().body, { ns: 'proj', db: 'main', ac: 'agent', key: 'k' });

  // A project's own database: sp00ky sets the coordinates, the vault the login.
  await Surreal.system({
    SPKY_DB_URL: 'https://own.example', SPKY_DB_NS: 'studycall', SPKY_DB_NAME: 'main',
    SURREAL_USER: 'api', SURREAL_PASS: 'p2', SURREAL_AUTH_NS: 'studycall', SURREAL_AUTH_DB: 'main',
  } as Env).query('RETURN 1;');
  assert.equal(last().url, 'https://own.example/rpc');
  assert.equal(last().headers.authorization, basic('api', 'p2'));
  assert.equal(last().headers['surreal-auth-ns'], 'studycall');
  assert.equal(last().headers['surreal-auth-db'], 'main');

  // An older .dev.vars: SURREAL_* only, root login, ws URL with /rpc.
  await Surreal.system({
    SURREAL_URL: 'ws://localhost:8666/rpc', SURREAL_NS: 'main', SURREAL_DB: 'studycall', SURREAL_USER: 'root', SURREAL_PASS: 'root',
  } as Env).query('RETURN 1;');
  assert.equal(last().url, 'http://localhost:8666/rpc');
  assert.equal(last().headers.authorization, basic('root', 'root'));
  assert.equal(last().headers['surreal-ns'], 'main');
  assert.equal(last().headers['surreal-db'], 'studycall');
  assert.equal(last().headers['surreal-auth-ns'], undefined);

  assert.throws(() => Surreal.system({ SPKY_DB_URL: 'https://surreal.example' } as Env), /not configured/);
  assert.throws(() => Surreal.system({ SPKY_DB_URL: 'https://surreal.example', SPKY_DB_NS: 'proj', SPKY_DB_NAME: 'main' } as Env), /login missing/);
});

test('Surreal stores a retained PDF image without exceeding the RPC body limit', {
  skip: !process.env.BOLO_TEST_SURREAL,
}, async (t) => {
  const namespace = `bolo_image_${crypto.randomUUID().replaceAll('-', '')}`;
  const db = Surreal.system({
    SPKY_DB_URL: 'http://127.0.0.1:8666', SPKY_DB_NS: namespace, SPKY_DB_NAME: 'main',
    SPKY_DB_USER: 'root', SPKY_DB_PASS: 'root',
  } as Env);
  // Same byte length as the retained page-two image in the failed import.
  const image = Buffer.alloc(2_493_163, 7).toString('base64');
  const originalFetch = globalThis.fetch;
  t.mock.method(globalThis, 'fetch', async (url: string | URL | Request, init?: RequestInit) => {
    if (typeof init?.body === 'string') {
      assert.ok(new TextEncoder().encode(init.body).byteLength <= 4 * 1024 * 1024, 'RPC request exceeds the 4 MiB body limit');
    }
    return originalFetch(url, init);
  });
  try {
    await db.query(`DEFINE NAMESPACE ${namespace}; DEFINE DATABASE main; DEFINE TABLE image SCHEMAFULL; DEFINE FIELD data ON image TYPE bytes;`);
    await db.query('CREATE image:k1 SET data = encoding::base64::decode($image);', { image });
    const stored = await db.query<string>('RETURN encoding::base64::encode((SELECT VALUE data FROM ONLY image:k1));');
    assert.ok(Buffer.from(stored, 'base64').equals(Buffer.from(image, 'base64')), 'Stored image bytes must remain unchanged');
  } finally {
    await db.query(`REMOVE NAMESPACE ${namespace};`);
  }
});
