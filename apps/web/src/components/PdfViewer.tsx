import { createEffect, createSignal, For, on, onCleanup, Show } from 'solid-js';
import { useDb } from '@spooky-sync/client-solid';
import { Download, Minus, Plus } from 'lucide-solid';
import type { PDFDocumentProxy, RenderTask } from 'pdfjs-dist';
import type { schema } from '../schema.gen';
import { fetchPdf } from '../lib/api';
import { loadPdfjs } from '../lib/pdfjs';
import { Button, IconButton, Loader } from './ui';


const ZOOMS = [1, 1.25, 1.5, 2, 3];

/**
 * The original PDF of a material, page by page. Pages are placeholders sized
 * like the page until they come near the screen; only those get a canvas, and
 * canvases far off screen are dropped again, which keeps phones within their
 * canvas memory. `page` scrolls that page into view (a citation link);
 * `onPage` follows the page being read.
 */
export function PdfViewer(props: {
  materialId: string;
  size?: number | null;
  fileName?: string | null;
  page?: number | null;
  onPage?: (page: number) => void;
}) {
  const db = useDb<typeof schema>();
  const [doc, setDoc] = createSignal<PDFDocumentProxy | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  // Width / height of each page at scale 1. Unrendered pages assume page 1's.
  const [ratios, setRatios] = createSignal<number[]>([]);
  const [width, setWidth] = createSignal(0);
  const [zoom, setZoom] = createSignal(0);
  const [current, setCurrent] = createSignal(1);
  // Bumped by "Try again": a failed load is not cached, so this fetches anew.
  const [attempt, setAttempt] = createSignal(0);
  createEffect(() => props.onPage?.(current()));
  const pages: HTMLDivElement[] = [];
  const near = new Set<number>();
  const tasks = new Map<number, RenderTask>();

  // Load the document whenever the material changes, or on a retry.
  createEffect(
    on(
      [() => props.materialId, attempt],
      ([id]) => {
        let cancelled = false;
        let loading: { destroy(): Promise<void> } | null = null;
        setDoc(null);
        setError(null);
        void (async () => {
          try {
            const [lib, buf] = await Promise.all([loadPdfjs(), fetchPdf(db, id, props.size)]);
            if (cancelled) return;
            const task = lib.getDocument({ data: new Uint8Array(buf.slice(0)) });
            loading = task;
            const pdf = await task.promise;
            if (cancelled) return;
            const first = (await pdf.getPage(1)).getViewport({ scale: 1 });
            setRatios(Array.from({ length: pdf.numPages }, () => first.width / first.height));
            setDoc(pdf);
          } catch (err) {
            if (!cancelled) setError(err instanceof Error ? err.message : 'The PDF could not be loaded.');
          }
        })();
        onCleanup(() => {
          cancelled = true;
          for (const t of tasks.values()) t.cancel();
          tasks.clear();
          near.clear();
          void loading?.destroy();
        });
      },
    ),
  );

  const pageWidth = () => Math.max(1, width() * ZOOMS[zoom()]);

  const render = async (n: number) => {
    const pdf = doc();
    const el = pages[n - 1];
    if (!pdf || !el || tasks.has(n) || el.firstChild) return;
    const page = await pdf.getPage(n);
    const base = page.getViewport({ scale: 1 });
    const ratio = base.width / base.height;
    if (Math.abs((ratios()[n - 1] ?? ratio) - ratio) > 0.001) setRatios((r) => r.map((v, i) => (i === n - 1 ? ratio : v)));
    if (!near.has(n) || doc() !== pdf) return;
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const viewport = page.getViewport({ scale: (pageWidth() / base.width) * dpr });
    const canvas = document.createElement('canvas');
    canvas.width = Math.floor(viewport.width);
    canvas.height = Math.floor(viewport.height);
    canvas.className = 'block h-full w-full';
    const task = page.render({ canvas, viewport });
    tasks.set(n, task);
    try {
      await task.promise;
      if (near.has(n)) el.replaceChildren(canvas);
    } catch {
      // Cancelled: the page left the screen or the zoom changed.
    } finally {
      tasks.delete(n);
    }
  };

  const release = (n: number) => {
    tasks.get(n)?.cancel();
    tasks.delete(n);
    pages[n - 1]?.replaceChildren();
  };

  // A new size or zoom: draw the pages near the screen again, sharply.
  createEffect(
    on(pageWidth, () => {
      for (const n of [...tasks.keys()]) release(n);
      for (let n = 1; n <= pages.length; n++) if (!near.has(n)) release(n);
      for (const n of near) {
        release(n);
        void render(n);
      }
    }, { defer: true }),
  );

  let nearObserver: IntersectionObserver | undefined;
  let currentObserver: IntersectionObserver | undefined;
  createEffect(() => {
    if (!doc()) return;
    // Two screens ahead and behind get drawn; the middle band of the screen
    // decides which page the indicator shows.
    nearObserver = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          const n = Number((e.target as HTMLElement).dataset.page);
          if (e.isIntersecting) {
            near.add(n);
            void render(n);
          } else {
            near.delete(n);
            release(n);
          }
        }
      },
      { rootMargin: '200% 0px' },
    );
    currentObserver = new IntersectionObserver(
      (entries) => {
        for (const e of entries) if (e.isIntersecting) setCurrent(Number((e.target as HTMLElement).dataset.page));
      },
      { rootMargin: '-45% 0px -45% 0px' },
    );
    queueMicrotask(() => {
      for (const el of pages) {
        if (!el) continue;
        nearObserver?.observe(el);
        currentObserver?.observe(el);
      }
    });
    onCleanup(() => {
      nearObserver?.disconnect();
      currentObserver?.disconnect();
    });
  });

  // A citation link: bring the page into view once the pages are laid out.
  createEffect(
    on([doc, () => props.page, width], ([pdf, page, w]) => {
      if (!pdf || !page || !w) return;
      const el = pages[Math.min(Math.max(page, 1), pdf.numPages) - 1];
      if (el) requestAnimationFrame(() => el.scrollIntoView({ block: 'start' }));
    }),
  );

  const download = async () => {
    const buf = await fetchPdf(db, props.materialId, props.size);
    const url = URL.createObjectURL(new Blob([buf], { type: 'application/pdf' }));
    const a = document.createElement('a');
    a.href = url;
    a.download = props.fileName || 'lecture.pdf';
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };

  let resize: ResizeObserver | undefined;
  onCleanup(() => resize?.disconnect());
  const measure = (el: HTMLDivElement) => {
    resize = new ResizeObserver(([entry]) => setWidth(Math.floor(entry.contentRect.width)));
    resize.observe(el);
  };

  return (
    <div class="relative min-w-0">
      {/* The viewer's controls stay on top while the pages scroll. */}
      <Show when={doc()}>
        <div class="sticky top-3 z-10 mb-3 flex justify-center">
          <div class="flex items-center gap-1 rounded-full border border-line bg-surface/95 px-2 py-1 shadow-card backdrop-blur">
            <span class="px-2 text-[13px] tabular-nums text-muted">
              p. {current()} / {doc()!.numPages}
            </span>
            <IconButton label="Zoom out" size="sm" disabled={zoom() === 0} onClick={() => setZoom((z) => Math.max(0, z - 1))}>
              <Minus size={16} stroke-width={1.9} />
            </IconButton>
            <IconButton label="Zoom in" size="sm" disabled={zoom() === ZOOMS.length - 1} onClick={() => setZoom((z) => Math.min(ZOOMS.length - 1, z + 1))}>
              <Plus size={16} stroke-width={1.9} />
            </IconButton>
            <IconButton label="Download the PDF" size="sm" onClick={() => void download()}>
              <Download size={16} stroke-width={1.9} />
            </IconButton>
          </div>
        </div>
      </Show>
      <div ref={measure} class="w-full">
        <Show
          when={doc()}
          fallback={
            <div class="flex flex-col items-center gap-3 py-10 text-center">
              <Show when={error()} fallback={<Loader size={7} pattern="download" label="Loading the PDF" />}>
                <p class="text-[15px] text-muted" role="alert">
                  {error()}
                </p>
              </Show>
              <Show when={error()}>
                <Button variant="soft" size="sm" onClick={() => setAttempt((n) => n + 1)}>
                  Try again
                </Button>
              </Show>
            </div>
          }
        >
          <div class="scroll-quiet overflow-x-auto">
            <div class="mx-auto flex flex-col gap-3 sm:gap-4" style={{ width: `${pageWidth()}px` }}>
              <For each={ratios()}>
                {(ratio, i) => (
                  <div
                    ref={(el) => (pages[i()] = el)}
                    data-page={i() + 1}
                    role="img"
                    aria-label={`Page ${i() + 1}`}
                    class="scroll-mt-20 overflow-hidden rounded-md bg-white shadow-card"
                    style={{ width: `${pageWidth()}px`, height: `${Math.round(pageWidth() / ratio)}px` }}
                  />
                )}
              </For>
            </div>
          </div>
        </Show>
      </div>
    </div>
  );
}
