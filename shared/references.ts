/**
 * The references a draft mentions, for turning it into a document with its
 * papers beside it (apps/web/src/lib/documents.ts, apps/api/src/documents.ts).
 * A reference is a link after its authors and year, as a reading list or a
 * draft writes it: "Stark et al. (2019): [Broadband polarimetric …](https://doi.org/…)",
 * also "Padmanabhan and Prakash (2025, preprint) [title](url)". Each is matched
 * to the course material holding the same paper, when there is one.
 */

export interface FoundReference {
  /** As written: "Stark et al.", "Padmanabhan and Prakash". */
  authors: string;
  year: string;
  title: string;
  url: string;
}

const NAME = String.raw`\p{Lu}[\p{L}'’-]+`;
const REFERENCE = new RegExp(
  String.raw`(${NAME}(?:\s+et\s+al\.?|(?:,\s*${NAME})*\s+(?:and|&)\s+${NAME})?)\s*\((\d{4})[a-z]?[^)\n]*\)\s*[:,.-]?\s*\[([^\]\n]+)\]\((https?:\/\/[^)\s]+)\)`,
  'gu',
);

const cleanTitle = (s: string) =>
  s
    .replace(/^[\s*_]+|[\s*_.]+$/g, '')
    .replace(/\s+/g, ' ')
    .trim();

/** The references in a document's Markdown, each once (by link), in order. */
export function findReferences(markdown: string): FoundReference[] {
  const out: FoundReference[] = [];
  const seen = new Set<string>();
  for (const m of markdown.matchAll(REFERENCE)) {
    const url = m[4];
    if (seen.has(url)) continue;
    seen.add(url);
    out.push({ authors: m[1].replace(/\s+/g, ' ').trim(), year: m[2], title: cleanTitle(m[3]), url });
  }
  return out;
}

const fold = (s: string) => s.toLowerCase().normalize('NFD').replace(/\p{M}/gu, '');
const words = (s: string) => new Set(fold(s).match(/[a-z0-9]{4,}/g) ?? []);

/**
 * The material holding each reference's paper, by first author, year and
 * title words in the material's title ("Midterm paper P02: Stark et al. 2019
 * - Broadband polarimetric …"). A material serves one reference at most.
 */
export function matchMaterials<M extends { id: string; title: string }>(refs: FoundReference[], materials: readonly M[]): (M | null)[] {
  const used = new Set<string>();
  return refs.map((ref) => {
    const author = fold(ref.authors.split(/[\s,]+/)[0] ?? '');
    const want = words(ref.title);
    let best: { m: M; score: number } | null = null;
    for (const m of materials) {
      if (used.has(m.id)) continue;
      const title = fold(m.title);
      if (!author || !title.includes(author) || !title.includes(ref.year)) continue;
      const have = words(m.title);
      const shared = [...want].filter((w) => have.has(w)).length;
      const score = want.size ? shared / want.size : 0;
      if (score >= 0.3 && (!best || score > best.score)) best = { m, score };
    }
    if (best) used.add(best.m.id);
    return best?.m ?? null;
  });
}

/** A material's text as a document starts from it: Markdown, without PDF page marks. */
export function draftMarkdown(markdown: string): string {
  return markdown
    .replace(/^<!-- page \d+ -->\n?/gm, '')
    .replace(/\n{3,}/g, '\n\n')
    .trim();
}

// ── Citations ─────────────────────────────────────────────────────────────
//
// Markdown cites Pandoc style, `[@key]`, `[@key, p. 4]`, `[@a; @b]`; LaTeX
// with natbib's `\cite`, `\citep` and `\citet`. The key is a paper's cite_key
// ("stark2019b"), or its record key for papers from before cite keys.

export const KEY = String.raw`[A-Za-z0-9_][A-Za-z0-9_:-]*`;
const ITEM = String.raw`@${KEY}(?:,[^\]\n;]*)?`;
export const CITATION_SOURCE = String.raw`\[${ITEM}(?:;\s*${ITEM})*\]`;

/** Every Markdown citation, `[@…]`. */
export const citationPattern = () => new RegExp(CITATION_SOURCE, 'g');

/** The cited keys of a Markdown citation, with what follows each ("p. 4"). */
export function parseCitation(src: string): { key: string; locator: string }[] {
  const inner = src.startsWith('[') ? src.slice(1, -1) : src;
  return inner.split(';').flatMap((part) => {
    const m = new RegExp(String.raw`^\s*@(${KEY})\s*(?:,\s*(.*))?$`).exec(part);
    return m ? [{ key: m[1], locator: (m[2] ?? '').trim() }] : [];
  });
}

/**
 * LaTeX's citations as Markdown ones, for the preview, the PDF and counting:
 * `\cite[p.~4]{a,b}` becomes `[@a; @b, p. 4]` (natbib's single optional
 * argument is the page; with two, the first is a note before, left out).
 */
export function texCitesToMarkdown(tex: string): string {
  return tex.replace(/\\(?:cite|citep|citet|parencite|textcite|autocite)\*?(?:\[([^\]]*)\])?(?:\[([^\]]*)\])?\{([^}]*)\}/g, (_m, a?: string, b?: string, keys = '') => {
    const post = (b ?? a ?? '').replace(/~/g, ' ').trim();
    const list = keys.split(',').map((k: string) => k.trim()).filter(Boolean);
    if (!list.length) return '';
    return `[${list.map((k: string, i: number) => `@${k}${post && i === list.length - 1 ? `, ${post}` : ''}`).join('; ')}]`;
  });
}

/**
 * Markdown citations as LaTeX ones, for text written into a LaTeX document:
 * `[@a; @b, p. 4]` becomes `\cite[p.~4]{a,b}`.
 */
export function markdownCitesToTex(md: string): string {
  return md.replace(citationPattern(), (src) => {
    const items = parseCitation(src);
    if (!items.length) return src;
    const post = items.map((i) => i.locator).filter(Boolean).at(-1) ?? '';
    return `\\cite${post ? `[${post.replace(/^p\.\s*/, 'p.~')}]` : ''}{${items.map((i) => i.key).join(',')}}`;
  });
}

/**
 * What gives Markdown away in text bound for a LaTeX document, or null:
 * headings, bold, links. LaTeX itself has none of them.
 */
export function markdownInTex(text: string): string | null {
  if (/^#{1,6}\s/m.test(text)) return 'Markdown headings (# ...): use \\section{...}';
  if (/\*\*[^*\n]+\*\*/.test(text)) return 'Markdown bold (**...**): use \\textbf{...}';
  if (/(^|[^\\!])\[[^\]\n]+\]\(https?:\/\//.test(text)) return 'Markdown links ([text](url)): use \\href{url}{text}';
  if (/^```/m.test(text)) return 'Markdown code fences (```): use verbatim';
  return null;
}

/** How often a text cites each key. LaTeX is read through its `\cite`s. */
export function countCitations(text: string, format: 'markdown' | 'tex' = 'markdown'): Map<string, number> {
  const md = format === 'tex' ? texCitesToMarkdown(text) : text;
  const out = new Map<string, number>();
  for (const m of md.matchAll(citationPattern())) for (const c of parseCitation(m[0])) out.set(c.key, (out.get(c.key) ?? 0) + 1);
  return out;
}

/**
 * A cite key for a new paper, BibTeX style: the first author's surname and
 * the year ("stark2019"), a letter after it when the document has that key
 * already ("stark2019b"). Without authors, the title's first word.
 */
export function makeCiteKey(paper: { authors?: string | null; year?: string | null; title: string }, taken: Iterable<string>): string {
  const used = new Set([...taken].map((k) => k.toLowerCase()));
  const first = (paper.authors ?? '').split(/;|\s+(?:and|&)\s+/)[0]?.trim() ?? '';
  const surname = first.includes(',') ? first.slice(0, first.indexOf(',')) : first.replace(/\s+et\s+al\.?$/i, '').split(/\s+/).at(-1) ?? '';
  const word = (surname || paper.title.split(/\s+/).find((w) => w.length > 3) || paper.title || 'paper')
    .normalize('NFD')
    .replace(/\p{M}/gu, '')
    .toLowerCase()
    .replace(/[^a-z0-9]/g, '');
  const base = `${word || 'paper'}${(paper.year ?? '').match(/\d{4}/)?.[0] ?? ''}`;
  if (!used.has(base)) return base;
  for (let i = 1; i < 26; i++) {
    const key = `${base}${String.fromCharCode(97 + i)}`;
    if (!used.has(key)) return key;
  }
  return `${base}${used.size}`;
}

/** A BibTeX entry for a paper, for the .bib next to a LaTeX document. */
export function bibtexEntry(paper: { key: string; title: string; authors?: string | null; year?: string | null; venue?: string | null; url?: string | null }): string {
  // Characters LaTeX reads as commands; one already escaped stays as it is.
  const esc = (s: string) => s.replace(/(?<!\\)([{}&%#_$])/g, '\\$1');
  // "Stark et al." and a separate "et al." entry both end in BibTeX's "others".
  const authors = (paper.authors ?? '')
    .split(/\s*;\s*/)
    .flatMap((name) => {
      const etAl = name.match(/^(.*?)[\s,]*\bet\s+al\.?$/i);
      return etAl ? [etAl[1], 'others'] : [name];
    })
    .filter(Boolean)
    .join(' and ');
  const fields: [string, string | null | undefined][] = [
    ['title', `{${esc(paper.title)}}`],
    ['author', authors ? esc(authors) : null],
    ['year', paper.year?.match(/\d{4}/)?.[0]],
    ['journal', paper.venue ? esc(paper.venue) : null],
    ['url', paper.url],
  ];
  const body = fields.filter(([, v]) => v).map(([k, v]) => `  ${k} = {${v}}`).join(',\n');
  return `@${paper.venue ? 'article' : 'misc'}{${paper.key},\n${body}\n}`;
}
