import { A } from '@solidjs/router';
import { createSignal, For, onCleanup, Show } from 'solid-js';
import { useDb } from '@spooky-sync/client-solid';
import { FileText, Search, X } from 'lucide-solid';
import type { schema } from '../schema.gen';
import { materialPath, pageLabel, searchCourse, type SearchHit } from '../lib/api';
import { IconButton, inputClass, Loader } from './ui';

const excerpt = (text: string, length = 220) => {
  const line = text.replace(/\s+/g, ' ').trim();
  return line.length > length ? `${line.slice(0, length).trimEnd()}…` : line;
};

/**
 * Searches a course's material the way the tutor does (meaning and keywords).
 * Each hit links to its place: a PDF opens at the page.
 */
export function CourseSearch(props: { courseId: string }) {
  const db = useDb<typeof schema>();
  const [query, setQuery] = createSignal('');
  const [hits, setHits] = createSignal<SearchHit[] | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  let seq = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(timer));

  const run = async (q: string) => {
    clearTimeout(timer);
    const mine = ++seq;
    if (q.trim().length < 3) {
      setHits(null);
      setBusy(false);
      setError(null);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const { hits } = await searchCourse(db, props.courseId, q.trim());
      if (mine === seq) setHits(hits);
    } catch (err) {
      if (mine === seq) setError(err instanceof Error ? err.message : 'Search failed. Try again.');
    } finally {
      if (mine === seq) setBusy(false);
    }
  };

  const clear = () => {
    setQuery('');
    void run('');
  };

  return (
    <section aria-label="Search this course" class="min-w-0">
      <form
        role="search"
        class="relative"
        onSubmit={(e) => {
          e.preventDefault();
          void run(query());
        }}
      >
        <Search size={16} stroke-width={1.75} class="pointer-events-none absolute left-3.5 top-1/2 -translate-y-1/2 text-muted" />
        <input
          type="search"
          class={`${inputClass} pl-10 pr-11`}
          placeholder="Search the lecture material"
          aria-label="Search the lecture material"
          value={query()}
          onInput={(e) => {
            const q = e.currentTarget.value;
            setQuery(q);
            clearTimeout(timer);
            timer = setTimeout(() => void run(q), 400);
          }}
        />
        <Show when={query()}>
          <IconButton label="Clear search" size="sm" type="button" class="absolute right-1 top-1/2 -translate-y-1/2" onClick={clear}>
            <X size={15} stroke-width={1.9} />
          </IconButton>
        </Show>
      </form>

      <div role="status" aria-live="polite" class="min-w-0">
        <Show when={busy() && !hits()}>
          <Loader label="Searching" class="ml-3 mt-4 text-faint" />
        </Show>
        <Show when={error()}>
          <p class="mt-3 break-words text-[13px] text-red">{error()}</p>
        </Show>
        <Show when={hits()}>
          {(list) => (
            <Show when={list().length} fallback={<p class="mt-3 text-[13px] text-muted">Nothing in this course matches.</p>}>
              <ol class={`scroll-quiet mt-3 max-h-[45dvh] space-y-1 overflow-y-auto overscroll-contain transition-opacity ${busy() ? 'opacity-60' : ''}`}>
                <For each={list()}>
                  {(hit) => (
                    <li>
                      <A
                        href={materialPath(props.courseId, hit.material, hit.page)}
                        class="block min-w-0 rounded-xl px-3 py-2.5 transition-colors hover:bg-soft"
                      >
                        <span class="flex min-w-0 items-center gap-1.5 text-[13px] text-muted">
                          <FileText size={13} stroke-width={1.75} class="shrink-0" />
                          <span class="truncate font-medium text-fg">{hit.materialTitle}</span>
                          <Show when={pageLabel(hit.page, hit.pageEnd) ?? hit.heading}>
                            {(where) => <span class="min-w-0 shrink truncate">· {where()}</span>}
                          </Show>
                        </span>
                        <span class="mt-1 line-clamp-2 block break-words text-[14px] leading-relaxed text-muted">{excerpt(hit.text)}</span>
                      </A>
                    </li>
                  )}
                </For>
              </ol>
            </Show>
          )}
        </Show>
      </div>
    </section>
  );
}
