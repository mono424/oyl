import { createVirtualizer } from '@tanstack/solid-virtual';
import { type Accessor, createEffect, createMemo, createSignal, For, type JSX, onCleanup, onMount, Show } from 'solid-js';

/** One window of rows, as useQuery returns it. */
export type ListWindow<T> = { data: () => readonly T[] | null | undefined; isSettled: () => boolean };

type Loaded<T> = { rows: T[]; settled: boolean };

/** The nearest ancestor that scrolls vertically: the page, for a list inside it. */
export function scrollParent(el: HTMLElement): HTMLElement | null {
  for (let p = el.parentElement; p; p = p.parentElement) {
    if (/(auto|scroll)/.test(getComputedStyle(p).overflowY)) return p;
  }
  return null;
}

/**
 * sp00ky's windowed infinite scroll (docs: client/pagination): one live query
 * per page window, another window as the reader nears the end, and only the
 * rows in view rendered. Every window stays live, so inserts, edits and
 * deletes reconcile in place.
 */
export function VirtualList<T>(props: {
  /**
   * Registers window `page`: LIMIT size START page * size, with a total order.
   * Called in the window's own scope, so a useQuery there (with
   * `deregisterOnCleanup`) goes away with the list.
   */
  window: (page: number, size: number) => ListWindow<T>;
  pageSize?: number;
  /** Row height before it is measured, in pixels. */
  estimateSize: number;
  key: (row: T) => string;
  children: (row: Accessor<T>) => JSX.Element;
  /** Shown once the first window is settled and empty. */
  empty?: JSX.Element;
  /** Scroll with the page (the nearest scrolling ancestor) instead of in a box of its own. */
  pageScroll?: boolean;
  class?: string;
  'aria-label'?: string;
}) {
  const size = () => props.pageSize ?? 50;
  const [pages, setPages] = createSignal(1);
  const [windows, setWindows] = createSignal<Record<number, Loaded<T> | undefined>>({});
  const report = (page: number, loaded: Loaded<T> | undefined) => setWindows((w) => ({ ...w, [page]: loaded }));

  // Contiguous rows from the first window. A window still loading, or a short
  // (final) page, ends them.
  const rows = createMemo(() => {
    const out: T[] = [];
    for (let p = 0; p < pages(); p++) {
      const w = windows()[p];
      if (!w) break;
      out.push(...w.rows);
      if (w.rows.length < size()) break;
    }
    return out;
  });
  // A settled short window is the true end; an unsettled one may still be filling.
  const canGrow = () => {
    const last = windows()[pages() - 1];
    return !!last && last.settled && last.rows.length >= size();
  };
  const isEmpty = () => !!windows()[0]?.settled && rows().length === 0;

  let root!: HTMLDivElement;
  let scroller: HTMLElement | null = null;
  // With the page scrolling, the list starts this far into it.
  const [margin, setMargin] = createSignal(0);
  const virtualizer = createVirtualizer({
    get count() {
      return rows().length;
    },
    get scrollMargin() {
      return margin();
    },
    // Asked before the list is in the page too: look up the page once it is.
    getScrollElement: () => {
      if (!props.pageScroll) return root ?? null;
      if (!scroller && root?.isConnected) scroller = scrollParent(root);
      return scroller;
    },
    estimateSize: () => props.estimateSize,
    getItemKey: (i) => props.key(rows()[i]),
    overscan: 6,
  });

  // Ask for the next window while the reader is still a few rows from the end.
  createEffect(() => {
    const last = virtualizer.getVirtualItems().at(-1);
    if (last && last.index >= rows().length - 8 && canGrow()) setPages(pages() + 1);
  });

  // Content above the list moves it: measure again on resize and on scroll.
  onMount(() => {
    if (!props.pageScroll) return;
    const page = (scroller ??= scrollParent(root));
    if (!page) return;
    const measure = () => setMargin(Math.round(root.getBoundingClientRect().top - page.getBoundingClientRect().top + page.scrollTop));
    measure();
    // The virtualizer mounted before the page was known: attach it now.
    virtualizer._willUpdate();
    const observer = new ResizeObserver(measure);
    for (let el = root.parentElement; el && el !== page; el = el.parentElement) observer.observe(el);
    observer.observe(page);
    page.addEventListener('scroll', measure, { passive: true });
    onCleanup(() => {
      observer.disconnect();
      page.removeEventListener('scroll', measure);
    });
  });

  return (
    <div ref={root} class={`${props.pageScroll ? '' : 'overflow-y-auto'} ${props.class ?? ''}`} aria-label={props['aria-label']}>
      <For each={Array.from({ length: pages() }, (_, p) => p)}>
        {(page) => <Window page={page} size={size()} open={props.window} report={report} />}
      </For>
      <Show when={!isEmpty()} fallback={props.empty}>
        <ul class="relative" style={{ height: `${virtualizer.getTotalSize()}px` }}>
          <For each={virtualizer.getVirtualItems()}>
            {(item) => (
              <li
                data-index={item.index}
                ref={(el) => queueMicrotask(() => virtualizer.measureElement(el))}
                class="absolute inset-x-0 top-0"
                style={{ transform: `translateY(${item.start - virtualizer.options.scrollMargin}px)` }}
              >
                <Show when={rows()[item.index]}>{(row) => props.children(row)}</Show>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </div>
  );
}

/** One live window. It reports its rows up and withdraws them when unmounted. */
function Window<T>(props: {
  page: number;
  size: number;
  open: (page: number, size: number) => ListWindow<T>;
  report: (page: number, loaded: Loaded<T> | undefined) => void;
}) {
  const result = props.open(props.page, props.size);
  // A fresh array, so a same-length change (a delete shifting the next row in) still lands.
  createEffect(() => props.report(props.page, { rows: [...(result.data() ?? [])], settled: result.isSettled() }));
  onCleanup(() => props.report(props.page, undefined));
  return null;
}
