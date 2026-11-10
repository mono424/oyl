import { ApiError, commitPaperRevision, type LiveFile, putSyncBlob, type SyncFiles, syncBlob } from './api';
import type { Db } from './query';

type CrdtField = Awaited<ReturnType<ReturnType<Db['getSp00ky']>['openCrdtField']>>;
/** Loro's document, as sp00ky hands it out (the web app does not depend on loro-crdt itself). */
export type LoroDoc = ReturnType<CrdtField['getDoc']>;

/**
 * The text files of a synced paper, written live on the web and in Oyl on
 * the Mac at once (docs/sync-protocol.md, Live text). Each is a `paper_file`
 * row whose `content` is a sp00ky CRDT field: a Loro document that sp00ky
 * keeps in step everywhere, sending this page's edits as rows of
 * `paper_file__crdt_log` and merging everyone else's as they arrive.
 *
 * A document starts from its seed, the text the file had when the API made
 * its row. Every client adds the seed as the same first edit: made by a peer
 * worked out from the file and the text, so all of them make the very same
 * operation and Loro keeps it once. The Mac does the same in Rust
 * (crates/oyl-sync/src/live_text.rs); keep the two in step.
 */

export const LIVE_TABLE = 'paper_file';
export const LIVE_FIELD = 'content';

const hex = (bytes: ArrayBuffer) => [...new Uint8Array(bytes)].map((b) => b.toString(16).padStart(2, '0')).join('');
const sha256 = async (text: string) => crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));

/** The seed's bytes as text, exactly as every client reads them (a byte order mark is kept). */
export function seedText(bytes: Uint8Array): string | null {
  try {
    return new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes);
  } catch {
    return null;
  }
}

/**
 * The Loro peer the seed edit of `fileId` is made by: the first 8 bytes of
 * SHA-256("oyl-seed\n<file id>\n<SHA-256 hex of the text>"), big-endian, kept
 * below 2^53 (exact in JavaScript), never 0. A different text gives another
 * peer, so two clients that somehow read different seeds add both texts
 * rather than two different operations under one id.
 */
export async function seedPeer(fileId: string, text: string): Promise<`${number}`> {
  const digest = new Uint8Array(await sha256(`oyl-seed\n${fileId}\n${hex(await sha256(text))}`));
  let peer = 0n;
  for (let i = 0; i < 8; i++) peer = (peer << 8n) | BigInt(digest[i]);
  peer &= (1n << 53n) - 1n;
  return String(peer === 0n ? 1n : peer) as `${number}`;
}

export interface OpenLiveFile {
  field: CrdtField;
  doc: LoroDoc;
  /** Closes the file; resolves once what was typed is on its way. */
  close: () => Promise<void>;
}

/** Opens a live file's document, once it is known, with its seed in it. Close it when done. */
export async function openLiveFile(db: Db, file: LiveFile): Promise<OpenLiveFile> {
  const sp00ky = db.getSp00ky();
  const field = await sp00ky.openCrdtField(LIVE_TABLE, file.id, LIVE_FIELD);
  let closed = false;
  const close = () => {
    if (closed) return Promise.resolve();
    closed = true;
    return sp00ky.closeCrdtField(LIVE_TABLE, file.id, LIVE_FIELD);
  };
  try {
    await new Promise<void>((resolve) => field.onLoaded(resolve));
    const doc = field.getDoc();
    const text = seedText(await syncBlob(db, file.seed));
    if (text === null) throw new ApiError(`${file.path} is not text.`, 422);
    const peer = await seedPeer(file.id, text);
    if (text && !doc.oplogVersion().get(peer)) {
      // Sent as a draft: sp00ky claims the seed's peer for this account and sends its edit.
      const draft = await sp00ky.createCrdtDraft(LIVE_TABLE, LIVE_FIELD);
      const seed = draft.getDoc();
      seed.setPeerId(peer);
      seed.getText(LIVE_FIELD).insert(0, text);
      seed.commit();
      await sp00ky.openCrdtField(LIVE_TABLE, file.id, LIVE_FIELD, draft);
      await sp00ky.closeCrdtField(LIVE_TABLE, file.id, LIVE_FIELD);
    }
    return { field, doc, close };
  } catch (err) {
    await close();
    throw err;
  }
}

/** A live file's text as this page holds it. */
export const liveText = (doc: LoroDoc) => doc.getText(LIVE_FIELD).toString();

// ── Revisions of what was written live ───────────────────────────────────

const encoder = new TextEncoder();

/**
 * `files` with these texts in place of their paths' blobs, the blobs stored;
 * null when every text is what `files` holds already.
 */
export async function withTexts(db: Db, files: SyncFiles, texts: ReadonlyMap<string, string>): Promise<SyncFiles | null> {
  const out: SyncFiles = { ...files };
  let changed = false;
  for (const [path, text] of texts) {
    const bytes = encoder.encode(text);
    const sha256 = hex(await crypto.subtle.digest('SHA-256', bytes));
    if (files[path]?.sha256 === sha256) continue;
    await putSyncBlob(db, sha256, bytes);
    out[path] = { sha256, size: bytes.byteLength };
    changed = true;
  }
  return changed ? out : null;
}

/**
 * Makes a revision (by the web) on the paper's head with the files `make`
 * works out from the head's: a checkpoint of what was written live here, or an
 * older revision brought back. When the head moved meanwhile it goes again on
 * the new one. Answers the revision's number, or null when nothing changed.
 */
export async function commitOnHead(
  db: Db,
  paperId: string,
  head: { id: string; files: SyncFiles } | null,
  make: (files: SyncFiles) => Promise<SyncFiles | null>,
  message?: string,
): Promise<number | null> {
  let base = head;
  for (let attempt = 0; attempt < 5; attempt++) {
    if (!base) return null;
    const files = await make(base.files);
    if (!files) return null;
    const answer = await commitPaperRevision(db, paperId, { parent: base.id, files, ...(message ? { message } : {}) });
    if (answer.made) return answer.n;
    base = answer.head ? { id: answer.head.revision, files: answer.head.files } : null;
  }
  throw new ApiError('The paper keeps changing elsewhere. Try again in a moment.', 503);
}
