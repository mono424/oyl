import type { PDFDocumentProxy } from 'pdfjs-dist';

/**
 * Where each part of an exercise sheet is on its PDF pages, from the pages'
 * text layer (pdf.js). Pure: the homework page saves what it finds.
 */

/** A piece of a sheet page: fractions of the page's height. */
export interface Segment {
  page: number;
  top: number;
  bottom: number;
}

export function parseRegion(region: string | null | undefined): Segment[] {
  if (!region) return [];
  try {
    const value = JSON.parse(region) as Segment[];
    return Array.isArray(value) ? value.filter((s) => s && s.page > 0 && s.bottom > s.top) : [];
  } catch {
    return [];
  }
}

interface Line {
  top: number;
  bottom: number;
  text: string;
}

const compact = (s: string) => s.toLowerCase().replace(/\s+/g, '');

/** A page's text as lines, top to bottom, with where each sits (fractions of the page). */
async function pageLines(pdf: PDFDocumentProxy, n: number): Promise<Line[]> {
  const page = await pdf.getPage(n);
  const viewport = page.getViewport({ scale: 1 });
  const content = await page.getTextContent();
  const items: { x: number; top: number; bottom: number; text: string }[] = [];
  for (const item of content.items) {
    if (!('str' in item) || !item.str.trim()) continue;
    const [, , , d, e, f] = item.transform as number[];
    const [x, y] = viewport.convertToViewportPoint(e, f);
    const h = Math.abs(d) || item.height || 10;
    items.push({ x, top: (y - h) / viewport.height, bottom: y / viewport.height, text: item.str });
  }
  items.sort((a, b) => a.top - b.top || a.x - b.x);
  const lines: Line[] = [];
  for (const it of items) {
    const line = lines.at(-1);
    // Items whose tops are within a third of a line height belong to one line.
    if (line && Math.abs(line.top - it.top) < (it.bottom - it.top) * 0.35) {
      line.text += ` ${it.text}`;
      line.bottom = Math.max(line.bottom, it.bottom);
    } else {
      lines.push({ top: it.top, bottom: it.bottom, text: it.text });
    }
  }
  return lines;
}

/**
 * Where each part's excerpt is on the sheet: from the line its anchor starts
 * ("Question 1", "Part (b)", "(ii)") down to the next part's, continuing onto
 * the next page when the part does. Anchors are searched in sheet order, so
 * an "(ii)" is the one after the previous part. A part whose anchor is not
 * found gets no excerpt (`[]`): the app shows its text instead.
 */
export async function locateParts(
  pdf: PDFDocumentProxy,
  parts: { anchor?: string | null; anchor_page?: number | null; page?: number | null }[],
): Promise<Segment[][]> {
  const cache = new Map<number, Line[]>();
  const lines = async (n: number) => {
    if (n < 1 || n > pdf.numPages) return [];
    if (!cache.has(n)) cache.set(n, await pageLines(pdf, n));
    return cache.get(n)!;
  };

  const starts: ({ page: number; top: number } | null)[] = [];
  let cursor = { page: 1, top: -1 };
  for (const part of parts) {
    const anchor = compact(part.anchor ?? '');
    const expected = part.anchor_page ?? part.page ?? cursor.page;
    let found: { page: number; top: number } | null = null;
    if (anchor) {
      search: for (let p = Math.max(1, Math.min(cursor.page, expected)); p <= Math.min(pdf.numPages, expected + 1); p++) {
        for (const line of await lines(p)) {
          if (p === cursor.page && line.top <= cursor.top) continue;
          if (compact(line.text).startsWith(anchor)) {
            found = { page: p, top: line.top };
            break search;
          }
        }
      }
    }
    starts.push(found);
    if (found) cursor = found;
  }

  // A part that ends its page runs to the page's foot: figures below the
  // text have no text layer, and would be cut off at the last line.
  const textBottom = async (n: number) => Math.min(1, Math.max(0.94, Math.max(0, ...(await lines(n)).map((l) => l.bottom)) + 0.012));
  const textTop = async (n: number) => Math.max(0, Math.min(1, ...(await lines(n)).map((l) => l.top)) - 0.012);
  const pad = (top: number) => Math.max(0, top - 0.012);

  const regions: Segment[][] = [];
  for (let i = 0; i < parts.length; i++) {
    const start = starts[i];
    if (!start) {
      regions.push([]);
      continue;
    }
    const end = starts.slice(i + 1).find((s) => s !== null) ?? null;
    if (end && end.page === start.page) {
      regions.push([{ page: start.page, top: pad(start.top), bottom: pad(end.top) }]);
      continue;
    }
    const segments: Segment[] = [{ page: start.page, top: pad(start.top), bottom: await textBottom(start.page) }];
    // A part that runs onto the next page, up to where the next part starts there.
    if (end && end.page === start.page + 1) {
      const top = await textTop(end.page);
      if (pad(end.top) - top > 0.03) segments.push({ page: end.page, top, bottom: pad(end.top) });
    }
    regions.push(segments.filter((s) => s.bottom - s.top > 0.01));
  }
  return regions;
}
