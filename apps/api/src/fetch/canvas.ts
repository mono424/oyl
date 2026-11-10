import { type Connector, ConnectorError } from '@hockeymask/core';
import { BadRequest, UserError } from '../surreal';
import { htmlToMarkdown } from './html';
import { rowsConnector, type SourceRow } from './mapping';
import { publicUrl, readText, safeFetch } from './net';

/**
 * Canvas LMS (bCourses is Berkeley's Canvas) through its REST API, with the
 * student's own access token (Account > Settings > New access token). Lists
 * one course's files, and its pages when asked. Many courses hide the Files
 * tab from students and share files through Modules only, so a refused file
 * list falls back to the files and pages linked from the course's modules.
 */

export const BCOURSES = 'https://bcourses.berkeley.edu';

const MAX_PAGES = 30; // of 100 items each
const MAX_MODULE_FILES = 300;
const MAX_JSON = 8 * 1024 * 1024;

/** Canvas refused the token itself: expired, revoked or mistyped. */
export class CanvasAuthError extends UserError {
  constructor(host: string) {
    super(`${host} did not accept the access token. Create a new one in your Canvas account settings and paste it here.`, 403);
  }
}

/** Canvas refused this one request (a hidden tab, a locked file). */
class CanvasForbidden extends Error {}

interface CanvasFile {
  id: number; display_name: string; filename: string; 'content-type'?: string; size?: number;
  url?: string; updated_at?: string; modified_at?: string; locked_for_user?: boolean; hidden_for_user?: boolean;
}
interface CanvasPage { page_id: number; url: string; title: string; updated_at?: string; html_url?: string; published?: boolean; locked_for_user?: boolean }
interface CanvasModuleItem { id: number; title: string; type: string; content_id?: number; url?: string; html_url?: string; page_url?: string }
interface CanvasModule { id: number; name: string; items?: CanvasModuleItem[]; items_url?: string }

export interface CanvasCourse { id: string; name: string; code: string | null; term: string | null }

const extOf = (name: string) => /\.([a-z0-9]{1,8})$/i.exec(name)?.[1]?.toLowerCase() ?? null;

/** `https://bcourses.berkeley.edu`, from whatever the student typed (a course URL works too). */
export function canvasBase(raw: string | null | undefined): string {
  const typed = (raw ?? '').trim() || BCOURSES;
  return publicUrl(/^https?:\/\//i.test(typed) ? typed : `https://${typed}`, 'The Canvas address').origin;
}

/** A Canvas course id from an id or a course URL (`.../courses/1234567`). */
export function canvasCourseId(raw: unknown): string {
  const s = String(raw ?? '').trim();
  const id = /\/courses\/(\d+)/.exec(s)?.[1] ?? (/^\d+$/.test(s) ? s : null);
  if (!id) throw new BadRequest('Pick a bCourses course, or paste its URL (…/courses/1234567).');
  return id;
}

export class CanvasClient {
  readonly base: string;
  private readonly host: string;

  constructor(base: string, private readonly token: string) {
    this.base = canvasBase(base);
    this.host = new URL(this.base).host;
    if (!token.trim()) throw new BadRequest('Paste a Canvas access token.');
  }

  private async request(pathOrUrl: string, accept = 'application/json'): Promise<{ res: Response; url: URL }> {
    const url = new URL(pathOrUrl, this.base);
    // The token never leaves its own Canvas, even if a listing says otherwise.
    if (url.origin !== this.base) throw new ConnectorError(`Canvas pointed outside ${this.host}.`);
    const out = await safeFetch(url, { accept, authorization: `Bearer ${this.token.trim()}` });
    if (out.res.status === 401 && out.res.headers.get('www-authenticate')) throw new CanvasAuthError(this.host);
    if (out.res.status === 401 || out.res.status === 403 || out.res.status === 404) {
      await out.res.body?.cancel();
      throw new CanvasForbidden(String(out.res.status));
    }
    if (!out.res.ok) {
      await out.res.body?.cancel();
      throw new BadRequest(`${this.host} answered ${out.res.status}. Try again later.`);
    }
    return out;
  }

  private async json<T>(path: string): Promise<{ data: T; next: string | null }> {
    const { res } = await this.request(path);
    const next = /<([^>]+)>;\s*rel="next"/.exec(res.headers.get('link') ?? '')?.[1] ?? null;
    return { data: JSON.parse(await readText(res, MAX_JSON)) as T, next };
  }

  private async all<T>(path: string): Promise<T[]> {
    const out: T[] = [];
    let next: string | null = path;
    for (let page = 0; next && page < MAX_PAGES; page++) {
      const r: { data: T[]; next: string | null } = await this.json<T[]>(next);
      if (!Array.isArray(r.data)) throw new BadRequest(`${this.host} answered with something other than a list.`);
      out.push(...r.data);
      next = r.next;
    }
    return out;
  }

  /** The courses the student is enrolled in, newest first. */
  async courses(): Promise<CanvasCourse[]> {
    const rows = await this.all<{ id: number; name?: string; course_code?: string; term?: { name?: string }; access_restricted_by_date?: boolean }>(
      '/api/v1/courses?enrollment_state=active&include[]=term&per_page=100',
    ).catch((err) => {
      if (err instanceof CanvasForbidden) throw new CanvasAuthError(this.host);
      throw err;
    });
    return rows
      .filter((c) => c.name && !c.access_restricted_by_date)
      .map((c) => ({ id: String(c.id), name: c.name!, code: c.course_code ?? null, term: c.term?.name ?? null }))
      .reverse();
  }

  async course(id: string): Promise<CanvasCourse> {
    try {
      const { data: c } = await this.json<{ id: number; name: string; course_code?: string; term?: { name?: string } }>(
        `/api/v1/courses/${canvasCourseId(id)}?include[]=term`,
      );
      return { id: String(c.id), name: c.name, code: c.course_code ?? null, term: c.term?.name ?? null };
    } catch (err) {
      if (err instanceof CanvasForbidden) throw new BadRequest(`You have no access to course ${id} on ${this.host}.`);
      throw err;
    }
  }

  private fileRow(f: CanvasFile, module: string | null): SourceRow | null {
    if (!f.url || f.locked_for_user || f.hidden_for_user) return null;
    const name = f.display_name || f.filename;
    return {
      id: `file:${f.id}`,
      name,
      kind: 'file',
      content_type: f['content-type'] ?? null,
      ext: extOf(f.filename || name),
      url: f.url,
      link: `${this.base}/files/${f.id}`,
      updated_at: f.modified_at ?? f.updated_at ?? null,
      module,
      size: f.size ?? null,
    };
  }

  private pageRow(courseId: string, p: { url: string; title: string; updated_at?: string; html_url?: string; page_id?: number }, module: string | null): SourceRow {
    return {
      id: `page:${p.page_id ?? p.url}`,
      name: p.title,
      kind: 'page',
      content_type: 'text/html',
      ext: null,
      url: `${this.base}/api/v1/courses/${courseId}/pages/${encodeURIComponent(p.url)}`,
      link: p.html_url ?? `${this.base}/courses/${courseId}/pages/${encodeURIComponent(p.url)}`,
      updated_at: p.updated_at ?? null,
      module,
      size: null,
    };
  }

  /** Everything the course shares with the student: files, and pages when `pages`. */
  async items(courseIdRaw: string, opts: { pages: boolean }): Promise<SourceRow[]> {
    const courseId = canvasCourseId(courseIdRaw);
    const rows = new Map<string, SourceRow>();
    const add = (r: SourceRow | null) => {
      if (r && !rows.has(r.id)) rows.set(r.id, r);
    };

    let filesListed = false;
    try {
      for (const f of await this.all<CanvasFile>(`/api/v1/courses/${courseId}/files?per_page=100&sort=updated_at&order=desc`)) add(this.fileRow(f, null));
      filesListed = true;
    } catch (err) {
      if (!(err instanceof CanvasForbidden)) throw err;
    }
    let pagesListed = !opts.pages;
    if (opts.pages) {
      try {
        const pages = await this.all<CanvasPage>(`/api/v1/courses/${courseId}/pages?per_page=100&published=true&sort=updated_at&order=desc`);
        for (const p of pages) if (p.published !== false && !p.locked_for_user) add(this.pageRow(courseId, p, null));
        pagesListed = true;
      } catch (err) {
        if (!(err instanceof CanvasForbidden)) throw err;
      }
    }

    // Modules, for what the tabs did not list, and for the module names.
    let modules: CanvasModule[] = [];
    try {
      modules = await this.all<CanvasModule>(`/api/v1/courses/${courseId}/modules?include[]=items&per_page=50`);
    } catch (err) {
      if (!(err instanceof CanvasForbidden)) throw err;
      if (!filesListed && !pagesListed) throw new BadRequest(`${this.host} shows you neither the files nor the modules of this course.`);
    }
    let fetched = 0;
    for (const m of modules) {
      const items = m.items ?? (m.items_url ? await this.all<CanvasModuleItem>(m.items_url).catch(() => []) : []);
      for (const it of items) {
        if (it.type === 'File' && it.content_id) {
          const known = rows.get(`file:${it.content_id}`);
          if (known) {
            known.module ??= m.name;
            continue;
          }
          if (filesListed || fetched >= MAX_MODULE_FILES || !it.url) continue;
          fetched++;
          try {
            add(this.fileRow((await this.json<CanvasFile>(it.url)).data, m.name));
          } catch (err) {
            if (!(err instanceof CanvasForbidden)) throw err;
          }
        } else if (it.type === 'Page' && it.page_url && opts.pages) {
          const known = [...rows.values()].find((r) => r.kind === 'page' && r.url.endsWith(`/pages/${encodeURIComponent(it.page_url!)}`));
          if (known) known.module ??= m.name;
          else if (!pagesListed) add(this.pageRow(courseId, { url: it.page_url, title: it.title, html_url: it.html_url }, m.name));
        }
      }
    }
    return [...rows.values()];
  }

  /** A file's download. Its URL redirects to file storage, which gets no token. */
  async download(url: string): Promise<Response> {
    try {
      return (await this.request(url, '*/*')).res;
    } catch (err) {
      if (err instanceof CanvasForbidden) throw new BadRequest('This file is no longer available to you.');
      throw err;
    }
  }

  /** A page's body as Markdown, with its current `updated_at`. */
  async page(url: string): Promise<{ title: string; markdown: string; updated_at: string | null }> {
    const { data } = await this.json<{ title: string; body?: string | null; updated_at?: string }>(url).catch((err) => {
      if (err instanceof CanvasForbidden) throw new BadRequest('This page is no longer available to you.');
      throw err;
    });
    return { title: data.title, markdown: htmlToMarkdown(data.body ?? '', this.base), updated_at: data.updated_at ?? null };
  }

  /** The course as a hockeymask connector, for previews and other tools. */
  connector(courseId: string, opts: { pages: boolean }): Connector {
    return rowsConnector('canvas', () => this.items(courseId, opts));
  }
}
