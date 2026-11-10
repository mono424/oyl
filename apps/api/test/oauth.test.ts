import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import type { Env } from '../src/env.ts';
import { approveOAuthConsent, getOAuthConsent, handleOAuth, verifyOAuthAccessToken } from '../src/oauth.ts';
import { handleMcp } from '../src/mcp.ts';
import { Surreal } from '../src/surreal.ts';

const origin = 'https://bolo.example';
const resource = `${origin}/mcp`;
const redirect = 'https://assistant.example/callback';
const verifier = 'a'.repeat(64);
const challenge = btoa(String.fromCharCode(...new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(verifier)))))
  .replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '');

function post(path: string, values: Record<string, unknown>, json = false) {
  return new Request(`${origin}${path}`, {
    method: 'POST', headers: { 'content-type': json ? 'application/json' : 'application/x-www-form-urlencoded' },
    body: json ? JSON.stringify(values) : new URLSearchParams(values as Record<string, string>),
  });
}

test('OAuth discovery is public, path-aware and advertised by the MCP challenge', async () => {
  const env = { API_ORIGIN: origin, APP_ORIGIN: origin } as Env;
  const protectedResource = await handleOAuth(new Request(`${origin}/.well-known/oauth-protected-resource/mcp`), env);
  assert.equal(protectedResource?.status, 200);
  assert.deepEqual((await protectedResource!.json() as {authorization_servers: string[]}).authorization_servers, [origin]);
  const server = await handleOAuth(new Request(`${origin}/.well-known/oauth-authorization-server`), env);
  const info = await server!.json() as {
    code_challenge_methods_supported: string[]; scopes_supported: string[];
    client_id_metadata_document_supported: boolean; authorization_response_iss_parameter_supported: boolean;
  };
  assert.deepEqual(info.code_challenge_methods_supported, ['S256']);
  assert.deepEqual(info.scopes_supported, ['courses']);
  assert.equal(info.client_id_metadata_document_supported, true);
  assert.equal(info.authorization_response_iss_parameter_supported, true);
  const unauthorized = await handleMcp(new Request(resource), env);
  assert.equal(unauthorized.status, 401);
  assert.ok(unauthorized.headers.get('www-authenticate')?.includes(`${origin}/.well-known/oauth-protected-resource/mcp`));
});

test('OAuth end-to-end grants preserve ownership and reject unsafe redemption and rotation', {
  skip: !process.env.BOLO_TEST_SURREAL,
}, async (t) => {
  const namespace = `bolo_oauth_${crypto.randomUUID().replaceAll('-', '')}`;
  const env = {
    SPKY_DB_URL: 'http://127.0.0.1:8666', SPKY_DB_NS: namespace, SPKY_DB_NAME: 'main', SPKY_DB_USER: 'root', SPKY_DB_PASS: 'root',
    API_ORIGIN: origin, APP_ORIGIN: origin,
  } as Env;
  const db = Surreal.system(env);
  async function reply(request: Request) {
    const result = await handleOAuth(request, env);
    assert.ok(result);
    return { response: result, data: result.headers.get('content-type')?.includes('json') ? await result.json() as Record<string, unknown> : {} };
  }
  let clientId = '';
  async function pending(extra: Record<string, string> = {}) {
    const params = new URLSearchParams({
      client_id: clientId, redirect_uri: redirect, response_type: 'code', code_challenge: challenge,
      code_challenge_method: 'S256', resource, scope: 'courses', state: 'keep-me', ...extra,
    });
    return reply(new Request(`${origin}/oauth/authorize?${params}`));
  }
  async function authorizeCode() {
    const result = await pending();
    assert.equal(result.response.status, 302);
    const requestId = new URL(result.response.headers.get('location')!).searchParams.get('request')!;
    const consent = await getOAuthConsent(env, 'user:u1', requestId);
    assert.equal(consent.clientName, 'Test assistant');
    assert.equal(consent.redirectHost, 'assistant.example');
    const approved = await approveOAuthConsent(env, 'user:u1', { requestId, allow: true });
    const callback = new URL(approved.redirectUrl);
    assert.equal(callback.searchParams.get('state'), 'keep-me');
    assert.equal(callback.searchParams.get('iss'), origin);
    await assert.rejects(approveOAuthConsent(env, 'user:u1', { requestId, allow: true }), /already|expired/);
    return callback.searchParams.get('code')!;
  }
  const exchange = (code: string, extra: Record<string, string> = {}) => reply(post('/oauth/token', {
    client_id: clientId, grant_type: 'authorization_code', code, code_verifier: verifier, redirect_uri: redirect, resource, ...extra,
  }));
  const rotate = (refresh: string, extra: Record<string, string> = {}) => reply(post('/oauth/token', {
    client_id: clientId, grant_type: 'refresh_token', refresh_token: refresh, resource, ...extra,
  }));
  try {
    await db.query(`DEFINE NAMESPACE ${namespace}; DEFINE DATABASE main;`);
    await db.queryAll(await readFile(new URL('../../../schema/src/schema.surql', import.meta.url), 'utf8'));
    await db.queryAll(await readFile(new URL('../src/server-schema.surql', import.meta.url), 'utf8'));
    await db.queryAll(await readFile(new URL('../src/server-schema.surql', import.meta.url), 'utf8'));
    await db.query(`CREATE user:u1 CONTENT {email:'student@example.test',name:'Student',password:'test'};
      CREATE user:u2 CONTENT {email:'other@example.test',name:'Other',password:'test'};
      CREATE course:c1 CONTENT {owner:user:u1,title:'My course'};
      CREATE course:c2 CONTENT {owner:user:u2,title:'Private course'};`);

    await t.test('registration accepts public clients and refuses unsafe callback URLs and methods', async () => {
      for (const uri of ['http://evil.example/callback', 'https://assistant.example/callback#fragment', 'https://user:pass@assistant.example/callback', 'javascript:alert(1)']) {
        const rejected = await reply(post('/oauth/register', { redirect_uris: [uri] }, true));
        assert.equal(rejected.response.status, 400);
      }
      const confidential = await reply(post('/oauth/register', { redirect_uris: [redirect], token_endpoint_auth_method: 'client_secret_post' }, true));
      assert.equal(confidential.response.status, 400);
      const oversized = await reply(post('/oauth/register', { redirect_uris: [redirect], client_name: 'x'.repeat(20_000) }, true));
      assert.equal(oversized.data.error, 'invalid_request');
      const registered = await reply(post('/oauth/register', { redirect_uris: [redirect], client_name: 'Test assistant' }, true));
      assert.equal(registered.response.status, 201);
      clientId = registered.data.client_id as string;
      for (const name of [crypto.randomUUID().toUpperCase(), '2026-10-21T09:15:00+02:00', 'user:example']) {
        const opaqueName = await reply(post('/oauth/register', { redirect_uris: [redirect], client_name: name }, true));
        assert.equal(opaqueName.response.status, 201);
        assert.equal(await db.query('RETURN (SELECT VALUE client_name FROM oauth_client WHERE client_id = $client LIMIT 1)[0]', {
          client: opaqueName.data.client_id,
        }), name);
      }
    });

    await t.test('authorization requires an exact callback, S256 and the correct resource', async () => {
      for (const extra of [{ redirect_uri: `${redirect}/elsewhere` }, { code_challenge_method: 'plain' }, { resource: 'https://other.example/mcp' }, { scope: 'admin' }]) {
        const rejected = await pending(extra);
        assert.equal(rejected.response.status, 400);
        assert.equal(rejected.response.headers.has('location'), false);
      }
      const result = await pending();
      const id = new URL(result.response.headers.get('location')!).searchParams.get('request')!;
      const denied = await approveOAuthConsent(env, 'user:u1', { requestId: id, allow: false });
      assert.equal(new URL(denied.redirectUrl).searchParams.get('error'), 'access_denied');
      assert.equal(new URL(denied.redirectUrl).searchParams.get('state'), 'keep-me');
      assert.equal(new URL(denied.redirectUrl).searchParams.get('iss'), origin, 'RFC 9207: errors carry the issuer too.');
      const expired = await pending();
      const expiredId = new URL(expired.response.headers.get('location')!).searchParams.get('request')!;
      await db.query('UPDATE type::record($id) SET expires_at = time::now() - 1s', { id: `oauth_request:${expiredId}` });
      await assert.rejects(getOAuthConsent(env, 'user:u1', expiredId), /expired/);
      await assert.rejects(approveOAuthConsent(env, 'user:u1', { requestId: expiredId, allow: true }), /expired/);
    });

    await t.test('opaque OAuth state preserves UUIDs, datetimes and Unicode exactly', async () => {
      for (const state of [crypto.randomUUID().toUpperCase(), '2026-10-21T09:15:00+02:00', 'user:example', 's:keep + / = and Grüße 🐿️']) {
        const result = await pending({ state });
        assert.equal(result.response.status, 302);
        const id = new URL(result.response.headers.get('location')!).searchParams.get('request')!;
        const declined = await approveOAuthConsent(env, 'user:u1', { requestId: id, allow: false });
        assert.equal(new URL(declined.redirectUrl).searchParams.get('state'), state);
      }
    });

    let access = '';
    let refresh = '';
    await t.test('PKCE, redirect, resource and single-use code binding are enforced', async () => {
      const code = await authorizeCode();
      for (const extra of [{ code_verifier: 'b'.repeat(64) }, { redirect_uri: `${redirect}/different` }, { resource: 'https://other.example/mcp' }]) {
        const rejected = await exchange(code, extra);
        assert.equal(rejected.response.status, 400);
      }
      const redeemed = await exchange(code);
      assert.equal(redeemed.response.status, 200);
      assert.equal(redeemed.response.headers.get('cache-control'), 'no-store');
      access = redeemed.data.access_token as string;
      refresh = redeemed.data.refresh_token as string;
      assert.equal((await exchange(code)).data.error, 'invalid_grant');
      const auth = await verifyOAuthAccessToken(env, access, resource);
      assert.ok(auth);
      assert.equal(auth.extra?.userId, 'user:u1');
      assert.equal(auth.resource?.href, resource);
      assert.deepEqual(auth.scopes, ['courses']);
      const courses = await Surreal.as(env, auth.extra!.dbToken as string).query<{id: string}[]>('SELECT id FROM course');
      assert.deepEqual(courses.map((course) => course.id), ['course:c1']);
      await db.queryAll(await readFile(new URL('../src/server-schema.surql', import.meta.url), 'utf8'));
      assert.equal(await Surreal.as(env, auth.extra!.dbToken as string).query<string>('RETURN $auth.id'), 'user:u1', 'Another Worker isolate must not replace active session signing keys.');
      assert.equal(await verifyOAuthAccessToken(env, access, 'https://other.example/mcp'), null);
      const secrets = await Surreal.as(env, auth.extra!.dbToken as string).query('SELECT * FROM oauth_access');
      assert.deepEqual(secrets, []);
    });

    await t.test('refresh rotates both credentials; reuse revokes the token family', async () => {
      assert.equal((await rotate(refresh, { resource: 'https://other.example/mcp' })).response.status, 400);
      const renewed = await rotate(refresh);
      assert.equal(renewed.response.status, 200);
      const nextAccess = renewed.data.access_token as string;
      const nextRefresh = renewed.data.refresh_token as string;
      assert.notEqual(nextRefresh, refresh);
      assert.equal(await verifyOAuthAccessToken(env, access, resource), null);
      assert.ok(await verifyOAuthAccessToken(env, nextAccess, resource));
      const replay = await rotate(refresh);
      assert.equal(replay.data.error, 'invalid_grant');
      assert.equal(await verifyOAuthAccessToken(env, nextAccess, resource), null);
      assert.equal((await rotate(nextRefresh)).data.error, 'invalid_grant');
    });

    await t.test('disconnecting the visible connection immediately invalidates access and refresh', async () => {
      const redeemed = await exchange(await authorizeCode());
      const key = await db.query<{id: string}[]>('SELECT id,created_at FROM api_key ORDER BY created_at DESC LIMIT 1');
      await db.query('UPDATE type::record($id) SET revoked_at = time::now()', { id: key[0].id });
      assert.equal(await verifyOAuthAccessToken(env, redeemed.data.access_token as string, resource), null);
      assert.equal((await rotate(redeemed.data.refresh_token as string)).data.error, 'invalid_grant');
      const removed = await exchange(await authorizeCode());
      const removable = await db.query<{id: string}[]>('SELECT id,created_at FROM api_key ORDER BY created_at DESC LIMIT 1');
      await db.query('DELETE type::record($id)', { id: removable[0].id });
      assert.equal(await verifyOAuthAccessToken(env, removed.data.access_token as string, resource), null);
      assert.equal((await rotate(removed.data.refresh_token as string)).data.error, 'invalid_grant');
    });

    await t.test('the revocation endpoint revokes the whole grant and treats unknown tokens equally', async () => {
      const redeemed = await exchange(await authorizeCode());
      const revoked = await reply(post('/oauth/revoke', { client_id: clientId, token: redeemed.data.refresh_token }));
      assert.equal(revoked.response.status, 200);
      assert.equal(await verifyOAuthAccessToken(env, redeemed.data.access_token as string, resource), null);
      assert.equal((await reply(post('/oauth/revoke', { client_id: clientId, token: 'unknown-token' }))).response.status, 200);
    });

    await t.test('authorization codes, access and refresh tokens expire and remain client-bound', async () => {
      const code = await authorizeCode();
      const secondClient = await reply(post('/oauth/register', { redirect_uris: [redirect], client_name: 'Another assistant' }, true));
      assert.equal((await exchange(code, { client_id: secondClient.data.client_id as string })).data.error, 'invalid_grant');
      const redeemed = await exchange(code);
      const currentAccess = redeemed.data.access_token as string;
      const currentRefresh = redeemed.data.refresh_token as string;
      assert.equal((await rotate(currentRefresh, { client_id: secondClient.data.client_id as string })).data.error, 'invalid_grant');
      assert.ok(await verifyOAuthAccessToken(env, currentAccess, resource), 'An unrelated client cannot invalidate the grant.');
      await db.query('UPDATE oauth_access SET expires_at = time::now() - 1s');
      assert.equal(await verifyOAuthAccessToken(env, currentAccess, resource), null);
      await db.query('UPDATE oauth_refresh SET expires_at = time::now() - 1s');
      assert.equal((await rotate(currentRefresh)).data.error, 'invalid_grant');
      const expiredCode = await authorizeCode();
      await db.query('UPDATE oauth_code SET expires_at = time::now() - 1s');
      assert.equal((await exchange(expiredCode)).data.error, 'invalid_grant');
    });

    await t.test('concurrent redemption can issue credentials only once', async () => {
      const code = await authorizeCode();
      const results = await Promise.all([exchange(code), exchange(code)]);
      assert.deepEqual(results.map((result) => result.response.status).sort(), [200, 400]);
      const successful = results.find((result) => result.response.status === 200)!;
      const renewed = await Promise.all([rotate(successful.data.refresh_token as string), rotate(successful.data.refresh_token as string)]);
      assert.deepEqual(renewed.map((result) => result.response.status).sort(), [200, 400]);
      const winner = renewed.find((result) => result.response.status === 200)!;
      assert.equal(await verifyOAuthAccessToken(env, winner.data.access_token as string, resource), null, 'Refresh reuse also revokes the winning token.');
    });
  } finally {
    await db.query(`REMOVE NAMESPACE ${namespace};`);
  }
});

test('clients identified by a metadata document (CIMD) connect without registering', {
  skip: !process.env.BOLO_TEST_SURREAL,
}, async (t) => {
  const namespace = `bolo_cimd_${crypto.randomUUID().replaceAll('-', '')}`;
  const env = {
    SPKY_DB_URL: 'http://127.0.0.1:8666', SPKY_DB_NS: namespace, SPKY_DB_NAME: 'main', SPKY_DB_USER: 'root', SPKY_DB_PASS: 'root',
    API_ORIGIN: origin, APP_ORIGIN: origin,
  } as Env;
  const db = Surreal.system(env);
  const clientUrl = 'https://chat.example/oauth/client.json';
  const callback = 'https://chat.example/connector_platform_oauth_redirect';
  const documents = new Map<string, unknown>([
    [clientUrl, {
      client_id: clientUrl, client_name: 'Chat Example', redirect_uris: [callback],
      token_endpoint_auth_method: 'private_key_jwt', token_endpoint_auth_methods_supported: ['none', 'private_key_jwt'],
      grant_types: ['authorization_code', 'refresh_token'], response_types: ['code'],
    }],
    ['https://chat.example/other.json', { client_id: clientUrl, redirect_uris: [callback] }],
    ['https://chat.example/secret.json', { client_id: 'https://chat.example/secret.json', redirect_uris: [callback], token_endpoint_auth_method: 'client_secret_basic' }],
  ]);
  const original = globalThis.fetch;
  const fetched: string[] = [];
  t.mock.method(globalThis, 'fetch', async (url: string | URL | Request, init?: RequestInit) => {
    const href = url instanceof Request ? url.url : String(url);
    if (!href.startsWith('https://chat.example/')) return original(url, init);
    fetched.push(href);
    assert.equal(init?.redirect, 'manual', 'Workers reject redirect: error.');
    return documents.has(href) ? Response.json(documents.get(href)) : new Response('missing', { status: 404 });
  });
  async function reply(request: Request) {
    const result = await handleOAuth(request, env);
    assert.ok(result);
    return { response: result, data: result.headers.get('content-type')?.includes('json') ? await result.json() as Record<string, unknown> : {} };
  }
  const authorize = (extra: Record<string, string> = {}) => reply(new Request(`${origin}/oauth/authorize?${new URLSearchParams({
    client_id: clientUrl, redirect_uri: callback, response_type: 'code', code_challenge: challenge, code_challenge_method: 'S256',
    resource, scope: 'openid courses offline_access', state: 'st', ...extra,
  })}`));
  try {
    await db.query(`DEFINE NAMESPACE ${namespace}; DEFINE DATABASE main;`);
    await db.queryAll(await readFile(new URL('../../../schema/src/schema.surql', import.meta.url), 'utf8'));
    await db.queryAll(await readFile(new URL('../src/server-schema.surql', import.meta.url), 'utf8'));
    await db.query(`CREATE user:u1 CONTENT {email:'student@example.test',name:'Student',password:'test'};`);

    await t.test('unsafe or mismatching documents are refused before any request is stored', async () => {
      for (const extra of [
        { client_id: 'https://chat.example/other.json' },
        { client_id: 'https://chat.example/secret.json' },
        { client_id: 'https://chat.example/missing.json' },
        { client_id: 'https://chat.example/oauth/client.json?x=1' },
        { client_id: 'https://127.0.0.1/client.json' },
        { redirect_uri: 'https://chat.example/elsewhere' },
        { scope: 'courses admin' },
      ]) {
        const rejected = await authorize(extra);
        assert.equal(rejected.response.status, 400, JSON.stringify(extra));
        assert.equal(rejected.response.headers.has('location'), false);
      }
      assert.ok(!fetched.some((href) => href.includes('?') || href.includes('127.0.0.1')), 'Invalid client ids are never fetched.');
      assert.equal(await db.query('RETURN count(SELECT id FROM oauth_request)'), 0);
    });

    await t.test('approval returns the code with the issuer, and the URL client redeems it', async () => {
      const started = await authorize({ resource: `${resource}/` });
      assert.equal(started.response.status, 302);
      const requestId = new URL(started.response.headers.get('location')!).searchParams.get('request')!;
      const consent = await getOAuthConsent(env, 'user:u1', requestId);
      assert.equal(consent.clientName, 'Chat Example');
      assert.equal(consent.clientHost, 'chat.example');
      const approved = new URL((await approveOAuthConsent(env, 'user:u1', { requestId, allow: true })).redirectUrl);
      assert.equal(approved.origin + approved.pathname, callback);
      assert.equal(approved.searchParams.get('iss'), origin);
      const tokens = await reply(post('/oauth/token', {
        client_id: clientUrl, grant_type: 'authorization_code', code: approved.searchParams.get('code')!, code_verifier: verifier,
        redirect_uri: callback, resource,
      }));
      assert.equal(tokens.response.status, 200, JSON.stringify(tokens.data));
      assert.equal(tokens.data.scope, 'courses');
      const auth = await verifyOAuthAccessToken(env, tokens.data.access_token as string, resource);
      assert.equal(auth?.clientId, clientUrl);
      assert.equal(auth?.extra?.clientName, 'Chat Example');
      // Authorizing again refreshes the same client rather than adding one.
      assert.equal((await authorize()).response.status, 302);
      assert.equal(await db.query('RETURN count(SELECT id FROM oauth_client)'), 1);
    });
  } finally {
    await db.query(`REMOVE NAMESPACE ${namespace};`);
  }
});
