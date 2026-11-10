import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from 'solid-js';
import { type GetTable, type TableModel, useDb, useQuery } from '@spooky-sync/client-solid';
import { History, X } from 'lucide-solid';
import type { schema } from '../schema.gen';
import { type DocumentVersion, documentVersion } from '../lib/api';
import { qDocumentVersions } from '../lib/query';
import { type Piece, textChanges } from '../lib/text-diff';
import { Button, formatDate, IconButton, Segmented } from './ui';

/**
 * A document's history (schema `document_version`). The database keeps it by
 * itself, whoever writes. In the side panel: the list, by day. In place of the
 * editor: the version picked, rendered or as what it changed, with the button
 * that brings it back as a new version. The list is live; the text comes from
 * the API, which rebuilds it from its changesets.
 */

export type VersionRow = TableModel<GetTable<typeof schema, 'document_version'>>;

const asDate = (v: unknown) => (v instanceof Date ? v : new Date(String(v)));
const time = (v: unknown) => asDate(v).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });

function dayOf(v: unknown): string {
  const date = asDate(v);
  const today = new Date();
  const yesterday = new Date(today.getFullYear(), today.getMonth(), today.getDate() - 1);
  if (date.toDateString() === today.toDateString()) return 'Today';
  if (date.toDateString() === yesterday.toDateString()) return 'Yesterday';
  return formatDate(date);
}

/** Who wrote a version, or what it did. */
function what(v: { author: string; restored_from?: number | null }): string {
  const who = v.author === 'assistant' ? 'AI assistant' : v.author === 'system' ? 'Earlier text' : 'You';
  return v.restored_from ? `${who}, back to version ${v.restored_from}` : who;
}

/** The side panel's list of versions, newest first, in groups by day. */
export function VersionList(props: { documentId: string; selected: number | null; onSelect: (n: number) => void }) {
  const db = useDb<typeof schema>();
  const versions = useQuery(() => qDocumentVersions(db, props.documentId));
  const rows = () => (versions.data() ?? []) as VersionRow[];
  const days = createMemo(() => {
    const groups: { day: string; rows: VersionRow[] }[] = [];
    for (const row of rows()) {
      const day = dayOf(row.updated_at);
      if (groups.at(-1)?.day === day) groups.at(-1)!.rows.push(row);
      else groups.push({ day, rows: [row] });
    }
    return groups;
  });
  return (
    <div class="scroll-quiet h-full min-h-0 overflow-y-auto overscroll-contain p-2">
      <Show
        when={rows().length}
        fallback={
          <div class="px-3 py-8 text-center">
            <p class="text-[14px] font-medium">{versions.isLoading() ? 'Loading…' : 'No versions yet'}</p>
            <Show when={!versions.isLoading()}>
              <p class="mx-auto mt-1 max-w-72 text-[13px] text-muted">Every change to the text is kept here from now on, yours and your AI assistant's.</p>
            </Show>
          </div>
        }
      >
        <For each={days()}>
          {(group) => (
            <section>
              <p class="px-3 pb-1 pt-3 text-[12px] font-medium uppercase tracking-wide text-faint">{group.day}</p>
              <ul>
                <For each={group.rows}>
                  {(row) => (
                    <li>
                      <button
                        type="button"
                        aria-current={props.selected === row.n}
                        class={`w-full rounded-xl px-3 py-2 text-left transition-colors hover:bg-soft ${props.selected === row.n ? 'bg-soft' : ''}`}
                        onClick={() => props.onSelect(row.n)}
                      >
                        <span class="flex items-baseline gap-2">
                          <span class="text-[14px] font-medium">{time(row.updated_at)}</span>
                          <Show when={row.n === rows()[0]?.n}>
                            <span class="text-[12px] text-muted">Current</span>
                          </Show>
                          <span class="ml-auto shrink-0 text-[12px] tabular-nums">
                            <Show when={row.added}>
                              <span class="text-green">+{row.added.toLocaleString()}</span>
                            </Show>{' '}
                            <Show when={row.removed}>
                              <span class="text-red">-{row.removed.toLocaleString()}</span>
                            </Show>
                          </span>
                        </span>
                        <span class="mt-0.5 block truncate text-[12px] text-muted">
                          Version {row.n} · {what(row)}
                        </span>
                      </button>
                    </li>
                  )}
                </For>
              </ul>
            </section>
          )}
        </For>
      </Show>
    </div>
  );
}

/** One version in place of the editor. Mount it per version (a keyed Show). */
export function VersionView(props: {
  documentId: string;
  n: number;
  /** Changes when the version does: it is the latest one, and someone writes on. */
  updatedAt?: unknown;
  isCurrent: boolean;
  /** The rendered text. */
  render: (content: string, format: string) => string;
  onRestore: (version: DocumentVersion) => Promise<void>;
  onClose: () => void;
}) {
  const db = useDb<typeof schema>();
  const [version, setVersion] = createSignal<DocumentVersion | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [mode, setMode] = createSignal<'preview' | 'changes'>('preview');
  const [restoring, setRestoring] = createSignal(false);

  let latest = 0;
  createEffect(
    on(
      () => String(props.updatedAt ?? ''),
      () => {
        const request = ++latest;
        void documentVersion(db, props.documentId, props.n).then(
          (v) => request === latest && (setVersion(v), setError(null)),
          (err) => request === latest && setError(err instanceof Error ? err.message : 'This version could not be read.'),
        );
      },
    ),
  );
  onCleanup(() => (latest = -1));

  onMount(() => {
    const key = (e: KeyboardEvent) => {
      if (e.key === 'Escape') props.onClose();
    };
    window.addEventListener('keydown', key);
    onCleanup(() => window.removeEventListener('keydown', key));
  });

  // Apart from `error`: the version is read again with every save, which clears that one.
  const [restoreError, setRestoreError] = createSignal<string | null>(null);
  const restore = async () => {
    const v = version();
    if (!v) return;
    setRestoring(true);
    setRestoreError(null);
    try {
      await props.onRestore(v);
    } catch (err) {
      setRestoreError(err instanceof Error ? err.message : 'This version could not be restored.');
      setRestoring(false);
    }
  };

  const html = createMemo(() => {
    const v = version();
    return v ? props.render(v.content, v.format) : '';
  });
  const changes = createMemo<Piece[]>(() => {
    const v = version();
    return v && mode() === 'changes' ? textChanges(v.previous, v.content) : [];
  });

  return (
    <div class="mx-auto max-w-3xl px-1 sm:px-4">
      <div class="sticky top-3 z-10 flex flex-wrap items-center gap-x-3 gap-y-2 rounded-2xl border border-line bg-surface px-3.5 py-2 shadow-card">
        <History size={16} stroke-width={1.75} class="shrink-0 text-muted" />
        <div class="min-w-0 flex-1 basis-40">
          <p class="truncate text-[14px] font-medium">Version {props.n}</p>
          <Show when={version()}>
            {(v) => (
              <p class="truncate text-[12px] text-muted">
                {formatDate(v().updated_at, true)} · {what(v())}
              </p>
            )}
          </Show>
        </div>
        <Segmented
          label="Show this version as"
          value={mode()}
          onChange={setMode}
          options={[
            { value: 'preview', label: 'Preview' },
            { value: 'changes', label: 'Changes' },
          ]}
        />
        <Show when={!props.isCurrent}>
          <Button size="sm" disabled={restoring() || !version()} onClick={() => void restore()} title="Bring this text back as a new version. Nothing is lost: what the document holds now stays in the history. The papers stay as they are.">
            {restoring() ? 'Restoring…' : 'Restore'}
          </Button>
        </Show>
        <IconButton label="Back to the current text (Esc)" size="sm" onClick={props.onClose}>
          <X size={16} stroke-width={1.75} />
        </IconButton>
      </div>

      <Show when={restoreError() ?? error()}>
        <p role="alert" class="mt-3 rounded-xl bg-soft px-3.5 py-2 text-[13px] text-muted">{restoreError() ?? error()}</p>
      </Show>
      <Show when={version()} fallback={<Show when={!error()}><div class="mt-6 h-40 animate-pulse rounded-xl bg-soft" /></Show>}>
        <div class="pb-24 pt-6">
          <Show
            when={mode() === 'changes'}
            fallback={
              <Show when={html().trim()} fallback={<p class="text-[14px] text-muted">Nothing to show in this version yet.</p>}>
                <div class="lecture" innerHTML={html()} />
              </Show>
            }
          >
            <VersionChanges pieces={changes()} first={props.n === 1} />
          </Show>
        </div>
      </Show>
    </div>
  );
}

function VersionChanges(props: { pieces: Piece[]; first: boolean }) {
  return (
    <Show
      when={props.pieces.some((p) => p.kind === 'added' || p.kind === 'removed')}
      fallback={<p class="text-[14px] text-muted">{props.first ? 'This version has no text.' : 'The text did not change in this version.'}</p>}
    >
      <Show when={props.first}>
        <p class="mb-3 text-[13px] text-muted">The first version: all of its text is new.</p>
      </Show>
      <div class="whitespace-pre-wrap break-words font-mono text-[13px] leading-relaxed">
        <For each={props.pieces}>
          {(piece) =>
            piece.kind === 'skip' ? (
              <div class="my-3 select-none border-y border-line py-1 text-center font-sans text-[12px] text-faint">
                {piece.lines.toLocaleString()} unchanged {piece.lines === 1 ? 'line' : 'lines'}
              </div>
            ) : piece.kind === 'added' ? (
              <ins class="rounded-sm bg-green/15 text-fg no-underline">{piece.text}</ins>
            ) : piece.kind === 'removed' ? (
              <del class="rounded-sm bg-red-soft text-red">{piece.text}</del>
            ) : (
              <span class="text-muted">{piece.text}</span>
            )
          }
        </For>
      </div>
    </Show>
  );
}

