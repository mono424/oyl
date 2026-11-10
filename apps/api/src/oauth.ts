import { oauthMetadataResponse, type AuthInfo, type OAuthMetadata } from '@modelcontextprotocol/server';
import type { Env } from './env';
import { randomKey, safeEqual, sha256Hex } from './auth';
import { ensureServerSchema, readLimited } from './service';
import { BadRequest, Surreal, signinRecord } from './surreal';

const SCOPE = 'courses';
const ACCESS_SECONDS = 3600;
const REFRESH_SECONDS = 30 * 24 * 3600;
const REQUEST_SECONDS = 10 * 60;
const CODE_SECONDS = 5 * 60;
const MAX_BODY = 16_384;
// Client ID Metadata Documents (CIMD) are small JSON files on the client's own
// site. ChatGPT identifies itself with one instead of registering.
const MAX_CLIENT_METADATA = 8192;
const CLIENT_METADATA_TIMEOUT_MS = 10_000;
// A document read this recently is used as stored. Older ones are read again,
// and the stored copy stands in while the client's site does not answer.
const CLIENT_METADATA_FRESH = '1h';
// OIDC scopes clients ask for by habit. This server has no ID tokens or user
// info, so they are left out of the grant rather than refused.
const IGNORED_SCOPES = new Set(['openid', 'profile', 'email', 'offline_access']);
const newId = (table: string) => `${table}:k${crypto.randomUUID().replaceAll('-', '')}`;
const deadline = (seconds: number) => new Date(Date.now() + seconds * 1000).toISOString();

interface Client {
  id: string;
  client_id: string;
  client_name: string;
  redirect_uris: string[];
}

interface AuthorizationRequest {
  id: string;
  client: string;
  redirect_uri: string;
  state?: string;
  /** The issuer the client discovered, returned as `iss` with the code (RFC 9207). */
  issuer?: string;
  resource: string;
  scope: string;
  challenge: string;
  expires_at: string;
  consumed_at?: string;
}

interface Grant {
  id: string;
  owner: string;
  client: string;
  resource: string;
  scope: string;
  connection: string;
  expires_at: string;
}

interface Code {
  id: string;
  grant: string;
  redirect_uri: string;
  challenge: string;
  expires_at: string;
  consumed_at?: string;
}

class OAuthFailure extends Error {
  constructor(readonly code: string, message: string, readonly status = 400) { super(message); }
}

function fail(code: string, description: string): never { throw new OAuthFailure(code, description); }

/** Public issuer and resource URLs must match across discovery, tokens and MCP. */
export function oauthOrigin(request: Request, env: Env): string {
  const origin = new URL(env.API_ORIGIN?.trim() || request.url).origin;
  const url = new URL(origin);
  if (url.protocol !== 'https:' && !(url.protocol === 'http:' && isLoopback(url.hostname))) {
    throw new Error('The OAuth API origin must use HTTPS.');
  }
  return origin;
}

function isLoopback(hostname: string): boolean {
  return hostname === 'localhost' || hostname === '127.0.0.1' || hostname === '[::1]';
}

export function appOrigin(env: Env, apiOrigin: string): string {
  const origins = env.APP_ORIGIN.split(',').map((value) => value.trim()).filter(Boolean);
  if (origins.includes(apiOrigin)) return apiOrigin;
  return origins.find((value) => value.startsWith('https://')) ?? origins[0] ?? apiOrigin;
}

function metadata(origin: string): OAuthMetadata {
  return {
    issuer: origin,
    authorization_endpoint: `${origin}/oauth/authorize`,
    token_endpoint: `${origin}/oauth/token`,
    registration_endpoint: `${origin}/oauth/register`,
    revocation_endpoint: `${origin}/oauth/revoke`,
    response_types_supported: ['code'],
    grant_types_supported: ['authorization_code', 'refresh_token'],
    token_endpoint_auth_methods_supported: ['none'],
    revocation_endpoint_auth_methods_supported: ['none'],
    code_challenge_methods_supported: ['S256'],
    scopes_supported: [SCOPE],
    // An https `client_id` is the URL of the client's metadata document.
    client_id_metadata_document_supported: true,
    // Every redirect back to the client carries `iss`, success or denial.
    authorization_response_iss_parameter_supported: true,
  } as OAuthMetadata;
}

function response(value: unknown, status = 200): Response {
  return Response.json(value, { status, headers: {
    'cache-control': 'no-store', pragma: 'no-cache', 'access-control-allow-origin': '*',
  } });
}

async function body(request: Request, json = false): Promise<Record<string, unknown>> {
  let raw: string;
  try { raw = new TextDecoder().decode(await readLimited(request.body, MAX_BODY)); }
  catch { return fail('invalid_request', 'The request body is invalid or too large.'); }
  const mime = request.headers.get('content-type')?.split(';')[0].trim();
  try {
    if (json) {
      if (mime !== 'application/json') fail('invalid_request', 'Send application/json.');
      const value: unknown = JSON.parse(raw);
      if (!value || typeof value !== 'object' || Array.isArray(value)) fail('invalid_request', 'Send a JSON object.');
      return value as Record<string, unknown>;
    }
    if (mime !== 'application/x-www-form-urlencoded') fail('invalid_request', 'Send a form-encoded request.');
    const params = new URLSearchParams(raw);
    for (const key of new Set(params.keys())) if (params.getAll(key).length !== 1) fail('invalid_request', 'Repeated parameters are not supported.');
    return Object.fromEntries(params);
  } catch (error) {
    if (error instanceof OAuthFailure) throw error;
    return fail('invalid_request', 'The request body could not be read.');
  }
}

function parameter(values: Record<string, unknown>, key: string, required = true): string {
  const value = values[key];
  if (value === undefined && !required) return '';
  if (typeof value !== 'string' || !value || value.length > 4096) fail('invalid_request', `Invalid ${key}.`);
  return value;
}

function redirectUri(value: unknown): string {
  if (typeof value !== 'string' || value.length > 2048 || /\s/.test(value)) fail('invalid_redirect_uri', 'Redirect URIs must be HTTPS URLs.');
  let url: URL;
  try { url = new URL(value); } catch { return fail('invalid_redirect_uri', 'Redirect URIs must be HTTPS URLs.'); }
  if ((url.protocol !== 'https:' && !(url.protocol === 'http:' && isLoopback(url.hostname))) || url.hash || url.username || url.password) {
    fail('invalid_redirect_uri', 'Redirect URIs must use HTTPS and have no fragment or credentials.');
  }
  return value;
}

function requestedScope(value: unknown): string {
  if (value === undefined || value === '') return SCOPE;
  if (typeof value !== 'string' || value.length > 512) fail('invalid_scope', 'Only the courses scope is available.');
  const scopes = value.split(' ').filter(Boolean);
  if (scopes.some((scope) => scope !== SCOPE && !IGNORED_SCOPES.has(scope))) fail('invalid_scope', 'Only the courses scope is available.');
  return SCOPE;
}

function requestedResource(value: unknown, expected: string): string {
  if (value !== undefined && value !== '' && (typeof value !== 'string' || value.replace(/\/$/, '') !== expected)) {
    fail('invalid_target', 'This server only grants access to its MCP endpoint.');
  }
  return expected;
}

async function findClient(env: Env, clientId: string): Promise<Client> {
  const row = await Surreal.system(env).query<Client | undefined>(
    'RETURN (SELECT * FROM oauth_client WHERE client_id = $client LIMIT 1)[0]', { client: clientId },
  );
  if (!row) fail('invalid_client', 'Unknown client.');
  return row;
}

/** An https `client_id` names a Client ID Metadata Document; anything else was registered here. */
export function metadataDocumentUrl(clientId: string): URL | null {
  if (!clientId.startsWith('https://')) return null;
  let url: URL;
  try { url = new URL(clientId); } catch { return fail('invalid_client', 'The client ID is not a valid URL.'); }
  // A stable, plain https URL on a named host: no credentials, port, query or
  // fragment, and a path beyond `/` (draft-ietf-oauth-client-id-metadata-document).
  const ipLiteral = /^\[|^\d+\.\d+\.\d+\.\d+$/.test(url.hostname);
  if (url.href !== clientId || url.username || url.password || url.port || url.search || url.hash || url.pathname === '/'
    || ipLiteral || !url.hostname.includes('.') || /(^|\.)localhost$/.test(url.hostname) || clientId.length > 512) {
    fail('invalid_client', 'The client ID must be a plain https URL of a client metadata document.');
  }
  return url;
}

/**
 * Reads the client's metadata document and stores what this server needs from
 * it under a record derived from the URL, so the token endpoint finds the same
 * client. Read again when the stored copy is over an hour old, so a client
 * that changes its redirect URIs or name is current soon after.
 */
async function metadataDocumentClient(env: Env, url: URL): Promise<Client> {
  const id = `oauth_client:m${(await sha256Hex(url.href)).slice(0, 32)}`;
  const stored = await Surreal.system(env).query<(Client & { fresh: boolean }) | undefined>(
    `RETURN (SELECT *, fetched_at > time::now() - ${CLIENT_METADATA_FRESH} AS fresh FROM type::record($id) WHERE client_id = $client)[0]`,
    { id, client: url.href },
  );
  if (stored?.fresh) return stored;
  let doc: unknown;
  try {
    // Workers have no redirect: 'error'; a redirect is refused as not ok.
    const res = await fetch(url, {
      headers: { accept: 'application/json', 'user-agent': 'TalkToBolo-OAuth/1.0' }, redirect: 'manual', signal: AbortSignal.timeout(CLIENT_METADATA_TIMEOUT_MS),
    });
    if (!res.ok) throw new Error(`status ${res.status}`);
    doc = JSON.parse(new TextDecoder().decode(await readLimited(res.body, MAX_CLIENT_METADATA)));
  } catch (error) {
    console.error('[oauth] client metadata document unavailable', url.host, error instanceof Error ? error.message : 'error');
    if (stored) return stored;
    return fail('invalid_client', 'The client metadata document could not be read.');
  }
  if (!doc || typeof doc !== 'object' || Array.isArray(doc)) fail('invalid_client_metadata', 'The client metadata document is not a JSON object.');
  const values = doc as Record<string, unknown>;
  if (values.client_id !== url.href) fail('invalid_client_metadata', 'The client metadata document names another client.');
  if (values.client_secret !== undefined) fail('invalid_client_metadata', 'A client metadata document must not hold a secret.');
  // The client may prefer another method; it uses one both sides support.
  const methods = values.token_endpoint_auth_methods_supported;
  const single = values.token_endpoint_auth_method;
  const publicClient = Array.isArray(methods) ? methods.includes('none') : single === undefined || single === 'none';
  if (!publicClient) fail('invalid_client_metadata', 'This server only supports public clients (token_endpoint_auth_method none).');
  for (const [key, needed] of [['grant_types', 'authorization_code'], ['response_types', 'code']] as const) {
    if (values[key] !== undefined && (!Array.isArray(values[key]) || !(values[key] as unknown[]).includes(needed))) {
      fail('invalid_client_metadata', `The client does not support ${needed}.`);
    }
  }
  if (!Array.isArray(values.redirect_uris) || values.redirect_uris.length === 0 || values.redirect_uris.length > 10) {
    fail('invalid_client_metadata', 'The client metadata document must list between one and ten redirect URIs.');
  }
  const redirects = [...new Set(values.redirect_uris.map(redirectUri))];
  const raw = typeof values.client_name === 'string' ? values.client_name.trim().slice(0, 80) : '';
  const name = raw && !/[\u0000-\u001f\u007f]/.test(raw) ? raw : url.hostname;
  await Surreal.system(env).query(
    `UPSERT type::record($id) SET client_id = $client, client_name = string::slice($name, 2), redirect_uris = $redirects,
       fetched_at = time::now()`,
    { id, client: url.href, name: `!:${name}`, redirects },
  );
  return { id, client_id: url.href, client_name: name, redirect_uris: redirects };
}

async function register(request: Request, env: Env): Promise<Response> {
  const values = await body(request, true);
  if (values.token_endpoint_auth_method !== undefined && values.token_endpoint_auth_method !== 'none') fail('invalid_client_metadata', 'Only public clients are supported.');
  for (const [key, supported] of [['grant_types', ['authorization_code', 'refresh_token']], ['response_types', ['code']]] as const) {
    const supplied = values[key];
    if (supplied !== undefined && (!Array.isArray(supplied) || supplied.length === 0 || supplied.some((value) => !supported.includes(value as never)))) {
      fail('invalid_client_metadata', `Unsupported ${key}.`);
    }
  }
  if (!Array.isArray(values.redirect_uris) || values.redirect_uris.length === 0 || values.redirect_uris.length > 10) fail('invalid_client_metadata', 'Register between one and ten redirect URIs.');
  const redirects = [...new Set(values.redirect_uris.map(redirectUri))];
  const name = typeof values.client_name === 'string' ? values.client_name.trim().slice(0, 80) : 'AI assistant';
  if (!name || /[\u0000-\u001f\u007f]/.test(name)) fail('invalid_client_metadata', 'Invalid client name.');
  requestedScope(values.scope);
  const clientId = `bolo_${randomKey(24)}`;
  await Surreal.system(env).query(
    'CREATE type::record($id) CONTENT {client_id:$client,client_name:string::slice($name,2),redirect_uris:$redirects}',
    { id: newId('oauth_client'), client: clientId, name: `!:${name}`, redirects },
  );
  return response({
    client_id: clientId, client_id_issued_at: Math.floor(Date.now() / 1000), client_name: name,
    redirect_uris: redirects, token_endpoint_auth_method: 'none',
    grant_types: ['authorization_code', 'refresh_token'], response_types: ['code'], scope: SCOPE,
  }, 201);
}

async function authorize(request: Request, env: Env, origin: string): Promise<Response> {
  const params = new URL(request.url).searchParams;
  for (const key of new Set(params.keys())) if (params.getAll(key).length !== 1) fail('invalid_request', 'Repeated parameters are not supported.');
  const values = Object.fromEntries(params);
  const clientId = parameter(values, 'client_id');
  const documentUrl = metadataDocumentUrl(clientId);
  const client = documentUrl ? await metadataDocumentClient(env, documentUrl) : await findClient(env, clientId);
  const redirect = parameter(values, 'redirect_uri');
  // Never redirect errors until the registered callback has been verified.
  if (!client.redirect_uris.includes(redirect)) fail('invalid_request', 'The redirect URI is not registered for this client.');
  if (parameter(values, 'response_type') !== 'code') fail('unsupported_response_type', 'Only authorization codes are supported.');
  if (parameter(values, 'code_challenge_method') !== 'S256') fail('invalid_request', 'PKCE S256 is required.');
  const challenge = parameter(values, 'code_challenge');
  if (!/^[A-Za-z0-9_-]{43}$/.test(challenge)) fail('invalid_request', 'Invalid PKCE challenge.');
  const scope = requestedScope(values.scope);
  const resource = requestedResource(values.resource, `${origin}/mcp`);
  const id = newId('oauth_request');
  const state = parameter(values, 'state', false);
  // Surreal's JSON transport recognizes UUIDs and datetimes inside strings.
  // Prefix opaque input before transport, then remove it inside the query
  // so the original bytes and formatting remain a string in the database.
  await Surreal.system(env).query(
    `CREATE type::record($id) CONTENT {
      client:type::record($client),redirect_uri:$redirect,
      state:IF $state = NONE THEN NONE ELSE string::slice($state,2) END,issuer:$issuer,resource:$resource,scope:$scope,
      challenge:$challenge,expires_at:<datetime>$expires
    }`,
    { id, client: client.id, redirect, state: state ? `!:${state}` : null, issuer: origin, resource, scope, challenge, expires: deadline(REQUEST_SECONDS) },
  );
  const target = new URL('/connect', appOrigin(env, origin));
  target.searchParams.set('request', id.split(':')[1]);
  return new Response(null, { status: 302, headers: { location: target.href, 'cache-control': 'no-store', 'referrer-policy': 'no-referrer' } });
}

function requestRecord(requestId: unknown): string {
  if (typeof requestId !== 'string' || !/^k[a-f0-9]{32}$/.test(requestId)) throw new BadRequest('This connection request is invalid or has expired.');
  return `oauth_request:${requestId}`;
}

async function findRequest(env: Env, requestId: unknown): Promise<AuthorizationRequest> {
  const row = await Surreal.system(env).query<AuthorizationRequest | undefined>(
    'RETURN (SELECT * FROM type::record($id) WHERE expires_at > time::now() AND consumed_at = NONE)[0]',
    { id: requestRecord(requestId) },
  );
  if (!row) throw new BadRequest('This connection request is invalid or has expired. Start connecting again.');
  return row;
}

export async function getOAuthConsent(env: Env, _userId: string, requestId: string) {
  await ensureServerSchema(env);
  const pending = await findRequest(env, requestId);
  const client = await Surreal.system(env).query<Client>('SELECT * FROM ONLY type::record($id)', { id: pending.client });
  if (!client) throw new BadRequest('This connection request is no longer available.');
  return {
    requestId, clientName: client.client_name, redirectHost: new URL(pending.redirect_uri).host,
    // A metadata document's name is vouched for by the site serving it.
    clientHost: client.client_id.startsWith('https://') ? new URL(client.client_id).host : null,
    scopes: [pending.scope], expiresAt: pending.expires_at,
  };
}

/** `issuer` is this server's OAuth issuer, for requests made before it was stored with them. */
export async function approveOAuthConsent(env: Env, userId: string, input: { requestId: string; allow: boolean }, issuer?: string) {
  await ensureServerSchema(env);
  if (typeof input.allow !== 'boolean') throw new BadRequest('Choose whether to allow this connection.');
  const pending = await findRequest(env, input.requestId);
  const client = await Surreal.system(env).query<Client>('SELECT * FROM ONLY type::record($id)', { id: pending.client });
  if (!client) throw new BadRequest('This connection request is no longer available.');
  const redirect = new URL(pending.redirect_uri);
  if (pending.state) redirect.searchParams.set('state', pending.state);
  // RFC 9207: tells the client which server answered, against mix-up attacks.
  const iss = pending.issuer ?? issuer;
  if (iss) redirect.searchParams.set('iss', iss);
  const code = randomKey(32);
  const connection = newId('api_key');
  const grant = newId('oauth_grant');
  const vars = {
    request: pending.id, owner: userId, client: pending.client, connection, grant,
    redirect: pending.redirect_uri, resource: pending.resource, scope: pending.scope, challenge: pending.challenge,
    code_id: newId('oauth_code'), code_hash: await sha256Hex(code), code_expires: deadline(CODE_SECONDS),
    grant_expires: deadline(REFRESH_SECONDS), name: `${client.client_name} connection`.slice(0, 80),
    unused_hash: await sha256Hex(randomKey(32)), allowed: input.allow,
  };
  try {
    await Surreal.system(env).queryAll(`BEGIN TRANSACTION;
      LET $claimed = UPDATE type::record($request) SET consumed_at = time::now()
        WHERE consumed_at = NONE AND expires_at > time::now() RETURN id;
      IF array::len($claimed) != 1 { THROW 'oauth_request_consumed'; };
      IF $allowed {
        CREATE type::record($connection) CONTENT {owner:type::record($owner),name:$name,prefix:'OAuth',key_hash:$unused_hash};
        CREATE type::record($grant) CONTENT {
          owner:type::record($owner),client:type::record($client),connection:type::record($connection),
          resource:$resource,scope:$scope,expires_at:<datetime>$grant_expires
        };
        CREATE type::record($code_id) CONTENT {
          grant:type::record($grant),code_hash:$code_hash,redirect_uri:$redirect,challenge:$challenge,expires_at:<datetime>$code_expires
        };
      };
      COMMIT TRANSACTION;`, vars);
  } catch (error) {
    if (error instanceof Error && /oauth_request_consumed|transaction.*conflict|conflict.*transaction|failed transaction/i.test(error.message)) {
      const current = await Surreal.system(env).query<{ consumed_at?: string; expires_at: string } | undefined>(
        'RETURN (SELECT consumed_at,expires_at FROM type::record($id))[0]', { id: pending.id },
      );
      if (!current || current.consumed_at || Date.parse(current.expires_at) <= Date.now()) {
        throw new BadRequest('This connection request was already used or expired. Start connecting again.');
      }
    }
    throw error;
  }
  if (input.allow) redirect.searchParams.set('code', code);
  else { redirect.searchParams.set('error', 'access_denied'); redirect.searchParams.set('error_description', 'The student declined access.'); }
  return { redirectUrl: redirect.href };
}

async function pkceChallenge(verifier: string): Promise<string> {
  const bytes = new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(verifier)));
  return btoa(String.fromCharCode(...bytes)).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '');
}

async function findGrant(env: Env, id: string, client: string): Promise<Grant> {
  const grant = await Surreal.system(env).query<Grant | undefined>(
    `RETURN (SELECT * FROM type::record($id) WHERE client = type::record($client)
      AND revoked_at = NONE AND connection.id != NONE AND connection.revoked_at = NONE AND connection.owner = owner
      AND expires_at > time::now())[0]`, { id, client },
  );
  if (!grant) fail('invalid_grant', 'This authorization is invalid, expired or revoked.');
  return grant;
}

async function token(request: Request, env: Env, origin: string): Promise<Response> {
  const values = await body(request);
  if (request.headers.has('authorization') || values.client_secret !== undefined) fail('invalid_client', 'This endpoint accepts public clients without secrets.');
  const client = await findClient(env, parameter(values, 'client_id'));
  const type = parameter(values, 'grant_type');
  const resource = requestedResource(values.resource, `${origin}/mcp`);
  let grant: Grant;
  let consumedId: string;
  let rotate = false;
  if (type === 'authorization_code') {
    const hash = await sha256Hex(parameter(values, 'code'));
    const code = await Surreal.system(env).query<Code | undefined>(
      'RETURN (SELECT * FROM oauth_code WHERE code_hash = $hash AND consumed_at = NONE AND expires_at > time::now() LIMIT 1)[0]', { hash },
    );
    if (!code) fail('invalid_grant', 'This authorization code is invalid, expired or already used.');
    if (parameter(values, 'redirect_uri') !== code.redirect_uri) fail('invalid_grant', 'Redirect URI does not match the authorization request.');
    const verifier = parameter(values, 'code_verifier');
    if (!/^[A-Za-z0-9._~-]{43,128}$/.test(verifier) || !safeEqual(await pkceChallenge(verifier), code.challenge)) fail('invalid_grant', 'PKCE verification failed.');
    grant = await findGrant(env, code.grant, client.id);
    consumedId = code.id;
  } else if (type === 'refresh_token') {
    const hash = await sha256Hex(parameter(values, 'refresh_token'));
    const refresh = await Surreal.system(env).query<{id: string; grant: string; consumed_at?: string; expires_at: string} | undefined>(
      'RETURN (SELECT * FROM oauth_refresh WHERE token_hash = $hash LIMIT 1)[0]', { hash },
    );
    if (!refresh) fail('invalid_grant', 'This refresh token is invalid, expired or revoked.');
    if (Date.parse(refresh.expires_at) <= Date.now()) fail('invalid_grant', 'This refresh token has expired.');
    grant = await findGrant(env, refresh.grant, client.id);
    if (refresh.consumed_at) {
      await Surreal.system(env).query('UPDATE type::record($id) SET revoked_at = time::now()', { id: grant.id });
      fail('invalid_grant', 'This refresh token was already used. Reconnect the app.');
    }
    consumedId = refresh.id;
    rotate = true;
    if (values.scope !== undefined && requestedScope(values.scope) !== grant.scope) fail('invalid_scope', 'Scope cannot be increased.');
  } else return fail('unsupported_grant_type', 'Only authorization_code and refresh_token are supported.');
  if (grant.resource !== resource) fail('invalid_target', 'The requested resource does not match this authorization.');
  const access = `bolo_at_${randomKey(32)}`;
  const refresh = `bolo_rt_${randomKey(32)}`;
  const accessExpiry = new Date(Math.min(Date.now() + ACCESS_SECONDS * 1000, Date.parse(grant.expires_at))).toISOString();
  try {
    await Surreal.system(env).queryAll(`BEGIN TRANSACTION;
      LET $grant = (SELECT * FROM type::record($grant_id) WHERE revoked_at = NONE AND expires_at > time::now()
        AND connection.id != NONE AND connection.revoked_at = NONE AND connection.owner = owner)[0];
      IF $grant = NONE { THROW 'oauth_invalid_grant'; };
      LET $claimed = UPDATE type::record($consumed_id) SET consumed_at = time::now()
        WHERE consumed_at = NONE AND expires_at > time::now() RETURN id;
      IF array::len($claimed) != 1 { THROW 'oauth_invalid_grant'; };
      IF $rotate { UPDATE oauth_access SET revoked_at = time::now() WHERE grant = type::record($grant_id); };
      CREATE type::record($access_id) CONTENT {
        grant:type::record($grant_id),owner:$grant.owner,token_hash:$access_hash,expires_at:<datetime>$access_expires
      };
      CREATE type::record($refresh_id) CONTENT {
        grant:type::record($grant_id),token_hash:$refresh_hash,expires_at:$grant.expires_at
      };
      COMMIT TRANSACTION;`, {
      grant_id: grant.id, consumed_id: consumedId, rotate, access_id: newId('oauth_access'), refresh_id: newId('oauth_refresh'),
      access_hash: await sha256Hex(access), refresh_hash: await sha256Hex(refresh), access_expires: accessExpiry,
    });
  } catch (error) {
    if (error instanceof Error && /oauth_invalid_grant|transaction.*conflict|conflict.*transaction|failed transaction/i.test(error.message)) {
      const current = await Surreal.system(env).query<{ consumed_at?: string; expires_at: string } | undefined>(
        'RETURN (SELECT consumed_at,expires_at FROM type::record($id))[0]', { id: consumedId },
      );
      // Surreal reports the first rolled-back statement before the actual
      // transaction error. Confirm invalidation before mapping it to OAuth.
      await findGrant(env, grant.id, client.id);
      if (current && !current.consumed_at && Date.parse(current.expires_at) > Date.now()) throw error;
      // A simultaneous refresh can race the initial read. Treat losing the
      // atomic claim as reuse, just like a token already marked consumed.
      if (rotate) await Surreal.system(env).query('UPDATE type::record($id) SET revoked_at = time::now()', { id: grant.id });
      fail('invalid_grant', 'This authorization was already used or revoked.');
    }
    throw error;
  }
  return response({
    access_token: access, token_type: 'Bearer', expires_in: Math.max(0, Math.floor((Date.parse(accessExpiry) - Date.now()) / 1000)),
    refresh_token: refresh, scope: grant.scope,
  });
}

async function revoke(request: Request, env: Env): Promise<Response> {
  const values = await body(request);
  const client = await findClient(env, parameter(values, 'client_id'));
  const hash = await sha256Hex(parameter(values, 'token'));
  // Unknown tokens and other clients' tokens return the same successful reply.
  await Surreal.system(env).queryAll(`BEGIN TRANSACTION;
    LET $matching_access = (SELECT VALUE grant FROM oauth_access WHERE token_hash = $hash LIMIT 1)[0];
    LET $matching_refresh = (SELECT VALUE grant FROM oauth_refresh WHERE token_hash = $hash LIMIT 1)[0];
    UPDATE oauth_grant SET revoked_at = time::now()
      WHERE (id = $matching_access OR id = $matching_refresh) AND client = type::record($client);
    COMMIT TRANSACTION;`, { hash, client: client.id });
  return new Response(null, { status: 200, headers: { 'cache-control': 'no-store', 'access-control-allow-origin': '*' } });
}

/** OAuth entry points are public; approval is a separate authenticated API call. */
export async function handleOAuth(request: Request, env: Env): Promise<Response | undefined> {
  const path = new URL(request.url).pathname;
  if (!path.startsWith('/oauth/') && !path.startsWith('/.well-known/oauth-')) return undefined;
  try {
    const origin = oauthOrigin(request, env);
    const discovered = oauthMetadataResponse(request, {
      oauthMetadata: metadata(origin), resourceServerUrl: new URL(`${origin}/mcp`), scopesSupported: [SCOPE], resourceName: 'TalkToBolo',
    });
    if (discovered) return discovered;
    if (!['/oauth/register', '/oauth/authorize', '/oauth/token', '/oauth/revoke'].includes(path)) return undefined;
    if (request.method === 'OPTIONS') return new Response(null, { status: 204, headers: {
      'access-control-allow-origin': '*', 'access-control-allow-methods': 'GET, POST, OPTIONS', 'access-control-allow-headers': 'content-type',
    } });
    const method = path === '/oauth/authorize' ? 'GET' : 'POST';
    if (request.method !== method) return new Response(null, { status: 405, headers: { allow: method } });
    await ensureServerSchema(env);
    if (path === '/oauth/register') return await register(request, env);
    if (path === '/oauth/authorize') return await authorize(request, env, origin);
    if (path === '/oauth/token') return await token(request, env, origin);
    return await revoke(request, env);
  } catch (error) {
    if (error instanceof OAuthFailure) return response({ error: error.code, error_description: error.message }, error.status);
    console.error('OAuth request failed', error instanceof Error ? error.name : 'Unknown error');
    return response({ error: 'server_error', error_description: 'The connection could not be completed. Try again.' }, 500);
  }
}

/** Access is checked on every request so disconnecting an app takes effect immediately. */
export async function verifyOAuthAccessToken(env: Env, token: string, resource: string): Promise<AuthInfo | null> {
  if (!/^bolo_at_[A-Za-z0-9_-]{43}$/.test(token)) return null;
  await ensureServerSchema(env);
  const row = await Surreal.system(env).query<{
    owner: string; expires_at: string; client_id: string; client_name: string; scope: string; resource: string;
  } | undefined>(`RETURN (SELECT owner,expires_at,grant.client.client_id AS client_id,grant.client.client_name AS client_name,
      grant.scope AS scope,grant.resource AS resource
    FROM oauth_access WHERE token_hash = $hash AND revoked_at = NONE AND expires_at > time::now()
      AND grant.revoked_at = NONE AND grant.expires_at > time::now()
      AND grant.connection.id != NONE AND grant.connection.revoked_at = NONE AND grant.connection.owner = owner AND grant.owner = owner LIMIT 1)[0]`,
  { hash: await sha256Hex(token) });
  if (!row || row.resource !== resource) return null;
  const dbToken = await signinRecord(env, 'oauth_agent', { oauth_key: token });
  if (!dbToken) return null;
  return {
    token, clientId: row.client_id, scopes: row.scope.split(' '), expiresAt: Math.floor(Date.parse(row.expires_at) / 1000),
    resource: new URL(row.resource), extra: { userId: row.owner, dbToken, clientName: row.client_name },
  };
}
