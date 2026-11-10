import type { Env } from '../env';
import { UserError, fromBase64, toBase64 } from '../surreal';

/**
 * A source's access token (a Canvas token) at rest: AES-GCM under a key
 * derived from the FETCHER_KEY secret, bound to its fetcher's id, so a copied
 * row decrypts for no other fetcher and a database dump alone reveals nothing.
 */

const VERSION = 'v1';

async function key(env: Env): Promise<CryptoKey> {
  if (!env.FETCHER_KEY) throw new UserError('bCourses sources are not set up on this server yet (FETCHER_KEY is missing).', 503);
  const raw = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(`talktobolo-fetcher:${env.FETCHER_KEY}`));
  return crypto.subtle.importKey('raw', raw, 'AES-GCM', false, ['encrypt', 'decrypt']);
}

export async function sealToken(env: Env, fetcherId: string, token: string): Promise<string> {
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const data = await crypto.subtle.encrypt(
    { name: 'AES-GCM', iv, additionalData: new TextEncoder().encode(fetcherId) },
    await key(env),
    new TextEncoder().encode(token),
  );
  return `${VERSION}.${toBase64(iv)}.${toBase64(new Uint8Array(data))}`;
}

export async function openToken(env: Env, fetcherId: string, sealed: string): Promise<string> {
  const [version, iv, data] = sealed.split('.');
  if (version !== VERSION || !iv || !data) throw new Error('Unknown sealed token format.');
  try {
    const plain = await crypto.subtle.decrypt(
      { name: 'AES-GCM', iv: fromBase64(iv), additionalData: new TextEncoder().encode(fetcherId) },
      await key(env),
      fromBase64(data),
    );
    return new TextDecoder().decode(plain);
  } catch {
    throw new UserError('The saved access token can no longer be read. Paste a new one.', 403);
  }
}
