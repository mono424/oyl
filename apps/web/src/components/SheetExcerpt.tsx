import { createEffect, createSignal, For, on, onCleanup, onMount, Show } from 'solid-js';
import type { PDFDocumentProxy } from 'pdfjs-dist';
import { renderMarkdown } from '../lib/markdown';
import type { Segment } from '../lib/sheet-regions';

/**
 * The exercise a homework answer is for: the page(s) of the PDF sheet it is
 * on, the part lit and marked at its edge, the rest dimmed, scrolled so the
 * part sits near the top. Without a region (a LaTeX sheet, a part whose
 * label was not found) its text. Pages are rendered once per width and
 * shared by every excerpt from them.
 */

const pages = new Map<string, Promise<HTMLCanvasElement>>();

function renderPage(pdf: PDFDocumentProxy, n: number, width: number): Promise<HTMLCanvasElement> {
  const key = `${pdf.fingerprints[0]}:${n}:${width}`;
  let hit = pages.get(key);
  if (!hit) {
    hit = (async () => {
      const page = await pdf.getPage(n);
      const base = page.getViewport({ scale: 1 });
      const viewport = page.getViewport({ scale: width / base.width });
      const canvas = document.createElement('canvas');
      canvas.width = Math.floor(viewport.width);
      canvas.height = Math.floor(viewport.height);
      await page.render({ canvas, viewport }).promise;
      return canvas;
    })();
    pages.set(key, hit);
    hit.catch(() => pages.delete(key));
    // A few pages at a time: canvases are large on phones.
    while (pages.size > 6) pages.delete(pages.keys().next().value!);
  }
  return hit;
}

/** Scrolls the nearest scrolling panel so `el` (the part) sits near its top. */
function reveal(el: HTMLElement) {
  let box = el.parentElement;
  while (box && !/(auto|scroll)/.test(getComputedStyle(box).overflowY)) box = box.parentElement;
  if (!box) return;
  const offset = el.getBoundingClientRect().top - box.getBoundingClientRect().top;
  box.scrollTo({ top: box.scrollTop + offset - 24, behavior: 'smooth' });
}

export function SheetExcerpt(props: { pdf: PDFDocumentProxy | null; segments: Segment[]; prompt: string; context?: string | null }) {
  let host!: HTMLDivElement;
  const [width, setWidth] = createSignal(0);
  // Whole pages, each with the bands of the part on it.
  const [pages, setPages] = createSignal<{ canvas: HTMLCanvasElement; bands: { top: number; bottom: number }[] }[]>([]);
  const [failed, setFailed] = createSignal(false);

  onMount(() => {
    const observer = new ResizeObserver(([entry]) => setWidth(Math.round(entry.contentRect.width)));
    observer.observe(host);
    onCleanup(() => observer.disconnect());
  });

  createEffect(
    on([() => props.pdf, () => JSON.stringify(props.segments), width], ([pdf, , w]) => {
      setPages([]);
      setFailed(false);
      if (!pdf || !props.segments.length || w < 40) return;
      let cancelled = false;
      onCleanup(() => {
        cancelled = true;
      });
      const px = Math.round(w * Math.min(window.devicePixelRatio || 1, 2));
      void (async () => {
        try {
          const numbers = [...new Set(props.segments.map((seg) => seg.page))];
          const out = [];
          for (const n of numbers) {
            const page = await renderPage(pdf, n, px);
            // The cached canvas is shared: show a copy.
            const copy = document.createElement('canvas');
            copy.width = page.width;
            copy.height = page.height;
            copy.getContext('2d')!.drawImage(page, 0, 0);
            copy.className = 'block h-auto w-full';
            out.push({ canvas: copy, bands: props.segments.filter((seg) => seg.page === n).map((seg) => ({ top: seg.top, bottom: seg.bottom })) });
          }
          if (!cancelled) setPages(out);
        } catch {
          if (!cancelled) setFailed(true);
        }
      })();
    }),
  );

  const usePdf = () => !!props.pdf && props.segments.length > 0 && !failed();

  return (
    <div ref={host} class="min-w-0">
      <Show
        when={usePdf()}
        fallback={
          <div class="lecture text-[14px] leading-relaxed">
            <Show when={props.context}>
              <div class="mb-2 text-muted" innerHTML={renderMarkdown(props.context!)} />
            </Show>
            <div innerHTML={renderMarkdown(props.prompt || '')} />
          </div>
        }
      >
        <Show when={pages().length} fallback={<div class="h-96 animate-pulse rounded-lg bg-soft" />}>
          <div class="space-y-3">
            <For each={pages()}>
              {(p, i) => (
                <div class="sheet-excerpt relative overflow-hidden rounded-md border border-line bg-white">
                  {p.canvas}
                  <For each={p.bands}>
                    {(b, j) => (
                      <>
                        {/* Everything but the part, dimmed; the part itself marked at its edge. */}
                        <div class="pointer-events-none absolute inset-x-0 top-0 bg-white/60" style={{ height: `${b.top * 100}%` }} />
                        <div class="pointer-events-none absolute inset-x-0 bottom-0 bg-white/60" style={{ height: `${(1 - b.bottom) * 100}%` }} />
                        <div
                          ref={(el) => i() === 0 && j() === 0 && queueMicrotask(() => reveal(el))}
                          class="pointer-events-none absolute left-0 w-[3px] rounded-full bg-blue/70"
                          style={{ top: `${b.top * 100}%`, height: `${(b.bottom - b.top) * 100}%` }}
                        />
                      </>
                    )}
                  </For>
                </div>
              )}
            </For>
          </div>
        </Show>
      </Show>
    </div>
  );
}
