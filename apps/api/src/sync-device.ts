import type { Env } from './env';
import { randomKey, sha256Hex } from './auth';
import { appOrigin, oauthOrigin } from './oauth';
import { ensureServerTable } from './service';
import { BadRequest, Conflict, NotFound, Surreal } from './surreal';

/**
 * How the desktop app signs in to sync (docs/sync-protocol.md, Auth), in the
 * style of OAuth's device flow (RFC 8628):
 *
 * 1. The app asks for a code (`startDeviceFlow`) and opens the web app's
 *    `/connect?code=<user code>`.
 * 2. The signed-in user sees "Oyl on <device>" there and approves or
 *    declines (`approveDevice`).
 * 3. The app polls with its device code (`pollDevice`) and, once approved,
 *    gets its sync key exactly once.
 *
 * The key (`oyl_sync_...`) is minted on that poll, so it is only ever seen by
 * the device. It becomes an `api_key` row (the connection Settings lists and
 * revokes, with a `key_hash` that matches no key) and a server-only
 * `sync_device` row holding the key's SHA-256. The `agent` access cannot sign
 * in with it, so it opens /api/sync and nothing else.
 */

export const SYNC_KEY_PREFIX = 'oyl_sync_';
export const DEVICE_SECONDS = 600;
export const POLL_SECONDS = 5;
const MAX_DEVICE_NAME = 60;
// Consonants only, so a code never spells a word; 20^8 codes.
const ALPHABET = 'BCDFGHJKLMNPQRSTVWXZ';

const newKey = (table: string) => `${table}:k${crypto.randomUUID().replaceAll('-', '')}`;

/** Text a person typed or a machine reported, on one line and cut to `max`. */
export function cleanText(raw: unknown, max: number): string {
  return typeof raw === 'string' ? raw.replace(/[\u0000-\u001f\u007f]+/g, ' ').replace(/\s+/g, ' ').trim().slice(0, max) : '';
}

export const deviceName = (raw: unknown) => cleanText(raw, MAX_DEVICE_NAME) || 'Mac';

export function newUserCode(): string {
  let out = '';
  while (out.length < 8) {
    // 240 is the largest multiple of 20 below 256: no letter comes up more often.
    for (const b of crypto.getRandomValues(new Uint8Array(16))) if (b < 240 && out.length < 8) out += ALPHABET[b % 20];
  }
  return out;
}

/** The code as people read it: BCDF-GHJK. */
export const formatUserCode = (code: string) => `${code.slice(0, 4)}-${code.slice(4)}`;

/** A user code as typed (any case, with or without the dash), or null. */
export function parseUserCode(raw: unknown): string | null {
  const code = typeof raw === 'string' ? raw.toUpperCase().replace(/[\s-]/g, '') : '';
  return code.length === 8 && [...code].every((ch) => ALPHABET.includes(ch)) ? code : null;
}

const codeId = (code: string) => `sync_device_code:c${code}`;

const isLoopback = (host: string) => host === 'localhost' || host === '127.0.0.1' || host === '[::1]';

/**
 * Where the web app's /connect page is: the app origin serving this API, or
 * else one of APP_ORIGIN, a local one while the API itself runs locally.
 */
export function connectOrigin(request: Request, env: Env): string {
  const api = oauthOrigin(request, env);
  const origins = env.APP_ORIGIN.split(',').map((o) => o.trim()).filter(Boolean);
  if (origins.includes(api)) return api;
  if (isLoopback(new URL(api).hostname)) {
    const local = origins.find((o) => isLoopback(new URL(o).hostname));
    if (local) return local;
  }
  return appOrigin(env, api);
}

export interface DeviceStart {
  device_code: string;
  user_code: string;
  verify_url: string;
  interval: number;
  expires_in: number;
}

/** A new sign-in for a device: the codes, and the page that approves it. */
export async function startDeviceFlow(env: Env, name: string, origin: string): Promise<DeviceStart> {
  await ensureServerTable(env, 'sync_device_code');
  const db = Surreal.system(env);
  // Housekeeping: codes from more than a day ago.
  await db.query('DELETE sync_device_code WHERE expires_at < time::now() - 1d;').catch((err) => console.error('[sync] code cleanup', err));
  const deviceCode = randomKey(32);
  const hash = await sha256Hex(deviceCode);
  for (let attempt = 0; attempt < 5; attempt++) {
    const code = newUserCode();
    try {
      // A code still in use by another sign-in is not handed out twice.
      await db.queryAll(
        `BEGIN TRANSACTION;
         IF (SELECT VALUE id FROM type::record($id) WHERE expires_at > time::now())[0] != NONE { THROW 'sync_code_taken'; };
         UPSERT type::record($id) CONTENT {
           device_hash: $hash, device_name: $name, status: 'pending', expires_at: time::now() + ${DEVICE_SECONDS}s
         };
         COMMIT TRANSACTION;`,
        { id: codeId(code), hash, name },
      );
      const formatted = formatUserCode(code);
      return {
        device_code: deviceCode,
        user_code: formatted,
        verify_url: `${origin}/connect?code=${formatted}`,
        interval: POLL_SECONDS,
        expires_in: DEVICE_SECONDS,
      };
    } catch (error) {
      if (error instanceof Error && /sync_code_taken|Transaction conflict/i.test(error.message)) continue;
      throw error;
    }
  }
  throw new Error('No free device code after five attempts.');
}

interface CodeRow {
  id: string;
  device_name: string;
  status: 'pending' | 'approved' | 'denied';
  owner?: string;
  created_at: string;
  expires_at: string;
}

async function openCode(env: Env, code: string): Promise<CodeRow | null> {
  return Surreal.system(env).query<CodeRow | null>(
    'RETURN (SELECT * FROM type::record($id) WHERE expires_at > time::now() AND consumed_at = NONE)[0]',
    { id: codeId(code) },
  );
}

const STALE = 'This code is not valid or has expired. Start signing in again from Oyl on your Mac.';

/** The sign-in a user code belongs to, for the /connect page. */
export async function deviceRequest(env: Env, rawCode: unknown) {
  const code = parseUserCode(rawCode);
  if (!code) throw new BadRequest('Enter the 8-letter code that Oyl shows, like BCDF-GHJK.');
  await ensureServerTable(env, 'sync_device_code');
  const row = await openCode(env, code);
  if (!row) throw new NotFound(STALE);
  return { user_code: formatUserCode(code), device_name: row.device_name, status: row.status, created: row.created_at, expires: row.expires_at };
}

/** The signed-in user's answer. Approving gives the device ten more minutes to collect its key. */
export async function approveDevice(env: Env, userId: string, rawCode: unknown, allow: unknown) {
  const code = parseUserCode(rawCode);
  if (!code) throw new BadRequest('Enter the 8-letter code that Oyl shows, like BCDF-GHJK.');
  if (typeof allow !== 'boolean') throw new BadRequest('Choose whether to allow this device.');
  await ensureServerTable(env, 'sync_device_code');
  const decided = await Surreal.system(env).query<{ device_name: string }[]>(
    `UPDATE type::record($id) SET status = $status, owner = type::record($owner),
       expires_at = IF $allow { time::now() + ${DEVICE_SECONDS}s } ELSE { expires_at }
     WHERE status = 'pending' AND expires_at > time::now() AND consumed_at = NONE
     RETURN device_name`,
    { id: codeId(code), status: allow ? 'approved' : 'denied', owner: userId, allow },
  );
  if (!decided?.length) {
    if (await openCode(env, code)) throw new Conflict('This sign-in was already answered.');
    throw new NotFound(STALE);
  }
  return { status: allow ? 'approved' : 'denied', device_name: decided[0].device_name };
}

export interface PollAnswer {
  status: 200 | 202 | 400 | 403 | 410 | 429;
  body: Record<string, unknown>;
  retryAfter?: number;
}

const expired = (): PollAnswer => ({
  status: 410,
  body: { error: 'This sign-in has expired or was already used. Start again.', code: 'expired_token' },
});

/**
 * One poll of the device: 202 while the user has not answered, 429 when it
 * polls faster than `interval`, 403 once declined, 410 for an unknown, expired
 * or used code, and 200 with the sync key once approved (that one time only).
 */
export async function pollDevice(env: Env, rawDeviceCode: unknown): Promise<PollAnswer> {
  if (typeof rawDeviceCode !== 'string' || !/^[A-Za-z0-9_-]{43}$/.test(rawDeviceCode)) {
    return { status: 400, body: { error: 'Send the `device_code` from POST /api/sync/device.', code: 'invalid_request' } };
  }
  await ensureServerTable(env, 'sync_device_code');
  const db = Surreal.system(env);
  const row = await db.query<CodeRow | null>(
    `RETURN (SELECT * FROM sync_device_code WHERE device_hash = $hash AND expires_at > time::now() AND consumed_at = NONE LIMIT 1)[0]`,
    { hash: await sha256Hex(rawDeviceCode) },
  );
  if (!row) return expired();

  if (row.status === 'pending') {
    // A second of slack for timers that fire a little early.
    const polled = await db.query<unknown[]>(
      `UPDATE type::record($id) SET polled_at = time::now()
       WHERE polled_at = NONE OR polled_at <= time::now() - ${POLL_SECONDS - 1}s RETURN id`,
      { id: row.id },
    );
    if (!polled?.length) {
      return { status: 429, body: { error: 'Polling too fast.', code: 'slow_down', interval: POLL_SECONDS }, retryAfter: POLL_SECONDS };
    }
    return { status: 202, body: { status: 'pending', interval: POLL_SECONDS }, retryAfter: POLL_SECONDS };
  }

  if (row.status === 'denied') {
    await db.query('UPDATE type::record($id) SET consumed_at = time::now() WHERE consumed_at = NONE', { id: row.id });
    return { status: 403, body: { error: 'The sign-in was declined in the browser.', code: 'access_denied' } };
  }

  const token = `${SYNC_KEY_PREFIX}${randomKey(32)}`;
  const connection = newKey('api_key');
  try {
    await db.queryAll(
      `BEGIN TRANSACTION;
       LET $claimed = UPDATE type::record($id) SET consumed_at = time::now()
         WHERE status = 'approved' AND owner != NONE AND consumed_at = NONE AND expires_at > time::now()
         RETURN owner, device_name;
       IF array::len($claimed) != 1 { THROW 'sync_code_used'; };
       CREATE type::record($connection) CONTENT {
         owner: $claimed[0].owner, name: 'Oyl on ' + $claimed[0].device_name, prefix: $prefix, key_hash: $unused
       };
       CREATE type::record($device) CONTENT {
         owner: $claimed[0].owner, connection: type::record($connection), name: $claimed[0].device_name, token_hash: $token_hash
       };
       COMMIT TRANSACTION;`,
      {
        id: row.id,
        connection,
        device: newKey('sync_device'),
        prefix: token.slice(0, SYNC_KEY_PREFIX.length + 4),
        // The connection row's own hash belongs to no key: only sync_device opens sync.
        unused: await sha256Hex(randomKey(32)),
        token_hash: await sha256Hex(token),
      },
    );
  } catch (error) {
    // Another poll of the same code collected the key first.
    if (error instanceof Error && /sync_code_used|Transaction conflict/i.test(error.message)) return expired();
    throw error;
  }
  const account = await db.query<{ id: string; name: string; email: string } | null>(
    'RETURN (SELECT id, name, email FROM type::record($owner))[0]',
    { owner: row.owner },
  );
  return { status: 200, body: { token, account: account ?? { id: row.owner, name: '', email: '' } } };
}
