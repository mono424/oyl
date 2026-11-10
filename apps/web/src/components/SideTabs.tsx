import { createMemo, createSignal, For, Show } from 'solid-js';
import { A } from '@solidjs/router';
import { useDb, useQuery } from '@spooky-sync/client-solid';
import { Check, Plus, Quote, Search } from 'lucide-solid';
import type { schema } from '../schema.gen';
import { latexToMarkdown } from '../../../../shared/latex';
import { texCitesToMarkdown } from '../../../../shared/references';
import { idKey, idString } from '../lib/ids';
import { renderMarkdown } from '../lib/markdown';
import { firstRow, qDocument } from '../lib/query';
import { Button, inputClass } from './ui';
import { MaterialBody } from './Papers';

/**
 * The tabs of a document's side panel (routes/Document.tsx) beyond its
 * preview and papers: anything of the course opened beside the text, a
 * material (its PDF or text) or another of the student's documents, and
 * the list to open them from.
 */

export interface Openable {
  id: string;
  title: string;
}

/** The course's material and documents, to open in a tab. */
export function OpenPicker(props: { materials: Openable[]; documents: Openable[]; open: ReadonlySet<string>; onOpen: (id: string) => void }) {
  const [query, setQuery] = createSignal('');
  const match = (list: Openable[]) => {
    const q = query().trim().toLowerCase();
    return q ? list.filter((o) => o.title.toLowerCase().includes(q)) : list;
  };
  const section = (title: string, list: () => Openable[], empty: string) => (
    <div>
      <p class="px-3 pb-1 pt-3 text-[12px] font-medium uppercase tracking-wide text-faint">{title}</p>
      <ul>
        <For each={list()} fallback={<li class="px-3 py-2 text-[13px] text-faint">{query().trim() ? 'Nothing matches.' : empty}</li>}>
          {(o) => (
            <li>
              <button
                type="button"
                class="flex w-full items-center gap-2 rounded-xl px-3 py-2 text-left text-[14px] transition-colors hover:bg-soft"
                onClick={() => props.onOpen(o.id)}
              >
                <span class="min-w-0 flex-1 truncate">{o.title}</span>
                <Show when={props.open.has(o.id)} fallback={<Plus size={15} stroke-width={1.9} class="shrink-0 text-faint" />}>
                  <Check size={15} stroke-width={1.9} class="shrink-0 text-faint" />
                </Show>
              </button>
            </li>
          )}
        </For>
      </ul>
    </div>
  );
  return (
    <div class="scroll-quiet h-full min-h-0 overflow-y-auto overscroll-contain p-2">
      <div class="relative px-1 pt-1">
        <Search size={15} stroke-width={1.9} class="pointer-events-none absolute left-4 top-1/2 mt-0.5 -translate-y-1/2 text-faint" />
        <input
          class={`${inputClass} pl-9 text-[14px]`}
          placeholder="Find in the course"
          value={query()}
          onInput={(e) => setQuery(e.currentTarget.value)}
          aria-label="Find in the course"
          ref={(el) => queueMicrotask(() => el.focus({ preventScroll: true }))}
        />
      </div>
      {section('Material', () => match(props.materials), 'This course has no material yet.')}
      {section('Documents', () => match(props.documents), 'No other documents in this course.')}
    </div>
  );
}

/**
 * A material open in a tab. When it is one of the document's papers it is
 * cited from here, with the page being read; otherwise it can become one.
 */
export function MaterialTab(props: { materialId: string; title: string; isPaper: boolean; onCite: (locator?: string) => void; onAddPaper: () => void }) {
  const [page, setPage] = createSignal(1);
  const [isPdf, setIsPdf] = createSignal(false);
  return (
    <div class="flex h-full min-h-0 flex-col">
      <div class="flex shrink-0 items-center gap-1.5 border-b border-line py-1.5 pl-3.5 pr-2">
        <span class="min-w-0 flex-1 truncate text-[13px] font-medium">{props.title}</span>
        <Show
          when={props.isPaper}
          fallback={
            <Button variant="soft" size="sm" onClick={props.onAddPaper} title="Add it to the papers this document cites">
              <Plus size={13} stroke-width={1.9} /> Add to papers
            </Button>
          }
        >
          <Button variant="soft" size="sm" onClick={() => props.onCite()} title="Cite it at the cursor">
            <Quote size={13} stroke-width={1.9} /> Cite
          </Button>
          <Show when={isPdf()}>
            <Button variant="soft" size="sm" onClick={() => props.onCite(`p. ${page()}`)} title="Cite the page you are reading">
              p. {page()}
            </Button>
          </Show>
        </Show>
      </div>
      <div class="scroll-quiet min-h-0 flex-1 overflow-y-auto overscroll-contain p-3">
        <MaterialBody materialId={props.materialId} page={null} onPage={setPage} onPdf={setIsPdf} />
      </div>
    </div>
  );
}

/** Another of the student's documents, rendered, to read beside this one. */
export function DocumentTab(props: { documentId: string; courseKey: string }) {
  const db = useDb<typeof schema>();
  const doc = firstRow(useQuery(() => qDocument(db, props.documentId)));
  const html = createMemo(() => {
    const d = doc.data();
    if (!d) return '';
    return renderMarkdown(d.format === 'tex' ? latexToMarkdown(texCitesToMarkdown(d.content ?? ''), { colors: true }) : (d.content ?? ''));
  });
  return (
    <Show when={doc.data()} fallback={<p class="py-8 text-center text-[14px] text-muted">{doc.isLoading() ? 'Loading…' : 'This document was deleted.'}</p>}>
      {(d) => (
        <div class="flex h-full min-h-0 flex-col">
          <div class="flex shrink-0 items-center gap-1.5 border-b border-line py-1.5 pl-3.5 pr-2">
            <span class="min-w-0 flex-1 truncate text-[13px] font-medium">{d().title}</span>
            <A href={`/courses/${props.courseKey}/documents/${idKey(idString(d().id))}`} class="shrink-0 px-2 text-[13px] text-muted underline underline-offset-2 hover:text-fg">
              Edit it
            </A>
          </div>
          <div class="scroll-quiet min-h-0 flex-1 overflow-y-auto overscroll-contain px-5 py-4">
            <Show when={html().trim()} fallback={<p class="text-[14px] text-muted">Nothing written yet.</p>}>
              <div class="lecture" innerHTML={html()} />
            </Show>
          </div>
        </div>
      )}
    </Show>
  );
}
