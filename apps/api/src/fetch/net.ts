import { readLimited } from '../service';
import { BadRequest } from '../surreal';

/**
 * Requests a source makes on the student's behalf: to an address they typed
 * (a course page, a Canvas instance) and wherever it redirects. Every hop must
 * be public https by name, never an IP literal, localhost or an internal
 * suffix, and a Canvas token only goes to the origin it was created for.
 */

const INTERNAL_SUFFIXES = ['.local', '.localhost', '.internal', '.intranet', '.lan', '.home', '.corp', '.arpa'];
const MAX_REDIRECTS = 5;
const TIMEOUT_MS = 30_000;

/** The URL, if a source may fetch it. Throws a user-facing error otherwise. */
export function publicUrl(raw: string | URL, what = 'The address'): URL {
  let url: URL;
  try {
    url = new URL(String(raw).trim());
  } catch {
    throw new BadRequest(`${what} is not a valid URL.`);
  }
  if (url.protocol !== 'https:') throw new BadRequest(`${what} must start with https://.`);
  if (url.username || url.password) throw new BadRequest(`${what} must not contain a login.`);
  if (url.port && url.port !== '443') throw new BadRequest(`${what} must use the standard https port.`);
  const host = url.hostname.toLowerCase().replace(/\.$/, '');
  // IPv4 literals (also the short and hex forms URL normalizes to dotted
  // decimal) and IPv6 literals in brackets.
  if (/^[\d.]+$/.test(host) || host.startsWith('[') || host.includes(':')) {
    throw new BadRequest(`${what} must use a domain name, not an IP address.`);
  }
  if (!host.includes('.') || host === 'localhost' || INTERNAL_SUFFIXES.some((s) => host.endsWith(s))) {
    throw new BadRequest(`${what} must be a public website.`);
  }
  url.hash = '';
  return url;
}

export interface SafeFetchInit {
  method?: 'GET' | 'HEAD';
  accept?: string;
  /** Sent to the first URL's origin only, never to a redirect elsewhere. */
  authorization?: string;
  timeoutMs?: number;
}

/**
 * fetch with every redirect checked by hand. Answers the last response, with
 * `url` set to where it came from.
 */
export async function safeFetch(raw: string | URL, init: SafeFetchInit = {}): Promise<{ res: Response; url: URL }> {
  let url = publicUrl(raw);
  const origin = url.origin;
  const signal = AbortSignal.timeout(init.timeoutMs ?? TIMEOUT_MS);
  for (let hop = 0; ; hop++) {
    const headers: Record<string, string> = { 'user-agent': 'TalkToBolo source fetcher (+https://talktobolo.stg.spky.cloud)' };
    if (init.accept) headers.accept = init.accept;
    if (init.authorization && url.origin === origin) headers.authorization = init.authorization;
    let res: Response;
    try {
      res = await fetch(url, { method: init.method ?? 'GET', headers, redirect: 'manual', signal });
    } catch {
      if (signal.aborted) throw new BadRequest(`${url.host} took too long to answer.`);
      throw new BadRequest(`${url.host} could not be reached.`);
    }
    const location = res.headers.get('location');
    if (res.status >= 300 && res.status < 400 && location) {
      await res.body?.cancel();
      if (hop >= MAX_REDIRECTS) throw new BadRequest(`${url.host} redirected too many times.`);
      url = publicUrl(new URL(location, url), 'A redirect');
      continue;
    }
    return { res, url };
  }
}

/** A response body as text, refusing more than `max` bytes. */
export async function readText(res: Response, max: number): Promise<string> {
  return new TextDecoder().decode(await readLimited(res.body, max));
}
