import { ApiError, finishSiteUpload, uploadSiteFile } from './api';
import { newId, whenSynced } from './ids';
import type { Db } from './query';

/** A static website read from a folder, a .zip or loose files, ready to upload. */
export interface SiteBundle {
  title: string;
  files: { path: string; data: Blob }[];
}

// The API's limits (apps/api/src/sites.ts), checked here to fail before uploading.
const MAX_FILE_BYTES = 30 * 1024 * 1024;
const MAX_SITE_BYTES = 100 * 1024 * 1024;
const MAX_FILES = 2000;

const isHtml = (path: string) => /\.html?$/i.test(path);
/** Files a site never needs: system clutter, and dotfiles such as .git. */
const skipped = (path: string) => path.split('/').some((s) => s.startsWith('.') || s === '__MACOSX' || s === 'Thumbs.db');

/** The site's own title, from the page it will open on. */
async function pageTitle(file: { data: Blob } | undefined): Promise<string> {
  if (!file) return '';
  const html = await file.data.slice(0, 64 * 1024).text();
  return new DOMParser().parseFromString(html, 'text/html').title.trim();
}

async function toBundle(name: string, entries: { path: string; data: Blob }[]): Promise<SiteBundle> {
  let files = entries.filter((f) => f.path && !f.path.endsWith('/') && !skipped(f.path));
  // A folder or an archive of one: its name is not part of the site's addresses.
  for (;;) {
    const top = files[0]?.path.split('/')[0];
    if (!top || !files.every((f) => f.path.startsWith(`${top}/`))) break;
    name = top;
    files = files.map((f) => ({ ...f, path: f.path.slice(top.length + 1) }));
  }
  const pages = files.filter((f) => isHtml(f.path));
  if (!pages.length) throw new Error('No HTML page found. A site needs at least an index.html.');
  if (files.length > MAX_FILES) throw new Error(`That is ${files.length} files. Sites of up to ${MAX_FILES} files are supported.`);
  const big = files.find((f) => f.data.size > MAX_FILE_BYTES);
  if (big) throw new Error(`${big.path} is too large. Files up to 30 MB are supported.`);
  if (files.reduce((n, f) => n + f.data.size, 0) > MAX_SITE_BYTES) throw new Error('That site is too large. Sites up to 100 MB are supported.');
  const entry = pages.find((f) => f.path.toLowerCase() === 'index.html') ?? pages[0];
  return { title: (await pageTitle(entry)) || name, files };
}

/** Files picked or dropped: one .zip is unpacked, anything else is the site's files as they are. */
export async function siteFromFiles(picked: File[]): Promise<SiteBundle> {
  if (picked.length === 1 && /\.zip$/i.test(picked[0].name)) {
    const { unzip } = await import('fflate');
    const data = new Uint8Array(await picked[0].arrayBuffer());
    const unpacked = await new Promise<Record<string, Uint8Array>>((resolve, reject) =>
      unzip(data, (err, out) => (err ? reject(new Error('That .zip could not be read.')) : resolve(out))),
    );
    return toBundle(
      picked[0].name.replace(/\.zip$/i, ''),
      Object.entries(unpacked).map(([path, bytes]) => ({ path, data: new Blob([bytes as Uint8Array<ArrayBuffer>]) })),
    );
  }
  return toBundle(picked[0]?.name.replace(/\.html?$/i, '') ?? 'Site', picked.map((f) => ({ path: f.webkitRelativePath || f.name, data: f })));
}

/** Every file under a dropped folder, with its path from the dropped folder. */
async function walk(entry: FileSystemEntry, into: { path: string; data: Blob }[]): Promise<void> {
  if (entry.isFile) {
    const file = await new Promise<File>((resolve, reject) => (entry as FileSystemFileEntry).file(resolve, reject));
    into.push({ path: entry.fullPath.replace(/^\/+/, ''), data: file });
    return;
  }
  const reader = (entry as FileSystemDirectoryEntry).createReader();
  // readEntries answers in batches, until an empty one.
  for (;;) {
    const batch = await new Promise<FileSystemEntry[]>((resolve, reject) => reader.readEntries(resolve, reject));
    if (!batch.length) return;
    for (const child of batch) await walk(child, into);
  }
}

/**
 * A dropped folder, .zip or files. Call it inside the drop handler: the
 * browser empties `dataTransfer` once the handler returns.
 */
export function siteFromDrop(transfer: DataTransfer): Promise<SiteBundle> {
  const entries = Array.from(transfer.items).map((item) => item.webkitGetAsEntry()).filter((e) => !!e);
  const files = Array.from(transfer.files);
  return (async () => {
    if (!entries.some((e) => e.isDirectory)) return siteFromFiles(files);
    const found: { path: string; data: Blob }[] = [];
    for (const entry of entries) await walk(entry, found);
    return toBundle('Site', found);
  })();
}

async function sendFile(db: Db, siteId: string, file: SiteBundle['files'][number]) {
  for (let tries = 0; ; tries++) {
    try {
      return await uploadSiteFile(db, siteId, file.path, file.data);
    } catch (err) {
      // Only a dropped connection or a server hiccup is worth repeating.
      const refused = err instanceof ApiError && err.status >= 400 && err.status < 500;
      if (refused || tries >= 2) throw err;
    }
  }
}

/** Creates the course's site and uploads its files, a few at a time. Leaves no row when it fails. */
export async function uploadSite(db: Db, input: { owner: string; course: string; bundle: SiteBundle; onProgress: (done: number, total: number) => void }): Promise<string> {
  const { files, title } = input.bundle;
  const id = newId('site');
  await db.create(id, { owner: input.owner, course: input.course, title: title.slice(0, 200), status: 'uploading' });
  await whenSynced(db);
  try {
    const queue = [...files];
    let done = 0;
    input.onProgress(0, files.length);
    await Promise.all(
      Array.from({ length: 4 }, async () => {
        while (queue.length) {
          await sendFile(db, id, queue.shift()!);
          input.onProgress(++done, files.length);
        }
      }),
    );
    await finishSiteUpload(db, id);
    return id;
  } catch (err) {
    await db.delete('site', id).catch(() => undefined);
    throw err;
  }
}
