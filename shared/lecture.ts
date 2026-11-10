/**
 * Shared lecture sections for the search index and the student's progress UI.
 * Splits a lecture's markdown into search chunks: one heading section at a
 * time, paragraphs packed up to ~TARGET characters, each chunk carrying the
 * heading path it sits under. Deterministic, so every workflow step that
 * re-chunks the same content gets the same list.
 */
import { latexToMarkdown } from './latex';

/**
 * The Markdown of a material, whatever its kind: LaTeX source (`tex`) is
 * converted, everything else already is Markdown. Sections, search and the
 * tutor all read this.
 */
export function lectureMarkdown(kind: string, content: string): string {
  return kind === 'tex' ? latexToMarkdown(content ?? '') : (content ?? '');
}

export interface Chunk {
  heading: string | null;
  text: string;
}

const TARGET = 1400;
const MAX = 2200;

export function chunkMarkdown(markdown: string): Chunk[] {
  const lines = cleanForIndex(markdown).split('\n');
  const path: string[] = [];
  const sections: { heading: string | null; body: string[] }[] = [{ heading: null, body: [] }];
  let inFence = false;

  for (const line of lines) {
    if (/^\s*(```|~~~)/.test(line)) inFence = !inFence;
    const h = !inFence && /^(#{1,6})\s+(.*)$/.exec(line);
    if (h) {
      const level = h[1].length;
      path.length = level - 1;
      path[level - 1] = h[2].trim();
      sections.push({ heading: path.filter(Boolean).join(' > '), body: [] });
    } else {
      sections[sections.length - 1].body.push(line);
    }
  }

  const chunks: Chunk[] = [];
  for (const section of sections) {
    const paragraphs = section.body
      .join('\n')
      .split(/\n{2,}/)
      .map((p) => p.trim())
      .filter(Boolean)
      .flatMap(splitLong);
    let buf = '';
    for (const p of paragraphs) {
      if (buf && buf.length + p.length + 2 > TARGET) {
        chunks.push({ heading: section.heading, text: buf });
        buf = '';
      }
      buf = buf ? `${buf}\n\n${p}` : p;
    }
    if (buf) chunks.push({ heading: section.heading, text: buf });
  }
  return chunks.filter((c) => c.text.replace(/\W/g, '').length > 20);
}

/** Text the tutor and the index see: figures become their captions, page markers go. */
export function cleanForIndex(markdown: string): string {
  return markdown
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/!\[([^\]]*)\]\(asset:[A-Za-z0-9_-]+\)/g, (_m, alt: string) => (alt ? `[Figure: ${alt}]` : ''))
    .replace(/\n{3,}/g, '\n\n')
    .trim();
}

function splitLong(p: string): string[] {
  if (p.length <= MAX) return [p];
  const out: string[] = [];
  const sentences = p.split(/(?<=[.!?])\s+/);
  let buf = '';
  for (const s of sentences) {
    if (buf && buf.length + s.length + 1 > TARGET) {
      out.push(buf);
      buf = '';
    }
    buf = buf ? `${buf} ${s}` : s;
    while (buf.length > MAX) {
      out.push(buf.slice(0, MAX));
      buf = buf.slice(MAX);
    }
  }
  if (buf) out.push(buf);
  return out;
}

/** The text that gets embedded: heading path first, so it carries context. */
export const embedText = (c: Chunk) => (c.heading ? `${c.heading}\n\n${c.text}` : c.text);

/** The PDF pages a chunk was taken from, first and last. */
export interface ChunkPages {
  page: number;
  pageEnd: number;
}

const PAGE_MARKER = /<!--\s*page\s+(\d+)\s*-->/gi;
const PROBE = 80;
const normalize = (s: string) => s.replace(/\s+/g, ' ').trim();

/**
 * The pages each chunk of `chunkMarkdown(markdown)` comes from, read from the
 * `<!-- page N -->` markers the PDF converter writes. Null for every chunk of
 * material without markers (typed or Markdown lectures). The chunks are only
 * located, never changed: their positions are the student's progress.
 */
export function chunkPages(markdown: string, chunks: Chunk[]): (ChunkPages | null)[] {
  const segments: { page: number; text: string }[] = [];
  let page: number | null = null;
  let last = 0;
  for (const m of markdown.matchAll(PAGE_MARKER)) {
    // Text before the first marker counts as that marker's page.
    segments.push({ page: page ?? Number(m[1]), text: markdown.slice(last, m.index) });
    page = Number(m[1]);
    last = m.index + m[0].length;
  }
  if (page === null) return chunks.map(() => null);
  segments.push({ page, text: markdown.slice(last) });

  // The cleaned lecture as one whitespace-normalized string, with the offset
  // at which each page starts. Chunk text normalizes to a substring of it.
  let flat = '';
  const starts: { at: number; page: number }[] = [];
  for (const s of segments) {
    const text = normalize(cleanForIndex(s.text));
    if (!text) continue;
    if (flat) flat += ' ';
    starts.push({ at: flat.length, page: s.page });
    flat += text;
  }
  if (!starts.length) return chunks.map(() => null);
  const pageAt = (offset: number) => {
    let p = starts[0].page;
    for (const s of starts) {
      if (s.at > offset) break;
      p = s.page;
    }
    return p;
  };

  let cursor = 0;
  let previous: ChunkPages | null = null;
  return chunks.map((c) => {
    const text = normalize(c.text);
    const start = flat.indexOf(text.slice(0, PROBE), cursor);
    if (!text || start < 0) return previous;
    let end = start + text.length;
    if (!flat.startsWith(text, start)) {
      const tail = text.slice(-PROBE);
      const at = flat.indexOf(tail, start);
      end = at >= 0 ? at + tail.length : start + PROBE;
    }
    cursor = end;
    previous = { page: pageAt(start), pageEnd: pageAt(Math.max(start, end - 1)) };
    return previous;
  });
}
