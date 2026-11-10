import { BadRequest } from '../surreal';
import { type PageLink, parseHtml } from './html';
import type { SourceRow } from './mapping';
import { publicUrl, readText, safeFetch } from './net';

/**
 * A public web page, such as a course website's schedule: every link on it
 * (the mapping keeps the PDFs) and, as one `page` row, the page's own text.
 */

const MAX_HTML = 5 * 1024 * 1024;
const MAX_LINKS = 500;
/** Link texts that say nothing on their own: "Slides" in every week of a schedule. */
const VAGUE = /^((click |at )?(here|this link)|slides?|pdf|link|download|notes?|handout|lecture|video|recording|reading|just the .{0,30}|\d+|.{0,2})$/i;

const extOf = (u: URL) => /\.([a-z0-9]{1,8})$/i.exec(u.pathname)?.[1]?.toLowerCase() ?? null;
const fileName = (u: URL) => {
  try {
    return decodeURIComponent(u.pathname.split('/').pop() || u.hostname);
  } catch {
    return u.pathname.split('/').pop() || u.hostname;
  }
};

export interface WebPage {
  url: string;
  title: string | null;
  markdown: string;
  links: PageLink[];
}

export async function fetchPage(raw: string): Promise<WebPage> {
  const { res, url } = await safeFetch(raw, { accept: 'text/html,application/xhtml+xml' });
  if (!res.ok) {
    await res.body?.cancel();
    throw new BadRequest(`${url.host} answered ${res.status} for this page.`);
  }
  const type = res.headers.get('content-type') ?? '';
  if (type && !/html|xml|text\/plain/i.test(type)) {
    await res.body?.cancel();
    throw new BadRequest('That address is not a web page. Add files directly with Upload instead.');
  }
  const html = await readText(res, MAX_HTML);
  const page = parseHtml(html, url);
  return { url: url.href, title: page.title, markdown: page.markdown, links: page.links };
}

/**
 * A name for each link that reads well in the course's material list and
 * tells the files apart: the link text, or, where that says nothing on its
 * own ("Slides", "here"), its heading and file name. A text that several
 * links share gets the file name too.
 */
function linkName(link: PageLink, u: URL, counts: Map<string, number>): string {
  const text = link.text.replace(/\s+/g, ' ').trim();
  const file = fileName(u).replace(/\.[a-z0-9]{1,8}$/i, '').replace(/[_+]+/g, ' ').trim() || fileName(u);
  if (!text || VAGUE.test(text)) return link.heading ? `${link.heading}: ${file}` : file;
  if ((counts.get(text.toLowerCase()) ?? 0) > 1) return `${text} (${file})`;
  return text;
}

/** The rows of one page: its links, and the page itself (`version`: a hash of its text). */
export function pageRows(page: WebPage, version: string | null = null): SourceRow[] {
  const counts = new Map<string, number>();
  for (const l of page.links) {
    const k = l.text.trim().toLowerCase();
    counts.set(k, (counts.get(k) ?? 0) + 1);
  }
  const rows: SourceRow[] = [{
    id: `page:${page.url}`,
    name: page.title || fileName(new URL(page.url)),
    kind: 'page',
    content_type: 'text/html',
    ext: null,
    url: page.url,
    link: page.url,
    updated_at: version,
    module: null,
    size: null,
  }];
  for (const link of page.links.slice(0, MAX_LINKS)) {
    let u: URL;
    try {
      // Course sites still link files over http; fetch them over https.
      u = publicUrl(link.url.replace(/^http:\/\//i, 'https://'));
    } catch {
      continue; // an IP address, an intranet host
    }
    if (u.href === page.url) continue;
    rows.push({
      id: u.href,
      name: linkName(link, u, counts),
      kind: 'link',
      content_type: null,
      ext: extOf(u),
      url: u.href,
      link: u.href,
      updated_at: null,
      module: link.heading,
      size: null,
    });
  }
  return rows;
}

/**
 * Something that changes when a linked file does: its ETag, else its
 * Last-Modified and length. Null when the server says neither (or no HEAD).
 */
export async function linkVersion(url: string): Promise<string | null> {
  try {
    const { res } = await safeFetch(url, { method: 'HEAD', timeoutMs: 10_000 });
    await res.body?.cancel();
    if (!res.ok) return null;
    const etag = res.headers.get('etag');
    if (etag) return `etag:${etag.replace(/^W\//, '')}`;
    const modified = res.headers.get('last-modified');
    if (modified) return `modified:${modified}|${res.headers.get('content-length') ?? ''}`;
    return null;
  } catch {
    return null;
  }
}
