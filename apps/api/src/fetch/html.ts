/**
 * Just enough HTML for course pages: the title, every link with its text and
 * the heading it sits under, and the readable text as Markdown. A small
 * tokenizer rather than a DOM: it runs the same in the Worker and in tests,
 * and course sites are simple. Scripts, styles and forms are skipped; images
 * keep only their alt text, so nothing is loaded from the page later.
 */

export interface PageLink {
  /** Absolute, without the #fragment. */
  url: string;
  text: string;
  /** The nearest heading above the link, if any. */
  heading: string | null;
}

export interface ParsedPage {
  title: string | null;
  links: PageLink[];
  /** The page's main content (`<main>`, else `<article>`, else the body) as Markdown. */
  markdown: string;
}

const SKIP = new Set(['title', 'script', 'style', 'noscript', 'template', 'svg', 'canvas', 'iframe', 'object', 'select', 'button', 'form']);
const BLOCK = new Set([
  'p', 'div', 'section', 'article', 'main', 'header', 'footer', 'aside', 'nav', 'ul', 'ol', 'table', 'thead', 'tbody',
  'tr', 'blockquote', 'figure', 'figcaption', 'dl', 'dt', 'dd', 'address', 'details', 'summary', 'hr', 'body',
]);
const HEADINGS: Record<string, number> = { h1: 1, h2: 2, h3: 3, h4: 4, h5: 5, h6: 6 };

const TOKEN = /<!--[\s\S]*?-->|<!\[CDATA\[[\s\S]*?\]\]>|<![^>]*>|<\/?([a-zA-Z][a-zA-Z0-9-]*)((?:[^>"']|"[^"]*"|'[^']*')*)>/g;
const ATTR = /([^\s"'>/=]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+)))?/g;

const NAMED: Record<string, string> = {
  amp: '&', lt: '<', gt: '>', quot: '"', apos: "'", nbsp: ' ', ndash: '-', mdash: '-', hellip: '...',
  lsquo: "'", rsquo: "'", ldquo: '"', rdquo: '"', copy: '(c)', reg: '(R)', times: 'x', middot: '·', bull: '•',
  auml: 'ä', ouml: 'ö', uuml: 'ü', Auml: 'Ä', Ouml: 'Ö', Uuml: 'Ü', szlig: 'ß', eacute: 'é', egrave: 'è', agrave: 'à',
};

export function decodeEntities(text: string): string {
  return text.replace(/&(#x[0-9a-f]+|#\d+|[a-z]+);?/gi, (m, e: string) => {
    if (e[0] === '#') {
      const code = e[1] === 'x' || e[1] === 'X' ? parseInt(e.slice(2), 16) : parseInt(e.slice(1), 10);
      return Number.isFinite(code) && code > 0 && code < 0x110000 ? String.fromCodePoint(code) : m;
    }
    return NAMED[e] ?? NAMED[e.toLowerCase()] ?? m;
  });
}

function attributes(raw: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const m of raw.matchAll(ATTR)) out[m[1].toLowerCase()] = decodeEntities(m[2] ?? m[3] ?? m[4] ?? '');
  return out;
}

function resolve(href: string, base: URL): URL | null {
  try {
    const u = new URL(href.trim(), base);
    if (u.protocol !== 'https:' && u.protocol !== 'http:') return null;
    u.hash = '';
    return u;
  } catch {
    return null;
  }
}

const clean = (s: string) => s.replace(/\s+/g, ' ').trim();

interface Tag { name: string; close: boolean; selfClosing: boolean; attrs: string }

function* tags(html: string): Generator<Tag | { text: string }> {
  let at = 0;
  for (const m of html.matchAll(TOKEN)) {
    const start = m.index!;
    if (start > at) yield { text: html.slice(at, start) };
    at = start + m[0].length;
    if (!m[1]) continue; // comment, doctype, CDATA
    const attrs = m[2] ?? '';
    yield { name: m[1].toLowerCase(), close: m[0][1] === '/', selfClosing: /\/\s*$/.test(attrs), attrs };
  }
  if (at < html.length) yield { text: html.slice(at) };
}

/** The part of the page worth reading: `<main>`, else `<article>`, else everything. */
function mainContent(html: string): string {
  for (const name of ['main', 'article']) {
    const open = new RegExp(`<${name}\\b[^>]*>`, 'i').exec(html);
    if (!open) continue;
    const close = html.toLowerCase().lastIndexOf(`</${name}>`);
    if (close > open.index) return html.slice(open.index + open[0].length, close);
  }
  return html;
}

export function parseHtml(html: string, pageUrl: string | URL): ParsedPage {
  let base = new URL(String(pageUrl));
  const baseTag = /<base\b[^>]*\bhref\s*=\s*["']?([^"'\s>]+)/i.exec(html);
  if (baseTag) base = resolve(decodeEntities(baseTag[1]), base) ?? base;
  const titleTag = /<title\b[^>]*>([\s\S]*?)<\/title>/i.exec(html);
  const title = titleTag ? clean(decodeEntities(titleTag[1])) || null : null;

  // Links, from the whole page: course sites often list files in a sidebar.
  const links: PageLink[] = [];
  const seen = new Set<string>();
  let heading: string | null = null;
  let headingText: string[] | null = null;
  let anchor: { url: URL | null; text: string[] } | null = null;
  let skip = 0;
  for (const t of tags(html)) {
    if ('text' in t) {
      if (skip) continue;
      const text = decodeEntities(t.text);
      headingText?.push(text);
      anchor?.text.push(text);
      continue;
    }
    if (SKIP.has(t.name)) {
      if (t.close) skip = Math.max(0, skip - 1);
      else if (!t.selfClosing) skip++;
      continue;
    }
    if (skip) continue;
    if (t.name in HEADINGS) {
      if (!t.close) headingText = [];
      else if (headingText) {
        heading = clean(headingText.join('')) || heading;
        headingText = null;
      }
    } else if (t.name === 'a') {
      if (!t.close) {
        const href = attributes(t.attrs).href;
        anchor = { url: href ? resolve(href, base) : null, text: [] };
      } else if (anchor) {
        if (anchor.url && !seen.has(anchor.url.href)) {
          seen.add(anchor.url.href);
          links.push({ url: anchor.url.href, text: clean(anchor.text.join('')), heading });
        }
        anchor = null;
      }
    } else if (t.name === 'img' && !t.close) {
      const alt = attributes(t.attrs).alt;
      if (alt) anchor?.text.push(alt);
    }
  }

  return { title, links, markdown: htmlToMarkdown(mainContent(html), base) };
}

/** HTML (a page body, a Canvas page) to Markdown the tutor can read. */
export function htmlToMarkdown(html: string, pageUrl: string | URL): string {
  const base = new URL(String(pageUrl));
  const out: string[] = [];
  const block = (s = '\n\n') => out.push(s);
  let skip = 0;
  let pre = 0;
  const lists: { ordered: boolean; n: number }[] = [];
  const anchors: (URL | null)[] = [];
  let cell = false;

  for (const t of tags(html)) {
    if ('text' in t) {
      if (skip) continue;
      const text = decodeEntities(t.text);
      out.push(pre ? text : text.replace(/\s+/g, ' '));
      continue;
    }
    const { name, close } = t;
    if (SKIP.has(name)) {
      if (close) skip = Math.max(0, skip - 1);
      else if (!t.selfClosing) skip++;
      continue;
    }
    if (skip) continue;
    if (name in HEADINGS) {
      block();
      if (!close) out.push(`${'#'.repeat(HEADINGS[name])} `);
    } else if (name === 'br') {
      out.push('\n');
    } else if (name === 'li') {
      if (!close) {
        const list = lists[lists.length - 1];
        const indent = '  '.repeat(Math.max(0, lists.length - 1));
        out.push(`\n${indent}${list?.ordered ? `${++list.n}.` : '-'} `);
      }
    } else if (name === 'ul' || name === 'ol') {
      if (!close) lists.push({ ordered: name === 'ol', n: 0 });
      else lists.pop();
      block();
    } else if (name === 'pre') {
      if (!close) {
        block();
        out.push('```\n');
        pre++;
      } else {
        out.push('\n```');
        pre = Math.max(0, pre - 1);
        block();
      }
    } else if (name === 'code' && !pre) {
      out.push('`');
    } else if (name === 'strong' || name === 'b') {
      out.push('**');
    } else if (name === 'em' || name === 'i') {
      out.push('_');
    } else if (name === 'a') {
      if (!close) {
        const href = attributes(t.attrs).href;
        const url = href ? resolve(href, base) : null;
        anchors.push(url);
        if (url) out.push('[');
      } else {
        const url = anchors.pop();
        if (url) out.push(`](${url.href})`);
      }
    } else if (name === 'img' && !close) {
      const alt = attributes(t.attrs).alt?.trim();
      if (alt) out.push(`[Image: ${alt}]`);
    } else if (name === 'td' || name === 'th') {
      if (!close) {
        out.push(cell ? ' | ' : '');
        cell = true;
      }
    } else if (name === 'tr') {
      cell = false;
      out.push('\n');
    } else if (name === 'hr' && !close) {
      block('\n\n---\n\n');
    } else if (name === 'blockquote' && !close) {
      block('\n\n> ');
    } else if (BLOCK.has(name)) {
      block();
    }
  }

  // Tidy whitespace outside code blocks; keep list indents.
  let fenced = false;
  return out
    .join('')
    .replace(/\[\s*\]\([^)]*\)/g, '') // links without text
    .replace(/\*\*\s*\*\*/g, '')
    .split('\n')
    .map((line) => {
      if (line.startsWith('```')) fenced = !fenced;
      return fenced ? line : line.replace(/[ \t]+$/g, '').replace(/^[ \t]+(?![-\d])/g, '');
    })
    .join('\n')
    .replace(/\n{3,}/g, '\n\n')
    .trim();
}
