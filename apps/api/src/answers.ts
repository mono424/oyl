import type { Env } from './env';
import { ensureServerTable, readLimited } from './service';
import { BadRequest, NotFound, Surreal, recordId, toBase64 } from './surreal';

/**
 * Images added to a homework answer or a document: a sketch, a photo of a
 * drawing, a figure. Kept in the `materials` bucket and served by an
 * unguessable id, like the images of a lecture (an <img> cannot send the
 * session token).
 */

export const MAX_ANSWER_IMAGE_BYTES = 10 * 1024 * 1024;

// In a text these are `answer-image:<id>`, in parentheses in Markdown and in
// braces in LaTeX. AI clients see the image's address and may write either back.
export const imagesOutside = (text: string, origin: string) =>
  text.replace(/([({])answer-image:([A-Za-z0-9_-]+)(?=[)}])/g, (_m, open: string, id: string) => `${open}${origin}/answer-images/${id}`);
export const imagesInside = (text: string, origin: string) =>
  text.replaceAll(`(${origin}/answer-images/`, '(answer-image:').replaceAll(`{${origin}/answer-images/`, '{answer-image:');

const TYPES: { mime: string; magic: (b: Uint8Array) => boolean }[] = [
  { mime: 'image/png', magic: (b) => b[0] === 0x89 && b[1] === 0x50 && b[2] === 0x4e && b[3] === 0x47 },
  { mime: 'image/jpeg', magic: (b) => b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff },
  { mime: 'image/gif', magic: (b) => b[0] === 0x47 && b[1] === 0x49 && b[2] === 0x46 },
  { mime: 'image/webp', magic: (b) => b[0] === 0x52 && b[1] === 0x49 && b[2] === 0x46 && b[3] === 0x46 && b[8] === 0x57 && b[9] === 0x45 },
];

/** Stores an image for one of the user's answers or documents. Answers `answer_image:<id>`. */
export async function storeAnswerImage(
  env: Env,
  userId: string,
  table: 'answer' | 'document',
  rawId: string,
  body: ReadableStream<Uint8Array> | null,
): Promise<string> {
  const answer = recordId(table, rawId);
  await ensureServerTable(env, 'answer_image');
  const db = Surreal.system(env);
  const owned = await db.query<string | null>(
    'RETURN (SELECT VALUE id FROM type::record($answer) WHERE owner = type::record($owner))[0]',
    { answer, owner: userId },
  );
  if (!owned) throw new NotFound(table === 'answer' ? 'Answer not found.' : 'Document not found.');
  const bytes = await readLimited(body, MAX_ANSWER_IMAGE_BYTES);
  if (!bytes.byteLength) throw new BadRequest('The image is empty.');
  const type = TYPES.find((t) => t.magic(bytes));
  if (!type) throw new BadRequest('Add a PNG, JPEG, GIF or WebP image.');
  const id = `answer_image:i${crypto.randomUUID().replaceAll('-', '')}`;
  // The file first, so a row never points at a missing file.
  await db.query(
    `type::file('materials', '/answers/' + <string>record::id(type::record($id))).put(encoding::base64::decode($b64));
     CREATE type::record($id) CONTENT { owner: type::record($owner), answer: type::record($answer), mime: $mime, size: $size };`,
    { id, owner: userId, answer, mime: type.mime, size: bytes.byteLength, b64: toBase64(bytes) },
  );
  return id;
}

/** An answer image by id, for the public route. */
export async function loadAnswerImage(env: Env, rawId: string): Promise<{ b64: string; mime: string } | null> {
  const id = recordId('answer_image', rawId);
  return Surreal.system(env).query<{ b64: string; mime: string } | null>(
    `LET $row = SELECT mime FROM ONLY type::record($id);
     LET $bytes = IF $row { type::file('materials', '/answers/' + <string>record::id(type::record($id))).get() } ELSE { NONE };
     RETURN IF $row AND $bytes { { mime: $row.mime, b64: encoding::base64::encode($bytes) } } ELSE { NONE };`,
    { id },
  );
}
