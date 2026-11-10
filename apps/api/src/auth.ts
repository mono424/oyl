import type { MiddlewareHandler } from 'hono';
import type { AppEnv, Env } from './env';
import { Surreal, SurrealAuthError } from './surreal';

// Verified tokens, per isolate, for a minute: a call screen or an upload makes
// several requests in a row and each would otherwise cost a database round trip.
const verified = new Map<string, { userId: string; until: number }>();

/**
 * Who sent this token? Asks SurrealDB itself, so there is no JWT key to share
 * with the API: a token SurrealDB accepts for the `account` access is valid.
 */
export async function verifyUserToken(env: Env, token: string): Promise<string | null> {
  const hit = verified.get(token);
  if (hit && hit.until > Date.now()) return hit.userId;
  try {
    const [userId, access] = await Surreal.as(env, token).query<[string | null, string | null]>('RETURN [$auth.id, $access]');
    if (!userId || access !== 'account') return null;
    if (verified.size > 1000) verified.clear();
    verified.set(token, { userId, until: Date.now() + 60_000 });
    return userId;
  } catch (err) {
    if (err instanceof SurrealAuthError) return null;
    throw err;
  }
}

export const bearer = (header: string | undefined | null) => {
  const m = /^Bearer\s+(.+)$/i.exec(header ?? '');
  return m ? m[1].trim() : null;
};

/** Requires a signed-in web app user; sets `c.var.caller`. */
export const requireUser: MiddlewareHandler<AppEnv> = async (c, next) => {
  const token = bearer(c.req.header('authorization'));
  const userId = token ? await verifyUserToken(c.env, token) : null;
  if (!token || !userId) return c.json({ error: 'Your session has expired. Sign in again.' }, 401);
  c.set('caller', { userId, token });
  await next();
};

export async function sha256Hex(text: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
}

/** Constant-time string comparison for shared secrets. */
export function safeEqual(a: string, b: string): boolean {
  const ea = new TextEncoder().encode(a);
  const eb = new TextEncoder().encode(b);
  if (ea.length !== eb.length) return false;
  let diff = 0;
  for (let i = 0; i < ea.length; i++) diff |= ea[i] ^ eb[i];
  return diff === 0;
}

export function randomKey(bytes = 32): string {
  const buf = crypto.getRandomValues(new Uint8Array(bytes));
  return btoa(String.fromCharCode(...buf)).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '');
}
