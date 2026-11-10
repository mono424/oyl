import { encode as encodePng } from 'fast-png';
import { extractImages, extractText, getDocumentProxy } from 'unpdf';
import type { Env } from './env';
import { Surreal, UserError, fromBase64 } from './surreal';

/** How many stored parts the material's PDF has (0: no PDF). */
export async function pdfPartCount(env: Env, materialId: string): Promise<number> {
  return Surreal.system(env).query<number>(
    'RETURN count(SELECT id FROM source_part WHERE material = type::record($m))',
    { m: materialId },
  );
}

/** The uploaded PDF's parts in order, read a few at a time. */
export async function* pdfParts(env: Env, materialId: string, count: number): AsyncGenerator<Uint8Array> {
  const db = Surreal.system(env);
  let next = 0;
  // A few parts per request: each comes back base64 in JSON.
  for (let from = 0; from < count; from += 3) {
    const rows = await db.query<{ idx: number; b64?: string | null }[]>(
      `SELECT idx, {
         LET $bytes = type::file('materials', '/sources/' + <string>record::id(id)).get() ?? data;
         RETURN IF $bytes != NONE { encoding::base64::encode($bytes) } ELSE { NONE };
       } AS b64 FROM source_part
       WHERE material = type::record($m) AND idx >= $from AND idx < $to ORDER BY idx`,
      { m: materialId, from, to: from + 3 },
    );
    for (const r of rows) {
      if (r.idx !== next || r.b64 == null) {
        throw new UserError('The uploaded PDF is missing one or more parts. Upload it again.');
      }
      next++;
      yield fromBase64(r.b64);
    }
  }
  if (next !== count) throw new UserError('The uploaded PDF is missing one or more parts. Upload it again.');
}

/** The uploaded PDF, reassembled from its stored parts. */
export async function loadPdf(env: Env, materialId: string): Promise<Uint8Array> {
  const count = await pdfPartCount(env, materialId);
  if (!count) throw new UserError('The uploaded PDF is missing. Upload it again.');
  const parts: Uint8Array[] = [];
  for await (const part of pdfParts(env, materialId, count)) parts.push(part);
  const out = new Uint8Array(parts.reduce((n, p) => n + p.byteLength, 0));
  let off = 0;
  for (const p of parts) {
    out.set(p, off);
    off += p.byteLength;
  }
  return out;
}

export async function pageCount(bytes: Uint8Array): Promise<number> {
  const pdf = await getDocumentProxy(bytes.slice());
  return pdf.numPages;
}

// ── Images ────────────────────────────────────────────────────────────────

const MIN_SIDE = 64;
const MAX_SIDE = 1600;
const MAX_PNG_BYTES = 2.5 * 1024 * 1024;

export interface ExtractedImage {
  page: number;
  width: number;
  height: number;
  png: Uint8Array;
  hash: string;
}

/**
 * Raster images embedded in pages [from, to], as PNG. Icons and bullets are
 * skipped, large images scaled down. Vector drawings are not images to PDF.js
 * and stay in the original PDF only.
 */
export async function extractPageImages(bytes: Uint8Array, from: number, to: number): Promise<ExtractedImage[]> {
  // Images above ~8 megapixels (full-page scans) are not decoded at all: at
  // 4 bytes a pixel they would not fit in a Worker's 128 MB.
  const pdf = await getDocumentProxy(bytes.slice(), { maxImageSize: 8_000_000 });
  const out: ExtractedImage[] = [];
  for (let page = from; page <= Math.min(to, pdf.numPages); page++) {
    let images;
    try {
      images = await extractImages(pdf, page);
    } catch (err) {
      console.warn(`[pdf] images on page ${page} skipped`, err);
      continue;
    }
    const seen = new Set<string>();
    for (const im of images) {
      if (im.width < MIN_SIDE || im.height < MIN_SIDE) continue;
      const scaled = downscale(im.data, im.width, im.height, im.channels, MAX_SIDE);
      const png = encodePng({ width: scaled.width, height: scaled.height, data: scaled.data, channels: im.channels, depth: 8 });
      if (png.byteLength > MAX_PNG_BYTES) continue;
      const hash = await sha256(scaled.data);
      if (seen.has(hash)) continue;
      seen.add(hash);
      out.push({ page, width: scaled.width, height: scaled.height, png, hash });
    }
  }
  return out;
}

/** Box-filter downscale so the longest side is at most `max`. */
function downscale(data: Uint8ClampedArray, w: number, h: number, ch: number, max: number) {
  const scale = Math.max(w, h) / max;
  if (scale <= 1) return { data: new Uint8Array(data.buffer, data.byteOffset, data.byteLength), width: w, height: h };
  const nw = Math.max(1, Math.round(w / scale));
  const nh = Math.max(1, Math.round(h / scale));
  const out = new Uint8Array(nw * nh * ch);
  for (let y = 0; y < nh; y++) {
    const y0 = Math.floor(y * scale);
    const y1 = Math.min(h, Math.floor((y + 1) * scale));
    for (let x = 0; x < nw; x++) {
      const x0 = Math.floor(x * scale);
      const x1 = Math.min(w, Math.floor((x + 1) * scale));
      for (let c = 0; c < ch; c++) {
        let sum = 0;
        let n = 0;
        for (let yy = y0; yy < y1; yy++) {
          for (let xx = x0; xx < x1; xx++) {
            sum += data[(yy * w + xx) * ch + c];
            n++;
          }
        }
        out[(y * nw + x) * ch + c] = n ? Math.round(sum / n) : 0;
      }
    }
  }
  return { data: out, width: nw, height: nh };
}

async function sha256(data: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', data);
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
}

// ── Markdown ──────────────────────────────────────────────────────────────

export interface PageImageRef {
  id: string;
  page: number;
  width: number | null;
  height: number | null;
}

/**
 * Pages [from, to] as Markdown, from the PDF's text layer: a `<!-- page N -->`
 * line, the page's text, then the images extracted from that page. The
 * original PDF stays in the viewer for its layout.
 */
export async function pagesToMarkdown(bytes: Uint8Array, from: number, to: number, images: PageImageRef[]): Promise<string> {
  const pdf = await getDocumentProxy(bytes.slice());
  const { text } = await extractText(pdf, { mergePages: false });
  const parts: string[] = [];
  for (let page = from; page <= to; page++) {
    const pics = images.filter((im) => im.page === page).map((im) => `![](asset:${im.id})`);
    parts.push([`<!-- page ${page} -->`, (text[page - 1] ?? '').trim(), ...pics].filter(Boolean).join('\n\n'));
  }
  return parts.join('\n\n');
}
