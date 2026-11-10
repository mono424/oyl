import { prepareImage } from './images';
import type { Db } from './query';

/**
 * The API's public routes (apps/api, `expose` in sp00ky.yml): PDF upload and
 * download, search, assets. Every request carries the user's SurrealDB token;
 * the API checks it against the database before doing anything. Background
 * work goes through the job outbox instead (jobs.ts).
 */
// Production serves API, MCP and extracted images under the app's own origin.
// An explicit override supports deployments with a separate API Worker.
export const API_URL = (import.meta.env.VITE_API_URL?.trim() || (import.meta.env.DEV ? 'http://localhost:8787' : window.location.origin)).replace(/\/+$/, '');
export const MCP_URL = `${API_URL}/mcp`;

export const assetUrl = (id: string) => `${API_URL}/material-assets/${encodeURIComponent(id)}`;
export const answerImageUrl = (id: string) => `${API_URL}/answer-images/${encodeURIComponent(id)}`;
/** A file of an uploaded site. Public: the id is unguessable and pages cannot send the token. */
export const siteUrl = (siteKey: string, path = '') => `${API_URL}/site-files/${encodeURIComponent(siteKey)}/${path.split('/').map(encodeURIComponent).join('/')}`;

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
  }
}

export async function api<T>(db: Db, path: string, init: RequestInit = {}): Promise<T> {
  const token = db.auth.token;
  if (!token) throw new ApiError('You are signed out. Sign in again to continue.', 401);
  const headers = new Headers(init.headers);
  headers.set('authorization', `Bearer ${token}`);
  if (init.body && typeof init.body === 'string' && !headers.has('content-type')) {
    headers.set('content-type', 'application/json');
  }
  const res = await fetch(`${API_URL}${path}`, { ...init, headers });
  const text = await res.text();
  const body = text ? safeJson(text) : null;
  if (!res.ok) {
    if (res.status === 405) throw new ApiError('The app could not reach its API service. Please reload and try again.', res.status);
    const msg = body && typeof body === 'object' && 'error' in body ? String(body.error) : `Request failed (${res.status})`;
    throw new ApiError(msg, res.status);
  }
  if (res.headers.get('content-type')?.includes('text/html')) {
    throw new ApiError('The app could not reach its API service. Please reload and try again.', 503);
  }
  return body as T;
}

function safeJson(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

/** Uploads a PDF for a material that already exists; processing starts at once. */
export const uploadPdf = (db: Db, materialId: string, file: File) =>
  api<{ ok: true }>(db, `/api/materials/${encodeURIComponent(materialId)}/pdf`, {
    method: 'POST',
    headers: { 'content-type': 'application/pdf', 'x-file-name': encodeURIComponent(file.name) },
    body: file,
  });

// The last few PDFs opened, so going back to a lecture does not download it again.
const pdfCache = new Map<string, Promise<ArrayBuffer>>();
const PDF_STALL_MS = 30_000;

/**
 * The original PDF of a material, for the viewer. A material's PDF changes
 * only when its source replaces it, which changes its size, so it is cached
 * by id and size. pdf.js takes ownership of the
 * buffer it is given: pass it a copy.
 */
export function fetchPdf(db: Db, materialId: string, size: number | null | undefined): Promise<ArrayBuffer> {
  const key = `${materialId}:${size ?? ''}`;
  const version = size ? `?v=${size}` : '';
  const hit = pdfCache.get(key);
  if (hit) {
    pdfCache.delete(key);
    pdfCache.set(key, hit);
    return hit;
  }
  const load = (async () => {
    const token = db.auth.token;
    if (!token) throw new ApiError('You are signed out. Sign in again to continue.', 401);
    // A request that stops making progress is given up, or the viewer (and
    // this cache) would wait on it forever. Each chunk restarts the clock, so
    // a large PDF on a slow connection still loads.
    const abort = new AbortController();
    let timer = setTimeout(() => abort.abort(), PDF_STALL_MS);
    const progress = () => {
      clearTimeout(timer);
      timer = setTimeout(() => abort.abort(), PDF_STALL_MS);
    };
    try {
      // The size in the URL: a source replacing the PDF changes it, so the
      // browser's cached copy of the old file is not reused.
      const res = await fetch(`${API_URL}/api/materials/${encodeURIComponent(materialId)}/pdf${version}`, {
        headers: { authorization: `Bearer ${token}` },
        signal: abort.signal,
      });
      if (!res.ok) {
        const body = await res.json().catch(() => null) as { error?: string } | null;
        throw new ApiError(body?.error ?? `The PDF could not be loaded (${res.status}).`, res.status);
      }
      if (!res.body) return await res.arrayBuffer();
      const reader = res.body.getReader();
      const chunks: Uint8Array[] = [];
      let size = 0;
      for (;;) {
        progress();
        const { done, value } = await reader.read();
        if (done) break;
        chunks.push(value);
        size += value.byteLength;
      }
      const out = new Uint8Array(size);
      let off = 0;
      for (const c of chunks) {
        out.set(c, off);
        off += c.byteLength;
      }
      return out.buffer;
    } catch (err) {
      if (err instanceof ApiError) throw err;
      throw new ApiError(
        abort.signal.aborted ? 'The PDF is taking too long to load. Try again.' : 'The PDF could not be loaded. Check your connection and try again.',
        0,
      );
    } finally {
      clearTimeout(timer);
    }
  })();
  pdfCache.set(key, load);
  load.catch(() => pdfCache.delete(key));
  while (pdfCache.size > 3) pdfCache.delete(pdfCache.keys().next().value!);
  return load;
}

export interface SearchHit {
  material: string;
  materialTitle: string;
  materialKind: 'text' | 'markdown' | 'pdf' | 'tex';
  position: number;
  rev: number;
  heading: string | null;
  text: string;
  /** PDF pages the passage comes from; null for typed and Markdown material. */
  page: number | null;
  pageEnd: number | null;
  score: number;
}

/** The app path of a material, opening a PDF at `page`. */
export const materialPath = (courseId: string, materialId: string, page?: number | null) =>
  `/courses/${courseId.slice(courseId.indexOf(':') + 1)}/materials/${materialId.slice(materialId.indexOf(':') + 1)}${page ? `?page=${page}` : ''}`;

/** "p. 12" or "pp. 12-13" for a cited passage, or null without pages. */
export const pageLabel = (page?: number | null, pageEnd?: number | null) =>
  page ? (pageEnd && pageEnd !== page ? `pp. ${page}-${pageEnd}` : `p. ${page}`) : null;

export const searchCourse = (db: Db, courseId: string, query: string) =>
  api<{ hits: SearchHit[] }>(db, `/api/courses/${encodeURIComponent(courseId)}/search`, {
    method: 'POST',
    body: JSON.stringify({ query }),
  });

export type ConceptLevel = 'unseen' | 'introduced' | 'struggling' | 'shaky' | 'solid' | 'mastered';

export interface GraphConcept {
  id: string;
  name: string;
  summary: string | null;
  level: ConceptLevel;
  score: number | null;
  checks: number;
  /** Background knowledge the lectures assume but do not teach. */
  prior: boolean;
  /** The broader concept this one is a part of. */
  parent: string | null;
  /** The lecture that teaches it, and where (1-based sections). */
  lecture: string | null;
  sections: number[];
  /** Within the student's place in the lecture. */
  reached: boolean;
  review_due: boolean;
  last_checked_at: string | null;
  misconceptions: string[];
  gaps: string[];
}

export interface CourseGraph {
  lectures: { id: string; title: string }[];
  concepts: GraphConcept[];
  /** `to` is a foundation of `from`. */
  requires: { from: string; to: string }[];
}

/** The course's concept map with what the student understood (concept, requires). */
export const fetchCourseGraph = (db: Db, courseId: string) =>
  api<CourseGraph>(db, `/api/courses/${encodeURIComponent(courseId)}/graph`);

// ── Sources (apps/api/src/fetch) ──────────────────────────────────────────

export type SourceKind = 'canvas' | 'web';

export interface SourceSettings {
  kind: SourceKind;
  /** web: the page. canvas: the Canvas address (bcourses.berkeley.edu by default). */
  url?: string;
  canvas_course?: string;
  include?: string;
  import_pages?: boolean;
  /** Also import LaTeX sources (.tex). */
  import_tex?: boolean;
  /** A bCourses access token, sent to the API only, which seals it. */
  token?: string;
}

export interface CanvasCourse {
  id: string;
  name: string;
  code: string | null;
  term: string | null;
}

export type PreviewStatus = 'new' | 'changed' | 'imported' | 'dismissed';

export interface SourcePreview {
  /** Everything the source lists, before the mapping filters it. */
  listed: number;
  items: { id: string; title: string; kind: 'pdf' | 'markdown' | 'tex'; link: string; status: PreviewStatus }[];
  total: number;
  issues: string[];
}

/** The courses a bCourses token can see, for the course picker. */
export const listCanvasCourses = (db: Db, input: { url?: string; token: string }) =>
  api<{ courses: CanvasCourse[] }>(db, '/api/fetchers/canvas/courses', { method: 'POST', body: JSON.stringify(input) });

/** What a source would import. `fetcher`: an existing source, using its saved token. */
export const previewSource = (db: Db, input: Partial<SourceSettings> & { fetcher?: string }) =>
  api<SourcePreview>(db, '/api/fetchers/preview', { method: 'POST', body: JSON.stringify(input) });

/** Adds a source to a course; its first check starts at once. */
export const createSource = (db: Db, input: SourceSettings & { course: string; name?: string; every_hours?: number }) =>
  api<{ id: string; name: string }>(db, '/api/fetchers/create', { method: 'POST', body: JSON.stringify(input) });

/** Changes a source. A new token switches a source its old token stopped back on. */
export const updateSource = (
  db: Db,
  fetcherId: string,
  patch: { name?: string; include?: string; import_pages?: boolean; import_tex?: boolean; every_hours?: number; token?: string; mapping?: string | null },
) => api<{ ok: true }>(db, `/api/fetchers/${encodeURIComponent(fetcherId)}`, { method: 'POST', body: JSON.stringify(patch) });

/** The hockeymask mapping a source runs: its own, or the template for its settings. */
export const sourceMapping = (db: Db, fetcherId: string) =>
  api<{ custom: boolean; mapping: unknown }>(db, `/api/fetchers/${encodeURIComponent(fetcherId)}/mapping`);

// ── Homework ──────────────────────────────────────────────────────────────

// The editor shows an upload until it ends, so one that hangs must end too.
const IMAGE_UPLOAD_MS = 120_000;

async function uploadImage(db: Db, path: string, file: Blob) {
  const image = await prepareImage(file);
  try {
    return await api<{ id: string }>(db, path, {
      method: 'POST',
      headers: { 'content-type': image.type || 'application/octet-stream' },
      body: image,
      signal: AbortSignal.timeout(IMAGE_UPLOAD_MS),
    });
  } catch (err) {
    if (err instanceof DOMException && err.name === 'TimeoutError') throw new ApiError('The image took too long to upload. Try again.', 504);
    throw err;
  }
}

/** Stores an image added to an answer; the answer refers to it as `answer-image:<key>`. */
export const uploadAnswerImage = (db: Db, answerId: string, file: Blob) => uploadImage(db, `/api/answers/${encodeURIComponent(answerId)}/images`, file);

/** Stores an image added to a document; the text refers to it as `answer-image:<key>`. */
export const uploadDocumentImage = (db: Db, documentId: string, file: Blob) => uploadImage(db, `/api/documents/${encodeURIComponent(documentId)}/images`, file);

export interface DocumentVersion {
  n: number;
  author: 'student' | 'assistant' | 'system';
  format: string;
  restored_from?: number | null;
  updated_at: string;
  content: string;
  /** The text of the version before it, to compare. */
  previous: string;
}

/** A version of a document, with its text as it was. */
export const documentVersion = (db: Db, documentId: string, n: number) =>
  api<DocumentVersion>(db, `/api/documents/${encodeURIComponent(documentId)}/versions/${n}`);

/** A document's text as the database holds it right now. */
export const documentText = (db: Db, documentId: string) =>
  api<{ content: string }>(db, `/api/documents/${encodeURIComponent(documentId)}/text`);

/** Goes back to a version, as a new version. Save the editor's text first: see whenSynced. */
export const restoreDocumentVersion = (db: Db, documentId: string, n: number) =>
  api<{ status: 'restored' | 'unchanged'; version: number }>(db, `/api/documents/${encodeURIComponent(documentId)}/versions/${n}/restore`, { method: 'POST' });

// ── Sites (apps/api/src/sites.ts) ─────────────────────────────────────────

/** Stores one file of a site that is still uploading. */
export const uploadSiteFile = (db: Db, siteId: string, path: string, data: Blob) =>
  api<{ ok: true }>(db, `/api/sites/${encodeURIComponent(siteId)}/files?path=${encodeURIComponent(path)}`, {
    method: 'POST',
    headers: { 'content-type': 'application/octet-stream' },
    body: data,
  });

/** Every file is up: the API lists the site's pages and opens it. */
export const finishSiteUpload = (db: Db, siteId: string) =>
  api<{ entry: string }>(db, `/api/sites/${encodeURIComponent(siteId)}/finish`, { method: 'POST' });
