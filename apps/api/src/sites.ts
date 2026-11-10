import { sha256Hex } from './auth';
import type { Env } from './env';
import { ensureServerTable, PART_BYTES, readLimited } from './service';
import { BadRequest, Conflict, NotFound, Surreal, fromBase64, recordId, toBase64 } from './surreal';

/**
 * Static websites a student uploaded to a course, audio and all. Each file is
 * a `site_file` row with its bytes in the `materials` bucket, and everything
 * is served publicly by the site's unguessable id, like a lecture's images: a
 * page cannot send the session token, and a site is meant to be opened by link.
 *
 * Sites are served from the app's own origin and hold whatever the student
 * uploaded, scripts included. So every response carries a CSP `sandbox`
 * without `allow-same-origin`: the browser gives the page an opaque origin, and
 * it cannot reach the app's storage, session or API as the signed-in student.
 */

export const MAX_SITE_FILE_BYTES = 30 * 1024 * 1024;
export const MAX_SITE_BYTES = 100 * 1024 * 1024;
export const MAX_SITE_FILES = 2000;
/** The most one range answer holds. The player asks again for the next bytes. */
const MAX_RANGE_PARTS = 4;
const PREFIX = '/site-files/';

const TYPES: Record<string, string> = {
  html: 'text/html; charset=utf-8',
  htm: 'text/html; charset=utf-8',
  js: 'text/javascript; charset=utf-8',
  mjs: 'text/javascript; charset=utf-8',
  css: 'text/css; charset=utf-8',
  json: 'application/json; charset=utf-8',
  map: 'application/json; charset=utf-8',
  xml: 'application/xml; charset=utf-8',
  txt: 'text/plain; charset=utf-8',
  md: 'text/plain; charset=utf-8',
  csv: 'text/csv; charset=utf-8',
  vtt: 'text/vtt; charset=utf-8',
  svg: 'image/svg+xml',
  png: 'image/png',
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  gif: 'image/gif',
  webp: 'image/webp',
  avif: 'image/avif',
  ico: 'image/x-icon',
  mp3: 'audio/mpeg',
  m4a: 'audio/mp4',
  aac: 'audio/aac',
  wav: 'audio/wav',
  ogg: 'audio/ogg',
  oga: 'audio/ogg',
  opus: 'audio/ogg',
  flac: 'audio/flac',
  mp4: 'video/mp4',
  webm: 'video/webm',
  woff: 'font/woff',
  woff2: 'font/woff2',
  ttf: 'font/ttf',
  otf: 'font/otf',
  wasm: 'application/wasm',
  pdf: 'application/pdf',
};

const extension = (path: string) => /\.([a-z0-9]+)$/i.exec(path)?.[1]?.toLowerCase() ?? '';
export const siteMime = (path: string) => TYPES[extension(path)] ?? 'application/octet-stream';
const isHtml = (path: string) => /^html?$/.test(extension(path));

/** The path as stored, or null for one that could not name a file of the site. */
export function sitePath(raw: string): string | null {
  if (!raw || raw.length > 512 || /[\\\0]/.test(raw)) return null;
  return raw.split('/').some((s) => !s || s === '.' || s === '..') ? null : raw;
}

const fileId = async (site: string, path: string) => `site_file:f${(await sha256Hex(`${site}\n${path}`)).slice(0, 32)}`;
const partPath = (id: string, n: number) => `/sites/${id.slice(id.indexOf(':') + 1)}_${n}`;

/** Stores one file of a site that is still uploading. Sending a path again replaces it. */
export async function storeSiteFile(env: Env, userId: string, rawId: string, rawPath: string, body: ReadableStream<Uint8Array> | null): Promise<void> {
  const site = recordId('site', rawId);
  const path = sitePath(rawPath);
  if (!path) throw new BadRequest(`Not a usable file path: ${rawPath.slice(0, 200)}`);
  await ensureServerTable(env, 'site_file');
  const db = Surreal.system(env);
  const held = await db.query<{ status: string; files: number; bytes: number } | null>(
    `LET $row = (SELECT status FROM type::record($site) WHERE owner = type::record($owner))[0];
     RETURN IF $row { {
       status: $row.status,
       files: count(SELECT id FROM site_file WHERE site = type::record($site)),
       bytes: math::sum(SELECT VALUE size FROM site_file WHERE site = type::record($site))
     } } ELSE { NONE };`,
    { site, owner: userId },
  );
  if (!held) throw new NotFound('Site not found.');
  if (held.status !== 'uploading') throw new Conflict('This site is already published. Upload it again as a new site.');
  if (held.files >= MAX_SITE_FILES) throw new BadRequest(`Sites of up to ${MAX_SITE_FILES} files are supported.`);
  const bytes = await readLimited(body, MAX_SITE_FILE_BYTES);
  if (held.bytes + bytes.byteLength > MAX_SITE_BYTES) throw new BadRequest(`Sites up to ${MAX_SITE_BYTES / 1024 / 1024} MB are supported.`);

  const id = await fileId(site, path);
  const parts = Math.ceil(bytes.byteLength / PART_BYTES);
  // A few at a time, the bucket files before the row that points at them.
  for (let from = 0; from < parts; from += 3) {
    await Promise.all(
      Array.from({ length: Math.min(3, parts - from) }, (_, i) =>
        db.query(`type::file('materials', $key).put(encoding::base64::decode($b64));`, {
          key: partPath(id, from + i),
          b64: toBase64(bytes.subarray((from + i) * PART_BYTES, (from + i + 1) * PART_BYTES)),
        }),
      ),
    );
  }
  await db.query(
    `UPSERT type::record($id) CONTENT { owner: type::record($owner), site: type::record($site), path: $path, size: $size, parts: $parts };`,
    { id, owner: userId, site, path, size: bytes.byteLength, parts },
  );
}

/** Every file is up: lists the site's pages, picks the one it opens on and opens it. */
export async function finishSite(env: Env, userId: string, rawId: string): Promise<{ entry: string }> {
  const site = recordId('site', rawId);
  const db = Surreal.system(env);
  const owned = await db.query<string | null>(
    'RETURN (SELECT VALUE id FROM type::record($site) WHERE owner = type::record($owner))[0]',
    { site, owner: userId },
  );
  if (!owned) throw new NotFound('Site not found.');
  const files = await db.query<{ path: string; size: number }[]>(
    'SELECT path, size FROM site_file WHERE site = type::record($site)',
    { site },
  );
  const pages = files.map((f) => f.path).filter(isHtml).sort();
  if (!pages.length) throw new BadRequest('A site needs at least one HTML page.');
  const entry = pages.find((p) => p.toLowerCase() === 'index.html') ?? pages[0];
  await db.query(
    `UPDATE type::record($site) SET status = 'ready', entry = $entry, pages = $pages, file_count = $count, size = $size;`,
    { site, entry, pages: JSON.stringify(pages), count: files.length, size: files.reduce((n, f) => n + f.size, 0) },
  );
  return { entry };
}

/** A `Range` header against a file's size: the bytes asked for, null for the whole file, 'invalid' for a 416. */
export function parseRange(header: string | undefined, size: number): { start: number; end: number } | 'invalid' | null {
  // Several ranges in one request are answered with the whole file.
  const m = header ? /^bytes=(\d*)-(\d*)$/.exec(header.trim()) : null;
  if (!m || (!m[1] && !m[2])) return null;
  let start: number;
  let end: number;
  if (!m[1]) {
    start = Math.max(0, size - Number(m[2]));
    end = size - 1;
  } else {
    start = Number(m[1]);
    end = m[2] ? Math.min(Number(m[2]), size - 1) : size - 1;
  }
  return start > end || start >= size ? 'invalid' : { start, end };
}

// Opaque-origin pages throw on localStorage and document.cookie, and a page
// that reads them unguarded would stop right there. They get a copy that lasts
// as long as the page.
const STORAGE_SHIM = `<script>(function(){for(const n of['localStorage','sessionStorage']){try{if(window[n])continue}catch(e){}const m=new Map();Object.defineProperty(window,n,{configurable:true,value:{get length(){return m.size},key:i=>[...m.keys()][i]??null,getItem:k=>m.has(String(k))?m.get(String(k)):null,setItem:(k,v)=>{m.set(String(k),String(v))},removeItem:k=>{m.delete(String(k))},clear:()=>m.clear()}})}try{document.cookie}catch(e){Object.defineProperty(document,'cookie',{configurable:true,get:()=>'',set(){}})}})()</script>`;

/** The page with the storage shim as the first thing in it, after the doctype and `<head>`. */
export function withStorageShim(html: Uint8Array): Uint8Array {
  // One character per byte, so a match's offsets are byte offsets.
  const start = String.fromCharCode(...html.subarray(0, 8192));
  const m = /<head(\s[^>]*)?>/i.exec(start) ?? /<!doctype[^>]*>/i.exec(start);
  const at = m ? m.index + m[0].length : 0;
  const shim = new TextEncoder().encode(STORAGE_SHIM);
  const out = new Uint8Array(html.byteLength + shim.byteLength);
  out.set(html.subarray(0, at));
  out.set(shim, at);
  out.set(html.subarray(at), at + shim.byteLength);
  return out;
}

function headers(extra: Record<string, string> = {}): Record<string, string> {
  return {
    // The page runs without the app's origin, see the top of this file. Popups
    // escape the sandbox so a link out of the site opens a normal page.
    'content-security-policy': 'sandbox allow-scripts allow-forms allow-modals allow-downloads allow-popups allow-popups-to-escape-sandbox',
    'x-content-type-options': 'nosniff',
    'x-robots-tag': 'noindex',
    // Keeps the site's unguessable address away from the pages it links to.
    'referrer-policy': 'no-referrer',
    // The opaque origin makes the site's own module scripts and fetches cross-origin.
    'access-control-allow-origin': '*',
    'access-control-expose-headers': 'accept-ranges, content-length, content-range',
    'cross-origin-resource-policy': 'cross-origin',
    ...extra,
  };
}

/** Parts `first` to `last` of a file, joined. */
async function readParts(db: Surreal, id: string, first: number, last: number): Promise<Uint8Array> {
  const chunks: Uint8Array[] = [];
  // A few per round: each comes back base64 in JSON.
  for (let n = first; n <= last; n += 3) {
    const round = Array.from({ length: Math.min(3, last - n + 1) }, (_, i) => n + i);
    const read = await Promise.all(
      round.map((part) => db.query<string>(`RETURN encoding::base64::encode(type::file('materials', $key).get());`, { key: partPath(id, part) })),
    );
    chunks.push(...read.map(fromBase64));
  }
  const out = new Uint8Array(chunks.reduce((n, c) => n + c.byteLength, 0));
  let off = 0;
  for (const c of chunks) {
    out.set(c, off);
    off += c.byteLength;
  }
  return out;
}

const notFound = () => new Response('Not found', { status: 404, headers: headers({ 'content-type': 'text/plain; charset=utf-8' }) });

/** One request under /site-files/. `pathname` is the request's whole path. */
export async function serveSiteFile(env: Env, rawSite: string, pathname: string, range: string | undefined): Promise<Response> {
  const site = recordId('site', rawSite);
  const base = `${PREFIX}${rawSite}`;
  const rest = pathname.slice(base.length);
  // The trailing slash makes a page's relative links resolve inside the site.
  if (!rest) return new Response(null, { status: 308, headers: headers({ location: `${base}/` }) });

  let path: string;
  try {
    path = decodeURIComponent(rest.slice(1));
  } catch {
    return notFound();
  }
  const db = Surreal.system(env);
  if (!path) {
    path = (await db.query<string | null>('RETURN (SELECT VALUE entry FROM type::record($site))[0]', { site })) ?? '';
    if (!path) return notFound();
  } else if (path.endsWith('/')) {
    path += 'index.html';
  }

  const stat = (p: string) => fileId(site, p).then((id) =>
    db.query<{ size: number; parts: number } | null>('RETURN (SELECT size, parts FROM type::record($id))[0]', { id }).then((row) => (row ? { id, ...row } : null)));
  const file = await stat(path);
  if (!file) {
    // A folder asked for without its slash.
    if (!extension(path) && (await stat(`${path}/index.html`))) {
      return new Response(null, { status: 308, headers: headers({ location: `${pathname}/` }) });
    }
    return notFound();
  }

  const type = siteMime(path);
  const read = (first: number, last: number) => (file.parts ? readParts(db, file.id, first, last) : Promise.resolve(new Uint8Array()));
  // No file changes once its site is published: only deleting the site does.
  const cache = { 'content-type': type, 'cache-control': 'public, max-age=3600' };

  if (isHtml(path)) {
    const page = withStorageShim(await read(0, file.parts - 1));
    return new Response(page, { headers: headers({ ...cache, 'content-length': String(page.byteLength) }) });
  }

  const asked = parseRange(range, file.size);
  if (asked === 'invalid') {
    return new Response(null, { status: 416, headers: headers({ 'content-range': `bytes */${file.size}` }) });
  }
  if (!asked) {
    const body = await read(0, file.parts - 1);
    return new Response(body, { headers: headers({ ...cache, 'accept-ranges': 'bytes', 'content-length': String(body.byteLength) }) });
  }

  const first = Math.floor(asked.start / PART_BYTES);
  const last = Math.min(Math.floor(asked.end / PART_BYTES), first + MAX_RANGE_PARTS - 1);
  const held = await read(first, last);
  const offset = asked.start - first * PART_BYTES;
  const end = Math.min(asked.end, (last + 1) * PART_BYTES - 1);
  const body = held.subarray(offset, offset + (end - asked.start + 1));
  return new Response(body, {
    status: 206,
    headers: headers({ ...cache, 'accept-ranges': 'bytes', 'content-length': String(body.byteLength), 'content-range': `bytes ${asked.start}-${end}/${file.size}` }),
  });
}
