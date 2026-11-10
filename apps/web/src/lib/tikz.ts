import DOMPurify from 'dompurify';

/**
 * TikZ pictures, drawn by TeX itself in the browser. A ```tikz block (what
 * shared/latex.ts makes of a tikzpicture, or written in Markdown) is the
 * preamble the picture needs, then the picture. TikZJax's TeX, compiled to
 * WebAssembly, runs it in a worker and dvi2html turns the result into an SVG.
 * Its files are served under /tikzjax/ (vite.config.ts) and load with the
 * first picture.
 *
 * `renderMarkdown` puts a placeholder per picture into its HTML
 * (`tikzPlaceholder`). Once placeholders are in the page they are drawn, one
 * at a time, and every placeholder of the same picture fills in. Drawn
 * pictures are kept in memory (the preview of an edited document keeps its
 * pictures as it re-renders) and in Cache Storage (they draw at once on the
 * next visit). The SVGs are sanitized: TeX's \special can write raw markup.
 */

const ROOT = `${import.meta.env.BASE_URL}tikzjax`;
const CACHE = 'tikz-v1';
const TIMEOUT_MS = 30_000;
const MAX_KEPT = 200;

interface Picture {
  source: string;
  svg?: string;
  error?: string;
}

const pictures = new Map<string, Picture>();
const queue: string[] = [];
let pumping: Promise<void> | null = null;

/** A short stable hash of the picture's source (cyrb53). */
function keyOf(source: string): string {
  let h1 = 0xdeadbeef;
  let h2 = 0x41c6ce57;
  for (let i = 0; i < source.length; i++) {
    const c = source.charCodeAt(i);
    h1 = Math.imul(h1 ^ c, 2654435761);
    h2 = Math.imul(h2 ^ c, 1597334677);
  }
  h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^ Math.imul(h2 ^ (h2 >>> 13), 3266489909);
  h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^ Math.imul(h1 ^ (h1 >>> 13), 3266489909);
  return (4294967296 * (2097151 & h2) + (h1 >>> 0)).toString(36);
}

const escape = (s: string) => s.replace(/[&<>"]/g, (c) => `&${{ '&': 'amp', '<': 'lt', '>': 'gt', '"': 'quot' }[c]};`);

function inner(p: Picture): string {
  if (p.svg) return p.svg;
  if (p.error) {
    return `<p class="tikz-note">The picture could not be drawn: ${escape(p.error)}</p><pre><code>${escape(p.source)}</code></pre>`;
  }
  return '<p class="tikz-note">Drawing the picture…</p>';
}

const state = (p: Picture) => (p.svg ? 'tikz' : p.error ? 'tikz tikz-error' : 'tikz tikz-pending');

/** The HTML of a picture: drawn if it was before, a placeholder that fills in once it is in the page otherwise. */
export function tikzPlaceholder(source: string): string {
  const key = keyOf(source);
  let p = pictures.get(key);
  if (p) {
    // Most recently used last.
    pictures.delete(key);
  } else {
    p = { source };
    for (const old of pictures.keys()) {
      if (pictures.size < MAX_KEPT) break;
      pictures.delete(old);
    }
  }
  pictures.set(key, p);
  watch();
  return `<div class="${state(p)}" data-tikz="${key}">${inner(p)}</div>`;
}

// ── Finding placeholders in the page ─────────────────────────────────────

let observer: MutationObserver | null = null;

function watch() {
  if (observer) return;
  observer = new MutationObserver((mutations) => {
    for (const m of mutations) {
      for (const node of m.addedNodes) {
        if (!(node instanceof Element)) continue;
        if (node.matches('.tikz-pending')) request(node);
        for (const el of node.querySelectorAll('.tikz-pending')) request(el);
      }
    }
  });
  observer.observe(document.body, { childList: true, subtree: true });
}

function request(el: Element) {
  const key = (el as HTMLElement).dataset.tikz;
  if (!key || !pictures.has(key)) return;
  const p = pictures.get(key)!;
  if (p.svg || p.error) return fill(key);
  if (!queue.includes(key)) queue.push(key);
  pumping ??= pump().finally(() => (pumping = null));
}

function fill(key: string) {
  const p = pictures.get(key);
  if (!p) return;
  for (const el of document.querySelectorAll(`[data-tikz="${key}"]`)) {
    el.className = state(p);
    el.innerHTML = inner(p);
  }
}

const shown = (key: string) => !!document.querySelector(`[data-tikz="${key}"]`);

async function pump() {
  for (let key = queue.shift(); key; key = queue.shift()) {
    const p = pictures.get(key);
    // Skipped once its placeholder left the page (a version of an edited picture).
    if (!p || p.svg || p.error || !shown(key)) continue;
    const kept = await cached(key);
    if (kept) {
      p.svg = kept;
    } else {
      try {
        p.svg = await draw(key, p.source);
        void keep(key, p.svg);
      } catch (e) {
        p.error = e instanceof Error ? e.message : 'TeX stopped.';
      }
    }
    fill(key);
  }
}

/** Resolves once the pictures in the page are drawn (or could not be): Export PDF waits for them. */
export async function tikzSettled(): Promise<void> {
  while (pumping) await pumping;
}

// ── Cache Storage ──────────────────────────────────────────────────────────

const cacheUrl = (key: string) => `/tikz-cache/${key}.svg`;

async function cached(key: string): Promise<string | null> {
  try {
    const hit = await (await caches.open(CACHE)).match(cacheUrl(key));
    return hit ? await hit.text() : null;
  } catch {
    return null;
  }
}

async function keep(key: string, svg: string) {
  try {
    await (await caches.open(CACHE)).put(cacheUrl(key), new Response(svg, { headers: { 'Content-Type': 'image/svg+xml' } }));
  } catch {
    // Not kept: drawn again next time.
  }
}

// ── TeX ────────────────────────────────────────────────────────────────────

/** The picture as TeX input: its preamble (packages it uses added) and the picture. */
function texInput(source: string): { preamble: string; body: string } {
  const lines = source.split('\n');
  let at = lines.findIndex((l) => /^\s*\\begin\{(tikzpicture|tikzcd)\}/.test(l) || /^\s*\\tikz\b/.test(l));
  if (at < 0) at = lines.findIndex((l) => !/^\s*\\(usepackage|usetikzlibrary|usepgfplotslibrary|tikzset|pgfplotsset|definecolor|colorlet|tikzstyle|providecommand|renewcommand|newcommand|def)\b/.test(l));
  if (at < 0) at = lines.length;
  let preamble = lines.slice(0, at).join('\n');
  const body = lines.slice(at).join('\n');
  const loads = (pkg: string) => new RegExp(`\\\\usepackage(\\[[^\\]]*\\])?\\{[^}]*\\b${pkg}\\b`).test(preamble);
  const extra: string[] = [];
  if (/\\begin\{tikzcd\}/.test(body) && !loads('tikz-cd')) extra.push('tikz-cd');
  if (/\\begin\{(axis|semilogxaxis|semilogyaxis|loglogaxis|polaraxis)\}/.test(body) && !loads('pgfplots')) extra.push('pgfplots');
  if (/\$|\\\(|\\\[|\\ensuremath/.test(body) && !loads('amssymb')) extra.push('amsmath', 'amssymb');
  if (extra.length) preamble = `\\usepackage{${extra.join(',')}}\n${preamble}`;
  return { preamble, body };
}

interface Call {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
}

let worker: Worker | null = null;
let ready: Promise<void> | null = null;
let uid = 0;
const calls = new Map<number, Call>();
let log: string[] = [];

/** The worker speaks the `threads` protocol: run messages in, a result or an error per call out. */
function call(method: string, args: unknown[]): Promise<unknown> {
  const id = ++uid;
  return new Promise((resolve, reject) => {
    calls.set(id, { resolve, reject });
    worker!.postMessage({ type: 'run', uid: id, method, args });
  });
}

function stop(error: Error) {
  worker?.terminate();
  worker = null;
  ready = null;
  for (const c of calls.values()) c.reject(error);
  calls.clear();
}

/**
 * The worker, wrapped: TeX asks for a file it does not have by fetching its
 * name. Only TikZJax's own files are fetched; anything else (a \input of a
 * URL, a file TikZJax lacks, which the app's SPA fallback would answer with
 * its index page) is not found.
 */
function workerUrl(): string {
  const root = new URL(ROOT, location.href).href;
  const code = `const root = ${JSON.stringify(root)};
const get = self.fetch.bind(self);
self.fetch = (input, init) => {
  const url = new URL(typeof input === 'string' ? input : input.url, root + '/');
  return url.href.startsWith(root + '/') && url.pathname.endsWith('.gz') ? get(url, init) : Promise.resolve(new Response(null, { status: 404 }));
};
importScripts(root + '/run-tex.js');`;
  return URL.createObjectURL(new Blob([code], { type: 'text/javascript' }));
}

function start(): Promise<void> {
  if (ready) return ready;
  if (!document.querySelector('link[data-tikz-fonts]')) {
    const link = document.createElement('link');
    link.rel = 'stylesheet';
    link.href = `${ROOT}/fonts.css`;
    link.dataset.tikzFonts = '';
    document.head.append(link);
  }
  const w = new Worker(workerUrl());
  worker = w;
  let initialized!: () => void;
  const init = new Promise<void>((resolve) => (initialized = resolve));
  w.onmessage = (e: MessageEvent) => {
    const m = e.data;
    if (typeof m === 'string') {
      log.push(m);
      return;
    }
    if (m?.type === 'init') return initialized();
    const c = calls.get(m?.uid);
    if (m?.type === 'result' && m.complete && c) {
      calls.delete(m.uid);
      c.resolve(m.payload);
    } else if (m?.type === 'error' && c) {
      calls.delete(m.uid);
      c.reject(new Error(m.error?.message ?? 'TeX stopped.'));
    } else if (m?.type === 'uncaughtError') {
      stop(new Error(m.error?.message ?? 'TeX stopped.'));
    }
  };
  w.onerror = () => stop(new Error('TeX could not be loaded.'));
  const loading = init.then(() => call('load', [new URL(ROOT, location.href).href]) as Promise<void>);
  ready = loading;
  loading.catch(() => undefined);
  return loading;
}

/** TeX's own message about what went wrong, if it gave one: its first `!` line and where. */
function texError(lines: string[]): string | null {
  const at = lines.findIndex((l) => l.startsWith('!'));
  if (at < 0) return null;
  // Its line number counts the preamble TikZJax adds: left out.
  const where = lines.slice(at + 1).find((l) => /^l\.\d+/.test(l))?.replace(/^l\.\d+\s*/, '');
  return [lines[at].replace(/^!\s*/, ''), where && `At: ${where}`].filter(Boolean).join(' ');
}

async function draw(key: string, source: string): Promise<string> {
  const { preamble, body } = texInput(source);
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    await start();
    log = [];
    const html = (await Promise.race([
      call('texify', [body, { showConsole: 'true', addToPreamble: preamble }]),
      new Promise((_r, reject) => (timer = setTimeout(() => reject(new Error('TeX took too long.')), TIMEOUT_MS))),
    ])) as string;
    if (!/<svg[\s>]/.test(html)) throw new Error('TeX drew nothing.');
    return clean(key, html);
  } catch (e) {
    // The worker keeps state from a failed run: the next picture gets a fresh one.
    stop(new Error('TeX stopped.'));
    throw new Error(texError(log) ?? (e instanceof Error ? e.message : 'TeX stopped.'));
  } finally {
    clearTimeout(timer);
  }
}

/** The SVG, sanitized, its pgf ids made unique to the picture (several pictures share a page). */
function clean(key: string, html: string): string {
  let svg = DOMPurify.sanitize(html, { USE_PROFILES: { svg: true, svgFilters: true } });
  const ids = [...new Set([...svg.matchAll(/\bid="(pgf[^"]*)"/g)].map((m) => m[1]))].sort((a, b) => b.length - a.length);
  for (const id of ids) svg = svg.replaceAll(id, `pgf-${key}-${id.slice(3)}`);
  return svg;
}
