/**
 * Papers synced from the desktop app (docs/sync-protocol.md): how the pages
 * name and read them.
 */

export interface SyncedFile {
  sha256: string;
  size: number;
}

export const engineLabel = (engine: string | null | undefined) => (engine === 'latex' ? 'LaTeX' : 'Typst');

/** Who wrote a revision: the Mac's name for `device:<name>`, else Web or Agent. */
export function authorLabel(author: string | null | undefined): string {
  if (!author) return '';
  if (author.startsWith('device:')) return author.slice('device:'.length) || 'Mac';
  return author === 'web' ? 'Web' : author === 'agent' ? 'Agent' : author;
}

/** A revision's files, sorted by path. Its `files` is JSON of path to { sha256, size }. */
export function fileList(json: string | null | undefined): [string, SyncedFile][] {
  try {
    const map = JSON.parse(json ?? '{}') as Record<string, SyncedFile>;
    return Object.entries(map).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  } catch {
    return [];
  }
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10 * 1024 ? 1 : 0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/** The largest file the page shows as text. */
export const MAX_SHOWN_BYTES = 2 * 1024 * 1024;

/** The bytes as text, or null for a file that is not UTF-8 text. */
export function asText(bytes: Uint8Array): string | null {
  if (bytes.subarray(0, 8192).includes(0)) return null;
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
}

/** How the source view highlights a file. */
export function sourceLanguage(path: string): 'latex' | 'typst' | 'plain' {
  const ext = /\.([a-z0-9]+)$/i.exec(path)?.[1]?.toLowerCase() ?? '';
  if (ext === 'typ') return 'typst';
  return ['tex', 'sty', 'cls', 'bib', 'ltx', 'dtx', 'bbx', 'cbx', 'bst'].includes(ext) ? 'latex' : 'plain';
}

/** A revision's `files` (JSON of path to { sha256, size }) as an object. */
export function fileMap(json: string | null | undefined): Record<string, SyncedFile> {
  try {
    const map = JSON.parse(json ?? '{}') as unknown;
    return map && typeof map === 'object' ? (map as Record<string, SyncedFile>) : {};
  } catch {
    return {};
  }
}

/** The folder of a path, with its slash ("sections/"), or "" at the top. */
const folderOf = (path: string) => path.slice(0, path.lastIndexOf('/') + 1);

/** `a/./b/../c` as `a/c`; null when it climbs out of the paper. */
function normalPath(path: string): string | null {
  const out: string[] = [];
  for (const part of path.split('/')) {
    if (!part || part === '.') continue;
    if (part === '..') {
      if (!out.length) return null;
      out.pop();
    } else out.push(part);
  }
  return out.join('/');
}

/** Where a comment starts on a line of LaTeX (an unescaped %), or its end. */
function commentAt(line: string): number {
  for (let i = 0; i < line.length; i++) {
    if (line[i] === '\\') i++;
    else if (line[i] === '%') return i;
  }
  return line.length;
}

const INPUT = /\\(?:input|include|subfile)\s*\{([^}]+)\}/g;

/**
 * A LaTeX file with the files it `\input`s (`\include`s, `\subfile`s) put in
 * place, as TeX reads it, for the preview. `text` gives a file's text, or
 * undefined for one that is not there. Paths are relative to the entry file's
 * folder, with or without `.tex`.
 */
export function inlineInputs(entry: string, text: (path: string) => string | undefined): string {
  const root = folderOf(entry);
  const visit = (path: string, depth: number, seen: Set<string>): string => {
    const source = text(path) ?? '';
    if (depth > 8) return source;
    return source
      .split('\n')
      .map((line) => {
        const cut = commentAt(line);
        const code = line.slice(0, cut).replace(INPUT, (whole, name: string) => {
          const base = normalPath(root + name.trim());
          const found = base === null ? undefined : [base, `${base}.tex`].find((p) => text(p) !== undefined && !seen.has(p));
          return found ? `\n${visit(found, depth + 1, new Set([...seen, found]))}\n` : whole;
        });
        return code + line.slice(cut);
      })
      .join('\n');
  };
  return visit(entry, 0, new Set([entry]));
}

/** The files a LaTeX paper may `\input`, whose text the preview needs. */
export const isTexSource = (path: string) => /\.(tex|ltx)$/i.test(path);

/** A BibTeX entry, enough to cite it and list it. */
export interface BibEntry {
  key: string;
  title: string;
  authors: string | null;
  year: string | null;
  venue: string | null;
  url: string | null;
}

/** BibTeX's `{...}`, `"..."` or bare value at `i`, and where it ends. */
function bibValue(s: string, i: number): { value: string; end: number } {
  if (s[i] === '{' || s[i] === '"') {
    const close = s[i] === '{' ? '}' : '"';
    let depth = 0;
    for (let j = i + 1; j < s.length; j++) {
      if (s[j] === '\\') j++;
      else if (s[j] === '{') depth++;
      else if (s[j] === '}' && depth > 0) depth--;
      else if (s[j] === close && depth === 0) return { value: s.slice(i + 1, j), end: j + 1 };
    }
    return { value: s.slice(i + 1), end: s.length };
  }
  const m = /^[^,}\s]*/.exec(s.slice(i))!;
  return { value: m[0], end: i + m[0].length };
}

/** A BibTeX value as plain text: braces, `\&` and `~` undone, white space folded. */
const bibText = (v: string) =>
  v
    .replace(/\\([&%$#_{}])/g, '$1')
    .replace(/[{}]/g, '')
    .replace(/~/g, ' ')
    .replace(/\s+/g, ' ')
    .trim();

/** The entries of a `.bib` file. `@string`, `@comment` and `@preamble` are skipped. */
export function parseBib(source: string): BibEntry[] {
  const entries: BibEntry[] = [];
  const head = /@([A-Za-z]+)\s*[{(]\s*([^,\s{}()]+)\s*,/g;
  for (let m = head.exec(source); m; m = head.exec(source)) {
    const type = m[1].toLowerCase();
    if (type === 'string' || type === 'comment' || type === 'preamble') continue;
    const fields: Record<string, string> = {};
    let i = head.lastIndex;
    for (;;) {
      const name = /^\s*([A-Za-z][\w-]*)\s*=\s*/.exec(source.slice(i));
      if (!name) break;
      i += name[0].length;
      const parts: string[] = [];
      for (;;) {
        const { value, end } = bibValue(source, i);
        parts.push(value);
        i = end;
        const join = /^\s*#\s*/.exec(source.slice(i));
        if (!join) break;
        i += join[0].length;
      }
      fields[name[1].toLowerCase()] = parts.join('');
      const comma = /^\s*,/.exec(source.slice(i));
      if (!comma) break;
      i += comma[0].length;
    }
    head.lastIndex = Math.max(head.lastIndex, i);
    const doi = fields.doi ? bibText(fields.doi) : '';
    entries.push({
      key: m[2],
      title: bibText(fields.title ?? '') || m[2],
      // Semicolons: lib/citations.ts reads "Vaswani, Ashish; Shazeer, Noam" as two people.
      authors: fields.author ? bibText(fields.author).split(/\s+and\s+/).join('; ') : null,
      year: /\d{4}/.exec(fields.year ?? fields.date ?? '')?.[0] ?? null,
      venue: bibText(fields.journal ?? fields.journaltitle ?? fields.booktitle ?? fields.publisher ?? fields.school ?? fields.institution ?? '') || null,
      url: fields.url ? bibText(fields.url) : doi ? `https://doi.org/${doi.replace(/^https?:\/\/(dx\.)?doi\.org\//, '')}` : null,
    });
  }
  return entries;
}

/** Author-year when the paper uses natbib or biblatex in that style, else numbered as LaTeX does. */
export function texCiteStyle(source: string): 'author-year' | 'numeric' {
  const style = /\\bibliographystyle\s*\{([^}]*)\}/.exec(source)?.[1] ?? '';
  if (/nat$|apa|harvard|chicago|agsm|apalike|authoryear/i.test(style)) return 'author-year';
  if (/\\usepackage\s*\[[^\]]*style\s*=\s*(authoryear|apa|chicago)/.test(source)) return 'author-year';
  return 'numeric';
}

/** Text files written live (as the API decides, apps/api/src/sync.ts `isLivePath`). */
const LIVE_EXTENSIONS = new Set(['tex', 'ltx', 'sty', 'cls', 'bib', 'bst', 'bbx', 'cbx', 'dtx', 'typ', 'md', 'markdown', 'txt']);
/** Larger text files are synced through revisions only. */
export const MAX_LIVE_BYTES = 512 * 1024;

/** Whether a file of this path and size is written live once the API made its row. */
export function isLiveFile(path: string, size: number): boolean {
  const dot = path.lastIndexOf('.');
  return size <= MAX_LIVE_BYTES && dot > path.lastIndexOf('/') + 1 && LIVE_EXTENSIONS.has(path.slice(dot + 1).toLowerCase());
}
