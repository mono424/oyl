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
