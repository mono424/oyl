/**
 * Images added to an answer or a document, made ready for the API, which
 * keeps PNG, JPEG, GIF and WebP up to 10 MB (apps/api/src/answers.ts).
 * Anything else the browser can open (a HEIC photo in Safari, a BMP), and a
 * photo too big, goes as a JPEG of at most 3200 pixels a side.
 */

const KEPT = ['image/png', 'image/jpeg', 'image/gif', 'image/webp'];
const MAX_BYTES = 10 * 1024 * 1024;
const MAX_SIDE = 3200;

/** Whether a file is an image (a HEIC photo can come without a type). */
export const isImage = (file: File) => file.type.startsWith('image/') || /\.(heic|heif)$/i.test(file.name);

export async function prepareImage(file: Blob): Promise<Blob> {
  if (KEPT.includes(file.type) && file.size <= MAX_BYTES) return file;
  return redrawn(file, 'image/jpeg');
}

/** An added image as a file for pdfLaTeX, which opens PNG and JPEG: anything else becomes a PNG. */
export async function texImageFile(url: string): Promise<{ bytes: Uint8Array; extension: 'png' | 'jpg' }> {
  const res = await fetch(url);
  if (!res.ok) throw new Error('The image could not be read.');
  const image = await res.blob();
  const extension = image.type === 'image/jpeg' ? 'jpg' : 'png';
  const file = extension === 'jpg' || image.type === 'image/png' ? image : await redrawn(image, 'image/png');
  return { bytes: new Uint8Array(await file.arrayBuffer()), extension };
}

async function redrawn(file: Blob, type: 'image/jpeg' | 'image/png'): Promise<Blob> {
  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(file);
  } catch {
    throw new Error('This image could not be opened. Add a PNG, JPEG, GIF or WebP image.');
  }
  const scale = Math.min(1, MAX_SIDE / Math.max(bitmap.width, bitmap.height));
  const canvas = document.createElement('canvas');
  canvas.width = Math.max(1, Math.round(bitmap.width * scale));
  canvas.height = Math.max(1, Math.round(bitmap.height * scale));
  const ctx = canvas.getContext('2d');
  if (!ctx) throw new Error('The image could not be added.');
  // What was clear prints as the page does, white (a JPEG has no transparency).
  ctx.fillStyle = '#fff';
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
  bitmap.close();
  const out = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, type, 0.9));
  if (!out) throw new Error('The image could not be added.');
  return out;
}
