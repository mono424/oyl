import { A, useParams, useSearchParams } from '@solidjs/router';
import { createMemo, createResource, For, lazy, Show, Suspense, type JSX } from 'solid-js';
import { useDb, useQuery } from '@spooky-sync/client-solid';
import { ArrowLeft, FileText } from 'lucide-solid';
import type { schema } from '../schema.gen';
import { syncBlob } from '../lib/api';
import { useAuth } from '../lib/auth';
import { asText, authorLabel, fileList, formatBytes, MAX_SHOWN_BYTES } from '../lib/cloud-papers';
import { idKey, idString } from '../lib/ids';
import { qPaperRevisions, useCloudPaper } from '../lib/query';
import { EngineChip } from '../components/EngineChip';
import { Card, formatDate, Loader, PageLoader } from '../components/ui';

// CodeMirror is large: loaded when a file is shown.
const SourceView = lazy(() => import('../components/SourceView'));

type Shown = { kind: 'text'; text: string } | { kind: 'binary' } | { kind: 'large' };

/**
 * One synced paper, read-only: a file of a revision (the entry file of the
 * latest one unless `?rev=` and `?file=` pick others) with the revisions and
 * that revision's files beside it. Mounted per paper (`perParam` in App.tsx).
 */
export default function CloudPaperPage() {
  const db = useDb<typeof schema>();
  const auth = useAuth();
  const params = useParams();
  const [search] = useSearchParams();
  const paperId = `paper_sync:${params.paperKey}`;
  const paper = useCloudPaper(db, auth.userId, () => paperId);
  const revisions = useQuery(() => qPaperRevisions(db, paperId));

  const revKey = () => (typeof search.rev === 'string' && search.rev ? search.rev : null);
  const selected = createMemo(() => {
    const list = revisions.data() ?? [];
    const key = revKey();
    if (key) return list.find((r) => idKey(r.id) === key) ?? null;
    return list.find((r) => idString(r.id) === idString(paper.data()?.head)) ?? list[0] ?? null;
  });
  const latest = () => Boolean(selected()) && idString(selected()!.id) === idString(paper.data()?.head);
  const files = createMemo(() => fileList(selected()?.files));
  const filePath = () => (typeof search.file === 'string' && search.file ? search.file : paper.data()?.main ?? '');
  const file = createMemo(() => files().find(([path]) => path === filePath())?.[1] ?? null);

  // Keyed by the blob, so moving between revisions that share the file reads it once.
  const [shown] = createResource(
    () => {
      const f = file();
      return f ? `${f.sha256}:${f.size}` : null;
    },
    async (key): Promise<Shown> => {
      const [sha, size] = key.split(':');
      if (Number(size) > MAX_SHOWN_BYTES) return { kind: 'large' };
      const text = asText(await syncBlob(db, sha));
      return text === null ? { kind: 'binary' } : { kind: 'text', text };
    },
  );

  const href = (rev: string | null, path: string | null) => {
    const q = new URLSearchParams();
    if (rev) q.set('rev', rev);
    if (path) q.set('file', path);
    const query = q.toString();
    return `/papers/${params.paperKey}${query ? `?${query}` : ''}`;
  };
  const chosenFile = () => (typeof search.file === 'string' && search.file ? search.file : null);

  return (
    <div class="mx-auto w-full min-w-0 max-w-6xl px-4 pb-8 pt-5 sm:px-8 sm:pb-14">
      <A href="/papers" class="inline-flex h-9 items-center gap-1.5 rounded-lg text-[14px] text-muted transition-colors hover:text-fg">
        <ArrowLeft size={16} stroke-width={1.75} /> Papers
      </A>
      <Show
        when={paper.data()}
        fallback={
          <Show when={!paper.isLoading()} fallback={<PageLoader label="Opening the paper" />}>
            <Card class="mt-4 p-6 text-[15px] text-muted">This paper is not in your account. It may have been removed.</Card>
          </Show>
        }
      >
        {(p) => (
          <>
            <div class="mt-2 flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1">
              <h1 class="min-w-0 break-words font-display text-[28px] font-normal leading-tight tracking-[-0.01em] sm:text-[32px]">{p().name}</h1>
              <EngineChip engine={p().engine} />
              <Show when={p().deleted_at}>
                <span class="rounded-md bg-warning-soft px-1.5 text-[12px] text-warning">Removed from the cloud</span>
              </Show>
            </div>
            <p class="mt-1.5 text-[14px] leading-relaxed text-muted [overflow-wrap:anywhere]">
              Entry file <span class="font-mono text-[13px] text-fg">{p().main}</span>
              <Show when={p().head}>
                , updated {formatDate(p().updated_at, true)}
                <Show when={authorLabel(p().updated_by)}>{(who) => <> from {who()}</>}</Show>
              </Show>
            </p>

            <div class="mt-6 grid min-w-0 gap-5 lg:grid-cols-[minmax(0,1fr)_288px]">
              <section class="min-w-0" aria-label="Source">
                <Show when={selected()}>
                  {(rev) => (
                    <div class="mb-2.5 flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-[13px] text-muted">
                      <span class="min-w-0 truncate font-mono text-[13px] text-fg">{filePath()}</span>
                      <Show when={file()}>{(f) => <span>{formatBytes(f().size)}</span>}</Show>
                      <span class="ml-auto">
                        Revision {rev().n}
                        <Show when={!latest()}>
                          , not the latest.{' '}
                          <A href={href(null, chosenFile())} class="text-fg underline decoration-line-strong underline-offset-2 hover:decoration-fg">
                            Show the latest
                          </A>
                        </Show>
                      </span>
                    </div>
                  )}
                </Show>
                <Show
                  when={selected()}
                  fallback={
                    <Placeholder>
                      <Show when={!revisions.isLoading()} fallback={<Loader size={6} label="Loading revisions" />}>
                        {revKey()
                          ? 'That revision is not among the latest 100 of this paper.'
                          : 'Nothing synced yet. Oyl on your Mac uploads the paper with its first sync.'}
                      </Show>
                    </Placeholder>
                  }
                >
                  <Show when={file()} fallback={<Placeholder>{filePath()} is not in this revision. Pick a file beside it.</Placeholder>}>
                    {(f) => (
                      <Show when={!shown.error} fallback={<Placeholder>{shown.error?.message ?? 'The file could not be loaded.'}</Placeholder>}>
                        <Show when={!shown.loading && shown()} fallback={<Placeholder><Loader size={6} label="Loading the file" /></Placeholder>}>
                          {(s) => {
                            const view = s();
                            return view.kind === 'text' ? (
                              <Suspense fallback={<Placeholder><Loader size={6} label="Opening the source" /></Placeholder>}>
                                <SourceView text={view.text} path={filePath()} />
                              </Suspense>
                            ) : (
                              <Placeholder>
                                {view.kind === 'large'
                                  ? `This file is ${formatBytes(f().size)}, too large to show here.`
                                  : `This is not a text file (${formatBytes(f().size)}).`}
                              </Placeholder>
                            );
                          }}
                        </Show>
                      </Show>
                    )}
                  </Show>
                </Show>
              </section>

              <aside class="min-w-0 space-y-5" aria-label="History and files">
                <Card class="p-1.5">
                  <h2 class="px-3 pb-1 pt-2.5 text-[13px] text-muted">Revisions</h2>
                  <ul class="scroll-quiet max-h-[22rem] overflow-y-auto">
                    <For each={revisions.data() ?? []} fallback={<li class="px-3 py-3 text-[13px] text-faint">None yet</li>}>
                      {(r) => {
                        const isHead = () => idString(r.id) === idString(p().head);
                        const active = () => idString(r.id) === idString(selected()?.id);
                        return (
                          <li>
                            <A
                              href={href(isHead() ? null : idKey(r.id), chosenFile())}
                              class={`block rounded-lg px-3 py-2 transition-colors ${active() ? 'bg-accent-soft' : 'hover:bg-soft'}`}
                              aria-current={active() ? 'true' : undefined}
                            >
                              <span class="flex items-center gap-2 text-[14px]">
                                <span class="font-medium tabular-nums">Revision {r.n}</span>
                                <Show when={isHead()}>
                                  <span class="rounded-md bg-soft px-1.5 text-[11px] text-muted">Latest</span>
                                </Show>
                                <span class="ml-auto shrink-0 text-[12px] text-muted">{formatDate(r.created_at, true)}</span>
                              </span>
                              <span class="mt-0.5 block truncate text-[13px] text-muted">
                                {authorLabel(r.author)}, {r.file_count} {r.file_count === 1 ? 'file' : 'files'}
                              </span>
                              <Show when={r.message}>
                                <span class="mt-0.5 line-clamp-2 block text-[13px] [overflow-wrap:anywhere]">{r.message}</span>
                              </Show>
                            </A>
                          </li>
                        );
                      }}
                    </For>
                  </ul>
                </Card>

                <Show when={files().length > 0}>
                  <Card class="p-1.5">
                    <h2 class="px-3 pb-1 pt-2.5 text-[13px] text-muted">Files in revision {selected()?.n}</h2>
                    <ul class="scroll-quiet max-h-[22rem] overflow-y-auto">
                      <For each={files()}>
                        {([path, f]) => {
                          const active = () => path === filePath();
                          return (
                            <li>
                              <A
                                href={href(revKey(), path === p().main ? null : path)}
                                class={`flex h-9 items-center gap-2 rounded-lg px-3 text-[13px] transition-colors ${active() ? 'bg-accent-soft text-fg' : 'text-muted hover:bg-soft hover:text-fg'}`}
                                aria-current={active() ? 'true' : undefined}
                                title={path}
                              >
                                <FileText size={15} stroke-width={1.6} class="shrink-0" />
                                <span class="min-w-0 flex-1 truncate font-mono text-[12px]">{path}</span>
                                <Show when={path === p().main}>
                                  <span class="shrink-0 rounded-md bg-soft px-1.5 text-[11px] text-muted">Entry</span>
                                </Show>
                                <span class="shrink-0 text-[12px] tabular-nums text-faint">{formatBytes(f.size)}</span>
                              </A>
                            </li>
                          );
                        }}
                      </For>
                    </ul>
                  </Card>
                </Show>
              </aside>
            </div>
          </>
        )}
      </Show>
    </div>
  );
}

/** Stands where the source would be, at its height. */
function Placeholder(props: { children: JSX.Element }) {
  return (
    <div class="flex h-[60dvh] min-w-0 items-center justify-center rounded-2xl border border-line bg-card px-6 text-center text-[14px] leading-relaxed text-muted [overflow-wrap:anywhere] sm:h-[70vh]">
      {props.children}
    </div>
  );
}
