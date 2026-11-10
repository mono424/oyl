import { A, useParams, useSearchParams } from '@solidjs/router';
import { createEffect, createMemo, createResource, createSignal, For, lazy, Match, on, onCleanup, onMount, Show, Suspense, Switch, type JSX, untrack } from 'solid-js';
import { type GetTable, type TableModel, useDb, useQuery } from '@spooky-sync/client-solid';
import { FileArchive, FileText, History, PanelRight, X } from 'lucide-solid';
import type { schema } from '../schema.gen';
import { ensureLiveFiles, type SyncFiles, syncBlob } from '../lib/api';
import { useAuth } from '../lib/auth';
import { type Paper, withReferences } from '../lib/citations';
import {
  asText,
  authorLabel,
  fileMap,
  formatBytes,
  inlineInputs,
  isLiveFile,
  isTexSource,
  MAX_SHOWN_BYTES,
  parseBib,
  texCiteStyle,
} from '../lib/cloud-papers';
import { latexToMarkdown } from '../../../../shared/latex';
import { texCitesToMarkdown } from '../../../../shared/references';
import { idKey, idString } from '../lib/ids';
import { renderMarkdown } from '../lib/markdown';
import { commitOnHead, LIVE_FIELD, liveText, type OpenLiveFile, openLiveFile, withTexts } from '../lib/paper-live';
import { qPaperFiles, qPaperRevisions, useCloudPaper } from '../lib/query';
import { textChanges } from '../lib/text-diff';
import { EngineChip } from '../components/EngineChip';
import { Focus, HeaderEndActions, HeaderTitle } from '../components/HeaderSlot';
import { SidePanel } from '../components/SidePanel';
import { VersionChanges } from '../components/Versions';
import { download, DownloadMenu } from '../components/DownloadMenu';
import { Button, Card, formatDate, IconButton, Loader, PageLoader, Segmented } from '../components/ui';

// CodeMirror is large: loaded when a file is shown.
const PaperEditor = lazy(() => import('../components/PaperEditor'));

/**
 * A synced paper, written here as in Oyl on the Mac, in the LaTeX editor's
 * layout (routes/Document.tsx): a file of it on the left and on the right, in
 * a panel as wide as it is dragged, tabs: the rendered paper (LaTeX), its
 * files and its revisions.
 *
 * Its text files are written live (lib/paper-live.ts): each is a sp00ky CRDT
 * document that this page, other browsers and Oyl on the Mac edit at once,
 * every keystroke merged into everyone's text as it arrives. Revisions stay
 * the history. The Mac makes one a few seconds after its file changes; this
 * page makes one of what was typed here once typing has paused a while, and
 * when it is left. A revision picked in the history shows in place of the
 * editor, with what it changed, and can be brought back. `?file=` picks a file
 * (the entry file without it), `?rev=` a revision. Mounted per paper
 * (`perParam` in App.tsx).
 */

type PaperRow = TableModel<GetTable<typeof schema, 'paper_sync'>>;
type RevisionRow = TableModel<GetTable<typeof schema, 'paper_revision'>>;
type FileRow = TableModel<GetTable<typeof schema, 'paper_file'>>;

const PANEL_WIDTH_KEY = 'paper-panel-width';
const panelKey = (paperId: string) => `paper-panel:${paperId}`;
/** The largest file the preview reads to put in place of its `\input`. */
const MAX_PREVIEW_FILE = 1024 * 1024;
/** A change from a Mac this recent shows it writing. */
const LIVE_MS = 20_000;
/** Typing paused this long: what was typed here becomes a revision. */
const CHECKPOINT_MS = 15_000;

export default function CloudPaperPage() {
  const db = useDb<typeof schema>();
  const auth = useAuth();
  const params = useParams();
  const paperId = `paper_sync:${params.paperKey}`;
  const paper = useCloudPaper(db, auth.userId, () => paperId);
  const revisions = useQuery(() => qPaperRevisions(db, paperId));
  const liveFiles = useQuery(() => qPaperFiles(db, paperId));
  return (
    <Show
      when={paper.data()}
      fallback={
        <Show when={!paper.isLoading()} fallback={<PageLoader label="Opening the paper" />}>
          <div class="mx-auto w-full max-w-3xl px-4 pt-6 sm:px-8">
            <Card class="p-6 text-[15px] text-muted">
              This paper is not in your account. It may have been removed.{' '}
              <A href="/papers" class="text-fg underline decoration-line-strong underline-offset-2 hover:decoration-fg">
                All papers
              </A>
            </Card>
          </div>
        </Show>
      }
    >
      {/* The row itself, not Show's narrowed value: sp00ky may update the same object. */}
      <Workspace
        paper={() => paper.data()!}
        revisions={() => (revisions.data() ?? []) as RevisionRow[]}
        revisionsLoading={revisions.isLoading}
        liveFiles={() => (liveFiles.data() ?? []) as FileRow[]}
      />
    </Show>
  );
}

function useMedia(query: string) {
  const list = matchMedia(query);
  const [matches, setMatches] = createSignal(list.matches);
  const update = () => setMatches(list.matches);
  list.addEventListener('change', update);
  onCleanup(() => list.removeEventListener('change', update));
  return matches;
}

const baseName = (path: string) => path.slice(path.lastIndexOf('/') + 1);
const fileName = (name: string) => name.replace(/[\\/:*?"<>|]+/g, ' ').trim() || 'paper';
const errorText = (err: unknown, fallback: string) => (err instanceof Error ? err.message : fallback);

function Workspace(props: { paper: () => PaperRow; revisions: () => RevisionRow[]; revisionsLoading: () => boolean; liveFiles: () => FileRow[] }) {
  const db = useDb<typeof schema>();
  const [search, setSearch] = useSearchParams();
  const wide = useMedia('(min-width: 1024px)');
  const paperId = idString(props.paper().id);
  const [notice, setNotice] = createSignal<string | null>(null);
  const removed = () => Boolean(props.paper().deleted_at);

  // ── The paper's files ──────────────────────────────────────────────────

  const head = createMemo<RevisionRow | null>(
    () => {
      const id = idString(props.paper().head);
      return (id && props.revisions().find((r) => idString(r.id) === id)) || null;
    },
    null,
    { equals: (a, b) => idString(a?.id) === idString(b?.id) },
  );
  const headFiles = createMemo<SyncFiles>(() => fileMap(head()?.files));
  const files = createMemo(() => Object.entries(headFiles()).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)));
  const live = createMemo(() => new Map(props.liveFiles().map((f) => [f.path, f] as const)));

  // Text files of the latest revision without their live document yet: the API makes them.
  const [preparing, setPreparing] = createSignal(false);
  createEffect(
    on(head, (h) => {
      if (!h || removed()) return;
      const missing = files().some(([p, f]) => isLiveFile(p, f.size) && !untrack(live).has(p));
      if (!missing || untrack(preparing)) return;
      setPreparing(true);
      ensureLiveFiles(db, paperId)
        .catch((err) => setNotice(`The files could not be prepared for writing here: ${errorText(err, 'try again in a moment.')}`))
        .finally(() => setPreparing(false));
    }),
  );

  const main = () => props.paper().main;
  const path = () => (typeof search.file === 'string' && search.file ? search.file : main());
  /** Moves to a file or a revision, keeping the rest of the address. */
  const show = (to: { file?: string; rev?: string | null }) =>
    setSearch({ ...(to.file !== undefined ? { file: to.file === main() ? undefined : to.file } : {}), ...(to.rev !== undefined ? { rev: to.rev ?? undefined } : {}) });

  // ── The live file shown ────────────────────────────────────────────────

  // Typed here and not in a revision yet, by path: the text, read when the revision is made.
  const typed = new Map<string, () => string>();
  const [version, setVersion] = createSignal(0);
  const shownRow = createMemo(() => live().get(path()) ?? null, null, { equals: (a, b) => idString(a?.id) === idString(b?.id) });
  const [opened, setOpened] = createSignal<{ path: string; file: OpenLiveFile } | null>(null);
  const [openError, setOpenError] = createSignal<string | null>(null);
  createEffect(
    on(shownRow, (row) => {
      setOpened(null);
      setOpenError(null);
      if (!row?.seed) return;
      let active = true;
      let handle: OpenLiveFile | null = null;
      let unsubscribe: (() => void) | null = null;
      const filePath = row.path;
      openLiveFile(db, { id: idString(row.id), path: filePath, seed: row.seed })
        .then((file) => {
          if (!active) return void file.close();
          handle = file;
          // The preview follows every change, typed here or not.
          unsubscribe = file.doc.subscribe(() => setVersion((n) => n + 1));
          setOpened({ path: filePath, file });
        })
        .catch((err) => active && setOpenError(errorText(err, 'The file could not be opened.')));
      onCleanup(() => {
        active = false;
        unsubscribe?.();
        if (!handle) return;
        // Kept as it is now for the revision still to come.
        if (typed.has(filePath)) {
          const text = liveText(handle.doc);
          typed.set(filePath, () => text);
        }
        void handle.close();
      });
    }),
  );
  const openedDoc = () => {
    const o = opened();
    return o && o.path === path() ? o.file.doc : null;
  };

  // ── Revisions of what is typed here ────────────────────────────────────

  const [checkpointing, setCheckpointing] = createSignal(false);
  let checkpointTimer: ReturnType<typeof setTimeout> | undefined;
  let checkpointRun: Promise<unknown> = Promise.resolve();
  const headRef = () => {
    const h = head();
    return h ? { id: idString(h.id), files: fileMap(h.files) } : null;
  };
  /** What was typed here, as a revision on the head. One at a time. */
  const checkpoint = () => {
    clearTimeout(checkpointTimer);
    checkpointRun = checkpointRun.then(async () => {
      if (!typed.size) return;
      const texts = new Map([...typed].map(([p, read]) => [p, read()]));
      setCheckpointing(true);
      try {
        await commitOnHead(db, paperId, headRef(), (base) => withTexts(db, base, texts));
        // Typed again meanwhile: those stay for the next one.
        for (const [p, text] of texts) if (typed.get(p)?.() === text) typed.delete(p);
      } catch (err) {
        console.warn('Saving a revision of the paper failed', err);
        checkpointTimer = setTimeout(checkpoint, CHECKPOINT_MS);
      } finally {
        setCheckpointing(false);
      }
    });
    return checkpointRun;
  };
  const edited = () => {
    const o = opened();
    if (!o) return;
    const doc = o.file.doc;
    typed.set(o.path, () => liveText(doc));
    clearTimeout(checkpointTimer);
    checkpointTimer = setTimeout(checkpoint, CHECKPOINT_MS);
  };
  const onHidden = () => document.visibilityState === 'hidden' && void checkpoint();
  document.addEventListener('visibilitychange', onHidden);
  window.addEventListener('pagehide', checkpoint);
  onCleanup(() => {
    document.removeEventListener('visibilitychange', onHidden);
    window.removeEventListener('pagehide', checkpoint);
    // Off to another page: what was typed still becomes a revision.
    void checkpoint();
  });

  // Edits on their way to the cloud.
  const [pending, setPending] = createSignal(db.pendingMutationCount);
  onCleanup(db.subscribeToPendingMutations((n) => setPending(n)));

  // ── Live ───────────────────────────────────────────────────────────────

  const [now, setNow] = createSignal(Date.now());
  const tick = setInterval(() => setNow(Date.now()), 5_000);
  onCleanup(() => clearInterval(tick));
  // A Mac changed the paper a moment ago: it is being written there.
  const liveWith = createMemo(() => {
    const p = props.paper();
    if (!p.updated_by?.startsWith('device:') || !p.updated_at) return null;
    return now() - new Date(p.updated_at as unknown as string).getTime() < LIVE_MS ? authorLabel(p.updated_by) : null;
  });
  const status = createMemo(() => {
    if (removed()) return 'Removed from the cloud, read-only';
    if (!props.paper().head) return 'Not synced yet';
    if (pending() > 0) return 'Syncing…';
    if (checkpointing()) return 'Saving a revision…';
    return head() ? `Live · revision ${head()!.n}` : 'Opening…';
  });

  // ── The panel's tabs ───────────────────────────────────────────────────

  const isLatex = () => props.paper().engine === 'latex' && isTexSource(main());
  const savedTab = (() => {
    try {
      return localStorage.getItem(panelKey(paperId));
    } catch {
      return null;
    }
  })();
  const [active, setActive] = createSignal(savedTab ?? (isLatex() ? 'preview' : 'files'));
  const current = createMemo(() => {
    const a = active();
    return a === 'preview' ? (isLatex() ? a : 'files') : a === 'history' ? a : 'files';
  });
  createEffect(() => {
    try {
      localStorage.setItem(panelKey(paperId), current());
    } catch {
      // Private windows: the tab is not remembered.
    }
  });
  const [showPanel, setShowPanel] = createSignal(wide());
  const openHistory = () => {
    setActive('history');
    setShowPanel(true);
  };

  // ── Preview (LaTeX) ────────────────────────────────────────────────────

  // LaTeX and BibTeX files that are not live, from their blobs, read once per version.
  const readTexts = new Map<string, string>();
  const [blobTexts, setBlobTexts] = createSignal<Record<string, string>>({});
  createEffect(
    on(headFiles, (all) => {
      if (!isLatex()) return;
      let current = true;
      onCleanup(() => (current = false));
      const wanted = Object.entries(all).filter(([p, f]) => (isTexSource(p) || /\.bib$/i.test(p)) && f.size <= MAX_PREVIEW_FILE);
      void Promise.all(
        wanted.map(async ([p, f]) => {
          let text = readTexts.get(f.sha256);
          if (text === undefined) {
            const bytes = await syncBlob(db, f.sha256).catch(() => null);
            text = bytes ? (asText(bytes) ?? '') : undefined;
            if (text !== undefined) readTexts.set(f.sha256, text);
          }
          return [p, text ?? ''] as const;
        }),
      ).then((list) => current && setBlobTexts(Object.fromEntries(list)));
    }),
  );
  /** A file's text as the preview reads it: live as typed when open here, else as last folded or committed. */
  const textFor = (p: string): string | undefined => {
    version();
    const doc = opened()?.path === p ? opened()!.file.doc : null;
    if (doc) return liveText(doc);
    return live().get(p)?.content ?? blobTexts()[p];
  };
  const papers = createMemo<Paper[]>(
    () =>
      files()
        .map(([p]) => p)
        .filter((p) => /\.bib$/i.test(p))
        .flatMap((p) => parseBib(textFor(p) ?? '')),
    [],
    { equals: (a, b) => JSON.stringify(a) === JSON.stringify(b) },
  );
  // \bibliographystyle and the packages sit in the entry file.
  const style = createMemo(() => texCiteStyle(textFor(main()) ?? ''));
  const previewing = () => showPanel() && current() === 'preview';
  const [previewSource, setPreviewSource] = createSignal('');
  let previewTimer: ReturnType<typeof setTimeout> | undefined;
  createEffect(() => {
    const text = previewing() && isLatex() ? inlineInputs(main(), textFor) : null;
    clearTimeout(previewTimer);
    if (text !== null) previewTimer = setTimeout(() => setPreviewSource(text), untrack(previewSource) ? 250 : 0);
  });
  onCleanup(() => clearTimeout(previewTimer));
  const preview = createMemo(() => {
    const tex = previewSource();
    return tex.trim() ? renderMarkdown(withReferences(latexToMarkdown(texCitesToMarkdown(tex), { colors: true }), papers(), style())) : '';
  });

  // ── Revisions ──────────────────────────────────────────────────────────

  const revKey = () => (typeof search.rev === 'string' && search.rev ? search.rev : null);
  const viewing = createMemo(() => {
    const key = revKey();
    return key ? (props.revisions().find((r) => idKey(r.id) === key) ?? null) : null;
  });
  const pickRevision = (r: RevisionRow) => {
    show({ rev: idKey(r.id) });
    if (!wide()) setShowPanel(false);
  };
  /**
   * Brings a revision back: its text goes into each live document as an edit
   * (merging with whatever is typed elsewhere meanwhile), then all of its files
   * become a new revision.
   */
  const restore = async (r: RevisionRow) => {
    await checkpoint();
    const back = fileMap(r.files);
    for (const [p, f] of Object.entries(back)) {
      const row = live().get(p);
      if (!row?.seed || f.size > MAX_SHOWN_BYTES) continue;
      const text = asText(await syncBlob(db, f.sha256));
      if (text === null) continue;
      const shown = opened()?.path === p ? opened()!.file : null;
      const file = shown ?? (await openLiveFile(db, { id: idString(row.id), path: p, seed: row.seed ?? '' }));
      file.doc.getText(LIVE_FIELD).update(text);
      file.doc.commit();
      if (!shown) await file.close();
    }
    const n = await commitOnHead(db, paperId, headRef(), async () => back, `Back to revision ${r.n}`);
    show({ rev: null });
    setNotice(
      n
        ? `Revision ${r.n} is back, as revision ${n}. Nothing is lost: the text from before stays in History.`
        : `The paper already holds the files of revision ${r.n}.`,
    );
    setTimeout(() => setNotice(null), 10_000);
  };

  // ── Downloads ──────────────────────────────────────────────────────────

  const bytesOf = async (p: string): Promise<Uint8Array<ArrayBuffer>> => {
    const text = live().has(p) ? textFor(p) : undefined;
    if (text !== undefined) return new TextEncoder().encode(text);
    const ref = headFiles()[p];
    if (!ref) throw new Error(`${p} is not in the paper.`);
    return (await syncBlob(db, ref.sha256)) as Uint8Array<ArrayBuffer>;
  };
  const downloadFile = async () => {
    try {
      download(baseName(path()), await bytesOf(path()), 'application/octet-stream');
    } catch (err) {
      setNotice(errorText(err, 'The file could not be downloaded.'));
    }
  };
  const downloadPaper = async () => {
    try {
      const all = await Promise.all(files().map(async ([p]) => [p, await bytesOf(p)] as const));
      const { zipSync } = await import('fflate');
      download(`${fileName(props.paper().name)}.zip`, zipSync(Object.fromEntries(all)) as Uint8Array<ArrayBuffer>, 'application/zip');
    } catch {
      setNotice('The paper could not be downloaded. Check your connection and try again.');
    }
  };

  onMount(() => {
    const key = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'e') {
        e.preventDefault();
        setShowPanel((v) => !v);
      }
    };
    window.addEventListener('keydown', key);
    onCleanup(() => window.removeEventListener('keydown', key));
  });

  // Wide screens: the panel's width, dragged at its edge and remembered.
  const [panelWidth, setPanelWidth] = createSignal(
    Number(localStorage.getItem(PANEL_WIDTH_KEY)) || Math.round(Math.min(760, Math.max(400, window.innerWidth * 0.4))),
  );
  const clampWidth = (w: number) => Math.round(Math.min(window.innerWidth * 0.7, Math.max(340, w)));

  const tabClass = (on: boolean) =>
    `flex h-8 shrink-0 items-center rounded-lg px-2.5 text-[13px] transition-colors ${on ? 'bg-soft font-medium text-fg' : 'text-muted hover:bg-soft hover:text-fg'}`;
  const layer = (on: boolean) => `absolute inset-0 ${on ? '' : 'pointer-events-none invisible'}`;
  const panel = () => (
    <div class="flex h-full min-h-0 flex-col">
      <div class="flex shrink-0 items-center gap-1 px-1.5 pb-0.5 pt-1.5">
        <div role="tablist" aria-label="Beside the source" class="scroll-quiet flex min-w-0 flex-1 items-center gap-0.5 overflow-x-auto">
          <Show when={isLatex()}>
            <button type="button" role="tab" aria-selected={current() === 'preview'} class={tabClass(current() === 'preview')} onClick={() => setActive('preview')}>
              Preview
            </button>
          </Show>
          <button type="button" role="tab" aria-selected={current() === 'files'} class={`${tabClass(current() === 'files')} gap-1`} onClick={() => setActive('files')}>
            Files <span class="font-normal text-faint">{files().length || ''}</span>
          </button>
          <button type="button" role="tab" aria-selected={current() === 'history'} class={tabClass(current() === 'history')} onClick={() => setActive('history')}>
            History
          </button>
        </div>
        <IconButton label="Hide the panel (⌘E)" size="sm" onClick={() => setShowPanel(false)}>
          <X size={15} stroke-width={1.9} />
        </IconButton>
      </div>
      <div class="relative min-h-0 flex-1">
        <Show when={isLatex()}>
          <div class={`${layer(current() === 'preview')} scroll-quiet overflow-y-auto overscroll-contain px-5 py-4 sm:px-7 sm:py-5`} aria-label="Preview">
            <Show when={preview()} fallback={<p class="text-[14px] text-muted">{head() ? 'The rendered paper shows here.' : 'The rendered paper shows here once it is synced.'}</p>}>
              <div class="lecture" innerHTML={preview()} />
            </Show>
          </div>
        </Show>
        <div class={`${layer(current() === 'files')} scroll-quiet overflow-y-auto overscroll-contain p-2`}>
          <ul>
            <For each={files()} fallback={<li class="px-3 py-8 text-center text-[13px] text-muted">{head() ? 'No files' : 'Nothing synced yet'}</li>}>
              {([p, f]) => {
                const on = () => p === path();
                return (
                  <li>
                    <button
                      type="button"
                      class={`flex h-9 w-full items-center gap-2 rounded-lg px-3 text-left text-[13px] transition-colors ${on() ? 'bg-accent-soft text-fg' : 'text-muted hover:bg-soft hover:text-fg'}`}
                      aria-current={on() ? 'true' : undefined}
                      title={p}
                      onClick={() => {
                        show({ file: p, rev: null });
                        if (!wide()) setShowPanel(false);
                      }}
                    >
                      <FileText size={15} stroke-width={1.6} class="shrink-0" />
                      <span class="min-w-0 flex-1 truncate font-mono text-[12px]">{p}</span>
                      <Show when={p === main()}>
                        <span class="shrink-0 rounded-md bg-soft px-1.5 text-[11px] text-muted">Entry</span>
                      </Show>
                      <span class="shrink-0 text-[12px] tabular-nums text-faint">{formatBytes(f.size)}</span>
                    </button>
                  </li>
                );
              }}
            </For>
          </ul>
        </div>
        <div class={`${layer(current() === 'history')} scroll-quiet overflow-y-auto overscroll-contain p-2`}>
          <ul>
            <For
              each={props.revisions()}
              fallback={
                <li class="px-3 py-8 text-center text-[13px] text-muted">
                  <Show when={!props.revisionsLoading()} fallback={<Loader size={6} label="Loading revisions" class="mx-auto text-faint" />}>
                    No revisions yet
                  </Show>
                </li>
              }
            >
              {(r) => {
                const isHead = () => idString(r.id) === idString(props.paper().head);
                const on = () => (viewing() ? idString(viewing()!.id) === idString(r.id) : isHead());
                return (
                  <li>
                    <button
                      type="button"
                      class={`block w-full rounded-xl px-3 py-2 text-left transition-colors ${on() ? 'bg-soft' : 'hover:bg-soft'}`}
                      aria-current={on() ? 'true' : undefined}
                      onClick={() => pickRevision(r)}
                    >
                      <span class="flex items-center gap-2 text-[14px]">
                        <span class="font-medium tabular-nums">Revision {r.n}</span>
                        <Show when={isHead()}>
                          <span class="text-[12px] text-muted">Current</span>
                        </Show>
                        <span class="ml-auto shrink-0 text-[12px] text-muted">{formatDate(r.created_at, true)}</span>
                      </span>
                      <span class="mt-0.5 block truncate text-[12px] text-muted">
                        {authorLabel(r.author)}, {r.file_count} {r.file_count === 1 ? 'file' : 'files'}
                      </span>
                      <Show when={r.message}>
                        <span class="mt-0.5 line-clamp-2 block text-[12px] [overflow-wrap:anywhere]">{r.message}</span>
                      </Show>
                    </button>
                  </li>
                );
              }}
            </For>
          </ul>
        </div>
      </div>
    </div>
  );

  // A file that is not live (not text, too large, or not prepared yet): its blob.
  const shownRef = createMemo(() => (shownRow() ? null : (headFiles()[path()] ?? null)), null, { equals: (a, b) => a?.sha256 === b?.sha256 });
  const [blob] = createResource(
    () => shownRef() ?? false,
    async (ref) => (ref.size > MAX_SHOWN_BYTES ? { kind: 'large' as const } : { kind: 'text' as const, text: asText(await syncBlob(db, ref.sha256)) }),
  );
  const editorKey = createMemo(() => {
    const doc = openedDoc();
    return doc ? { doc, path: path(), writable: !removed() } : null;
  }, null, { equals: (a, b) => a?.doc === b?.doc && a?.writable === b?.writable });
  const source = (
    <Switch fallback={<Placeholder><Loader size={6} label="Loading the file" /></Placeholder>}>
      <Match when={!props.paper().head}>
        <Placeholder>Nothing synced yet. Oyl on your Mac uploads the paper with its first sync.</Placeholder>
      </Match>
      <Match when={editorKey()} keyed>
        {(open) => (
          <Suspense fallback={<div class="m-4 h-40 animate-pulse rounded-xl bg-soft" />}>
            <Show
              when={open.writable}
              fallback={<PaperEditor path={open.path} text={liveText(open.doc)} label={`${open.path}, ${props.paper().name}`} />}
            >
              <PaperEditor path={open.path} live={open.doc} onEdit={edited} papers={papers} citeStyle={style} label={`${open.path}, ${props.paper().name}`} />
            </Show>
          </Suspense>
        )}
      </Match>
      <Match when={shownRow() && openError()}>
        <Placeholder>
          {openError()} <TextButton onClick={() => show({ file: main() })}>Open {main()}</TextButton>
        </Placeholder>
      </Match>
      <Match when={shownRow()}>
        <Placeholder><Loader size={6} label="Opening the live text" /></Placeholder>
      </Match>
      <Match when={head() && !headFiles()[path()]}>
        <Placeholder>
          {path()} is not in the paper. <TextButton onClick={() => show({ file: main() })}>Open {main()}</TextButton>
        </Placeholder>
      </Match>
      <Match when={shownRef() && isLiveFile(path(), shownRef()!.size) && preparing()}>
        <Placeholder><Loader size={6} label="Preparing the file for writing" /></Placeholder>
      </Match>
      <Match when={blob.error}>
        <Placeholder>{errorText(blob.error, 'The file could not be loaded.')}</Placeholder>
      </Match>
      <Match when={!blob.error && blob()}>
        {(_) => {
          const b = blob()!;
          return b.kind === 'text' && b.text !== null ? (
            <Suspense fallback={<div class="m-4 h-40 animate-pulse rounded-xl bg-soft" />}>
              <PaperEditor path={path()} text={b.text} label={`${path()}, ${props.paper().name}`} />
            </Suspense>
          ) : (
            <Placeholder>
              {b.kind === 'large' ? `This file is ${formatBytes(shownRef()!.size)}, too large to show here.` : `This is not a text file (${formatBytes(shownRef()!.size)}).`}{' '}
              <TextButton onClick={() => void downloadFile()}>Download it</TextButton>
            </Placeholder>
          );
        }}
      </Match>
    </Switch>
  );

  return (
    <div class="w-full px-3 sm:px-6">
      <Focus />
      <HeaderTitle>
        <div class="flex min-w-0 items-center gap-2.5">
          <A href="/papers" class="shrink-0 text-[16px] font-semibold tracking-tight text-muted transition-colors hover:text-fg sm:text-[17px]">
            Papers
          </A>
          <span class="text-faint">/</span>
          <h1 class="min-w-0 truncate text-[16px] font-semibold tracking-tight sm:text-[17px]" title={props.paper().name}>
            {props.paper().name}
          </h1>
          <EngineChip engine={props.paper().engine} class="max-sm:hidden" />
          <span class="hidden min-w-0 items-center gap-1.5 truncate text-[12px] text-muted sm:inline-flex" role="status">
            <Show when={liveWith() && !removed()} fallback={status()}>
              <span class="dot-breathe size-1.5 shrink-0 rounded-full bg-accent" aria-hidden="true" />
              Live with {liveWith()} · {pending() > 0 ? 'syncing…' : 'synced'}
            </Show>
          </span>
        </div>
      </HeaderTitle>
      <HeaderEndActions>
        <Button variant={showPanel() ? 'solid' : 'ghost'} size="sm" onClick={() => setShowPanel((v) => !v)} title="Show the preview, files and history beside the source (⌘E)">
          <PanelRight size={16} stroke-width={1.75} class="lg:hidden" />
          <span class="max-lg:sr-only">Panel</span> <span class="hidden opacity-60 lg:inline">⌘E</span>
        </Button>
        <IconButton label="History: earlier revisions of the paper" size="sm" onClick={openHistory}>
          <History size={16} stroke-width={1.75} />
        </IconButton>
        <DownloadMenu
          items={[
            { label: 'This file', detail: baseName(path()), icon: FileText, run: () => void downloadFile() },
            { label: 'The whole paper', detail: `${fileName(props.paper().name)}.zip, ${files().length} ${files().length === 1 ? 'file' : 'files'}`, icon: FileArchive, run: () => void downloadPaper() },
          ]}
        />
      </HeaderEndActions>

      <Show when={notice()}>
        <p class="mx-auto mt-3 flex max-w-3xl items-start gap-2 rounded-xl bg-soft px-3.5 py-2 text-[13px] text-muted" role="status">
          <span class="min-w-0 flex-1">{notice()}</span>
          <button type="button" class="shrink-0 text-faint hover:text-fg" aria-label="Dismiss" onClick={() => setNotice(null)}>
            <X size={14} stroke-width={2} />
          </button>
        </p>
      </Show>
      {/* Phones: the status that the top bar has no room for. */}
      <p class="mt-2 truncate text-center text-[12px] text-muted sm:hidden">{liveWith() ? `Live with ${liveWith()}` : status()}</p>

      <div class="relative mt-4 flex items-start gap-0">
        <div class="relative min-w-0 flex-1">
          <Show when={viewing()} keyed>
            {(r) => (
              <RevisionView
                revision={r}
                parent={props.revisions().find((x) => idString(x.id) === idString(r.parent)) ?? null}
                path={path()}
                isCurrent={idString(r.id) === idString(props.paper().head)}
                canRestore={!removed()}
                onRestore={() => restore(r)}
                onClose={() => show({ rev: null })}
              />
            )}
          </Show>
          {/* Hidden, not unmounted, so the editor keeps its place and its undo history. */}
          <div class={viewing() ? 'hidden' : ''}>
            <div class="oyl-card sticky top-3 mx-auto flex h-[calc(100dvh-7.5rem)] min-w-0 max-w-[58rem] flex-col overflow-hidden">
              <div class="flex h-10 shrink-0 items-center gap-2 border-b border-line px-4 text-[12px] text-muted">
                <FileText size={14} stroke-width={1.6} class="shrink-0" />
                <span class="min-w-0 truncate font-mono text-fg" title={path()}>
                  {path()}
                </span>
                <Show when={path() === main()}>
                  <span class="shrink-0 rounded-md bg-soft px-1.5 text-[11px]">Entry</span>
                </Show>
                <Show when={openedDoc() && !removed()}>
                  <span class="shrink-0 rounded-md bg-accent-soft px-1.5 text-[11px] text-fg" title="Written live: what is typed here shows in Oyl on your Mac as you type, and theirs here.">
                    Live
                  </span>
                </Show>
                <Show when={files().length > 1}>
                  <button type="button" class="ml-auto shrink-0 hover:text-fg" onClick={() => (setActive('files'), setShowPanel(true))}>
                    {files().length} files
                  </button>
                </Show>
              </div>
              <div class="min-h-0 flex-1 py-2 pr-2">{source}</div>
            </div>
          </div>
        </div>

        {/* Wide screens: the panel beside the source, as wide as it is dragged. */}
        <Show when={wide()}>
          <SidePanel
            open={showPanel()}
            width={panelWidth()}
            onWidth={(w) => setPanelWidth(clampWidth(w))}
            onResized={() => localStorage.setItem(PANEL_WIDTH_KEY, String(panelWidth()))}
            label="Beside the source"
          >
            {panel()}
          </SidePanel>
        </Show>
      </div>

      {/* Phones and tablets: the panel slides up over the source. */}
      <Show when={!wide() && showPanel()}>
        <div class="fixed inset-0 z-30 bg-[var(--overlay)]" onClick={() => setShowPanel(false)} />
        <aside
          class="fixed inset-x-0 bottom-0 z-40 flex h-[78dvh] flex-col overflow-hidden rounded-t-2xl border border-line-strong bg-raised shadow-raised backdrop-blur-xl"
          aria-label="Beside the source"
        >
          {panel()}
        </aside>
      </Show>
    </div>
  );
}

/**
 * A revision in place of the editor: the file as it was then, or what the
 * revision changed in it, with the button that brings the whole revision back.
 * Mount it per revision (a keyed Show).
 */
function RevisionView(props: {
  revision: RevisionRow;
  parent: RevisionRow | null;
  path: string;
  isCurrent: boolean;
  canRestore: boolean;
  onRestore: () => Promise<void>;
  onClose: () => void;
}) {
  const db = useDb<typeof schema>();
  const [mode, setMode] = createSignal<'source' | 'changes'>('changes');
  const [restoring, setRestoring] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const files = fileMap(props.revision.files);
  const before = props.parent ? fileMap(props.parent.files) : null;

  /** A file's text in a revision: null when it is not there, or not text. */
  const read = async (ref: { sha256: string; size: number } | undefined) =>
    ref && ref.size <= MAX_SHOWN_BYTES ? asText(await syncBlob(db, ref.sha256)) : null;
  const [texts] = createResource(
    () => props.path,
    async (p) => ({ now: await read(files[p]), was: before ? await read(before[p]) : null }),
  );

  onMount(() => {
    const key = (e: KeyboardEvent) => {
      if (e.key === 'Escape') props.onClose();
    };
    window.addEventListener('keydown', key);
    onCleanup(() => window.removeEventListener('keydown', key));
  });

  const restore = async () => {
    setRestoring(true);
    setError(null);
    try {
      await props.onRestore();
    } catch (err) {
      setError(errorText(err, 'This revision could not be brought back.'));
      setRestoring(false);
    }
  };

  const loaded = () => (texts.error || texts.loading ? undefined : texts());
  // The revision before is past the latest 100: what this one changed cannot be told.
  const unknownParent = Boolean(props.revision.parent) && !props.parent;
  const changes = createMemo(() => {
    const t = loaded();
    return t && mode() === 'changes' && !unknownParent ? textChanges(t.was ?? '', t.now ?? '') : [];
  });

  return (
    <div class="mx-auto max-w-[58rem] px-1 sm:px-4">
      <div class="sticky top-3 z-10 flex flex-wrap items-center gap-x-3 gap-y-2 oyl-glass rounded-xl px-3.5 py-2 shadow-raised">
        <History size={16} stroke-width={1.75} class="shrink-0 text-muted" />
        <div class="min-w-0 flex-1 basis-40">
          <p class="truncate text-[14px] font-medium">
            Revision {props.revision.n}
            <Show when={props.isCurrent}>
              <span class="ml-2 text-[12px] font-normal text-muted">Current</span>
            </Show>
          </p>
          <p class="truncate text-[12px] text-muted">
            {formatDate(props.revision.created_at, true)} · {authorLabel(props.revision.author)}
            <Show when={props.revision.message}>{(m) => <> · {m()}</>}</Show>
          </p>
        </div>
        <Segmented
          label="Show this revision as"
          value={mode()}
          onChange={setMode}
          options={[
            { value: 'changes', label: 'Changes' },
            { value: 'source', label: 'Source' },
          ]}
        />
        <Show when={!props.isCurrent && props.canRestore}>
          <Button size="sm" disabled={restoring()} onClick={() => void restore()} title="Make this revision's files the paper's again. Nothing is lost: what the paper holds now stays in the history.">
            {restoring() ? 'Restoring…' : 'Restore'}
          </Button>
        </Show>
        <IconButton label="Back to the current text (Esc)" size="sm" onClick={props.onClose}>
          <X size={16} stroke-width={1.75} />
        </IconButton>
      </div>

      <Show when={error() ?? texts.error}>
        <p role="alert" class="mt-3 rounded-xl bg-soft px-3.5 py-2 text-[13px] text-muted">{error() ?? errorText(texts.error, 'This revision could not be read.')}</p>
      </Show>
      <p class="mt-4 truncate px-1 font-mono text-[12px] text-muted">{props.path}</p>
      <Show when={loaded()} fallback={<Show when={!texts.error}><div class="mt-3 h-40 animate-pulse rounded-xl bg-soft" /></Show>}>
        {(t) => (
          <Show
            when={t().now !== null || t().was !== null}
            fallback={<p class="mt-3 px-1 text-[14px] text-muted">{files[props.path] ? 'This file is not text.' : `This file is not in revision ${props.revision.n}.`}</p>}
          >
            <Show
              when={mode() === 'changes'}
              fallback={
                <div class="oyl-card mt-3 h-[calc(100dvh-13rem)] min-w-0 overflow-hidden py-2 pr-2">
                  <Suspense fallback={<div class="m-4 h-40 animate-pulse rounded-xl bg-soft" />}>
                    <PaperEditor path={props.path} text={t().now ?? ''} label={`${props.path} in revision ${props.revision.n}`} />
                  </Suspense>
                </div>
              }
            >
              <div class="pb-24 pt-3">
                <Show when={!unknownParent} fallback={<p class="px-1 text-[14px] text-muted">The revision before this one is older than the latest 100, so what this one changed cannot be shown. Pick Source to read the file.</p>}>
                  <VersionChanges pieces={changes()} first={!props.revision.parent} />
                </Show>
              </div>
            </Show>
          </Show>
        )}
      </Show>
    </div>
  );
}

/** Stands where the source would be. */
function Placeholder(props: { children: JSX.Element }) {
  return <div class="flex h-full min-w-0 items-center justify-center px-6 text-center text-[14px] leading-relaxed text-muted [overflow-wrap:anywhere]">{props.children}</div>;
}

function TextButton(props: { onClick: () => void; children: JSX.Element }) {
  return (
    <button type="button" class="text-fg underline decoration-line-strong underline-offset-2 hover:decoration-fg" onClick={props.onClick}>
      {props.children}
    </button>
  );
}
