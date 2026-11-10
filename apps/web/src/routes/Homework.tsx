import { A, useNavigate, useParams } from '@solidjs/router';
import { createEffect, createMemo, createSignal, For, lazy, on, onCleanup, onMount, Show, Suspense, type JSX } from 'solid-js';
import { Portal } from 'solid-js/web';
import { type GetTable, type TableModel, useDb, useQuery } from '@spooky-sync/client-solid';
import { ExternalLink, FileDown, ImagePlus, Trash2, X } from 'lucide-solid';
import type { PDFDocumentProxy } from 'pdfjs-dist';
import type { schema } from '../schema.gen';
import { fetchPdf, materialPath, uploadAnswerImage } from '../lib/api';
import { idKey, idString } from '../lib/ids';
import { renderMarkdown } from '../lib/markdown';
import { useAuth } from '../lib/auth';
import { createDrafts } from '../lib/drafts';
import { exportPdf } from '../lib/print';
import { loadPdfjs } from '../lib/pdfjs';
import { firstRow, qAnswers, qHomework, qMaterial } from '../lib/query';
import { locateParts, parseRegion } from '../lib/sheet-regions';
import { Button, IconButton, PageLoader, Segmented } from '../components/ui';
import { Focus, HeaderEndActions, HeaderTitle } from '../components/HeaderSlot';
import { CourseCrumb } from '../components/CourseCrumb';
import { SidePanel } from '../components/SidePanel';
import { PdfViewer } from '../components/PdfViewer';
import { SheetExcerpt } from '../components/SheetExcerpt';
import type { EditorPart } from '../components/Editor';

const Editor = lazy(() => import('../components/Editor'));

/**
 * Doing homework: every answer of an exercise sheet in one document, part by
 * part in sheet order, with the whole window (no sidebars). It is one editor
 * (components/Editor.tsx), a quiet label in the margin saying which
 * part each answer is for. Beside the answers, in a panel as wide as you drag
 * it, the exercise of the part being written: its page with the part marked
 * and the rest dimmed, or the entire sheet. On a phone it is pinned above the
 * keyboard. Answers are Markdown with $math$ and images (pasted, dropped in,
 * or picked with the Image button: a photo of a drawing, a sketch), rendered
 * as you write, saved as you type, and print as a PDF.
 */

type AnswerRow = TableModel<GetTable<typeof schema, 'answer'>>;
type MaterialRow = TableModel<GetTable<typeof schema, 'material'>>;

const PANEL_WIDTH_KEY = 'homework-panel-width';

export default function HomeworkPage() {
  const params = useParams();
  const db = useDb<typeof schema>();
  const homeworkId = () => `homework:${params.homeworkKey}`;
  const homework = firstRow(useQuery(() => qHomework(db, homeworkId())));
  const answers = useQuery(() => qAnswers(db, homeworkId()));

  return (
    <Show when={homework.data()} fallback={homework.isLoading() ? <PageLoader /> : <div class="p-10 text-muted">Homework not found.</div>}>
      {(hw) => (
        // The sheet's live query mounts once its id is known, never switching in place.
        <Show
          when={hw().sheet ? idString(hw().sheet) : null}
          keyed
          fallback={<Workspace id={hw().id} title={hw().title} answers={answers.data() ?? []} sheet={null} courseKey={params.courseKey ?? ''} />}
        >
          {(sheetId) => <WithSheet id={hw().id} sheetId={sheetId} title={hw().title} answers={answers.data() ?? []} courseKey={params.courseKey ?? ''} />}
        </Show>
      )}
    </Show>
  );
}

function WithSheet(props: { id: string; sheetId: string; title: string; answers: AnswerRow[]; courseKey: string }) {
  const db = useDb<typeof schema>();
  const sheet = firstRow(useQuery(() => qMaterial(db, props.sheetId)));
  return <Workspace id={props.id} title={props.title} answers={props.answers} sheet={sheet.data() ?? null} courseKey={props.courseKey} />;
}

function useMedia(query: string) {
  const list = matchMedia(query);
  const [matches, setMatches] = createSignal(list.matches);
  const update = () => setMatches(list.matches);
  list.addEventListener('change', update);
  onCleanup(() => list.removeEventListener('change', update));
  return matches;
}

/** The part of the page the keyboard leaves visible (phones). */
function useVisualViewport() {
  const vv = window.visualViewport;
  const [state, setState] = createSignal({ top: vv?.offsetTop ?? 0, height: vv?.height ?? window.innerHeight });
  if (vv) {
    const update = () => setState({ top: vv.offsetTop, height: vv.height });
    vv.addEventListener('resize', update);
    vv.addEventListener('scroll', update);
    onCleanup(() => {
      vv.removeEventListener('resize', update);
      vv.removeEventListener('scroll', update);
    });
  }
  return state;
}

function Workspace(props: { id: string; title: string; answers: AnswerRow[]; sheet: MaterialRow | null; courseKey: string }) {
  const db = useDb<typeof schema>();
  const navigate = useNavigate();
  const wide = useMedia('(min-width: 1024px)');
  const viewport = useVisualViewport();

  // ── The sheet's PDF, for the excerpts ──────────────────────────────────

  const [pdf, setPdf] = createSignal<PDFDocumentProxy | null>(null);
  createEffect(
    on(
      () => (props.sheet?.kind === 'pdf' && props.sheet.source_size ? idString(props.sheet.id) : null),
      (id) => {
        setPdf(null);
        if (!id) return;
        let cancelled = false;
        let loading: { destroy(): Promise<void> } | null = null;
        void (async () => {
          try {
            const [lib, buf] = await Promise.all([loadPdfjs(), fetchPdf(db, id, props.sheet?.source_size)]);
            if (cancelled) return;
            const task = lib.getDocument({ data: new Uint8Array(buf.slice(0)) });
            loading = task;
            const doc = await task.promise;
            if (!cancelled) setPdf(doc);
          } catch {
            // Without the PDF the excerpts show the exercise's text.
          }
        })();
        onCleanup(() => {
          cancelled = true;
          void loading?.destroy();
        });
      },
    ),
  );

  // Find each part on the sheet once, and keep it with the answer.
  let located = false;
  createEffect(() => {
    const doc = pdf();
    const rows = props.answers;
    if (!doc || located || !rows.length || rows.every((a) => a.region !== undefined && a.region !== null)) return;
    located = true;
    void locateParts(doc, rows).then((regions) =>
      Promise.all(rows.map((a, i) => (a.region ? null : db.update('answer', a.id, { region: JSON.stringify(regions[i]) })))),
    );
  });

  // ── Editing ────────────────────────────────────────────────────────────

  const [active, setActive] = createSignal<string | null>(null);
  const [pending, setPending] = createSignal(0);
  const [notice, setNotice] = createSignal<string | null>(null);

  onMount(() => {
    setPending(db.pendingMutationCount);
    onCleanup(db.subscribeToPendingMutations((n) => setPending(n)));
  });

  const { text, textOf, edit, saveAll, unsaved, rejected } = createDrafts({
    db,
    rows: () => props.answers,
    save: (id, content) => db.update('answer', id, { content }),
  });

  const remove = async () => {
    if (!confirm(`Delete your answers to “${props.title}”? The exercise sheet stays in the course.`)) return;
    await db.delete('homework', props.id);
    navigate(`/courses/${props.courseKey}?tab=homework`);
  };

  // The editor shows an image where it goes while it uploads; only a failure is said here.
  const uploadImage = async (id: string, file: File) => {
    try {
      const { id: image } = await uploadAnswerImage(db, id, file);
      setNotice(null);
      return `![](answer-image:${idKey(image)})`;
    } catch (err) {
      setNotice(err instanceof Error ? err.message : 'The image could not be added.');
      return null;
    }
  };
  let addImages: ((files: File[]) => void) | undefined;
  let picker!: HTMLInputElement;

  // The editor is built once per set of parts; their texts flow in through `textOf`.
  const parts = createMemo<EditorPart[]>(
    () =>
      props.answers.map((a) => ({
        id: idString(a.id),
        question: a.question,
        questionTitle: a.question_title ?? '',
        label: a.label,
      })),
    [],
    { equals: (a, b) => JSON.stringify(a) === JSON.stringify(b) },
  );

  // ── The exercise beside (or above) the active answer ───────────────────

  const [showExercise, setShowExercise] = createSignal(true);
  // exercise: the part's page, the part marked. sheet: the entire sheet.
  const [mode, setMode] = createSignal<'exercise' | 'sheet'>('exercise');
  const current = createMemo(() => props.answers.find((a) => idString(a.id) === active()) ?? props.answers[0] ?? null);
  const hasPdf = () => props.sheet?.kind === 'pdf' && !!props.sheet.source_size;

  onMount(() => {
    const key = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'e') {
        e.preventDefault();
        setShowExercise((v) => !v);
      }
    };
    window.addEventListener('keydown', key);
    onCleanup(() => window.removeEventListener('keydown', key));
  });

  // Wide screens: the panel's width, dragged at its edge and remembered.
  const [panelWidth, setPanelWidth] = createSignal(
    Number(localStorage.getItem(PANEL_WIDTH_KEY)) || Math.round(Math.min(820, Math.max(440, window.innerWidth * 0.46))),
  );
  const clampWidth = (w: number) => Math.round(Math.min(window.innerWidth * 0.7, Math.max(360, w)));

  // Phones: how much room the pinned exercise takes (drag its handle).
  const [pinnedHeight, setPinnedHeight] = createSignal(240);
  const [folded, setFolded] = createSignal(false);
  let drag: { y: number; h: number; moved: boolean } | null = null;

  const answered = () => props.answers.filter((a) => text(a).trim()).length;
  const partName = (a: AnswerRow) => [a.question, a.label].filter(Boolean).join(' ') || 'Answer';
  const pageOf = (a: AnswerRow) => parseRegion(a.region)[0]?.page ?? a.page ?? a.anchor_page ?? 1;

  const panelHeader = (a: AnswerRow, compact: boolean): JSX.Element => (
    <div class="flex shrink-0 items-center gap-2 border-b border-line px-3 py-2 text-[12px] text-muted">
      <Show when={!compact && hasPdf()}>
        <Segmented
          label="Show"
          value={mode()}
          options={[
            { value: 'exercise', label: 'Exercise' },
            { value: 'sheet', label: 'Whole sheet' },
          ]}
          onChange={setMode}
        />
      </Show>
      <span class="min-w-0 flex-1 truncate">
        {partName(a)}
        {a.points ? ` · ${a.points} ${a.points === 1 ? 'point' : 'points'}` : ''}
        {hasPdf() ? ` · page ${pageOf(a)}` : ''}
      </span>
      <Show when={compact && props.sheet}>
        {(sheet) => (
          <A href={materialPath(idString(sheet().course), idString(sheet().id), pageOf(a))} class="inline-flex h-7 items-center gap-1 rounded-full px-2 hover:bg-soft hover:text-fg" title="Open the sheet">
            <ExternalLink size={13} stroke-width={1.9} />
          </A>
        )}
      </Show>
      <button type="button" class="-mr-1 inline-flex h-7 w-7 items-center justify-center rounded-full hover:bg-soft hover:text-fg" aria-label="Hide the exercise (⌘E)" onClick={() => setShowExercise(false)}>
        <X size={14} stroke-width={1.9} />
      </button>
    </div>
  );

  const excerpt = (a: AnswerRow) => (
    <SheetExcerpt pdf={pdf()} segments={parseRegion(a.region)} prompt={a.prompt} context={a.context} />
  );

  return (
    <div class="w-full px-3 sm:px-6">
      <Focus />
      <HeaderTitle>
        <div class="flex min-w-0 items-center gap-2.5">
          <CourseCrumb courseKey={props.courseKey} tab="homework" />
          <span class="text-faint">/</span>
          <h1 class="min-w-0 truncate text-[16px] font-semibold tracking-tight sm:text-[17px]">{props.title}</h1>
          <span class="hidden shrink-0 text-[12px] text-muted sm:inline">
            {rejected() ? 'Not saved yet. Type anything to try again.' : pending() > 0 || unsaved() ? 'Saving…' : `${answered()} of ${props.answers.length} answered · saved`}
          </span>
        </div>
      </HeaderTitle>
      <HeaderEndActions>
        <Button variant={showExercise() ? 'solid' : 'ghost'} size="sm" onClick={() => setShowExercise((v) => !v)} title="Show the exercise (⌘E)">
          Exercise <span class="hidden opacity-60 lg:inline">⌘E</span>
        </Button>
        <Show when={props.answers.length}>
          <IconButton label="Add an image to the answer (or drop one in)" size="sm" onClick={() => picker.click()}>
            <ImagePlus size={16} stroke-width={1.75} />
          </IconButton>
        </Show>
        <Button variant="ghost" size="sm" onClick={() => void exportPdf(props.title)} title="Print or save as PDF">
          <FileDown size={16} stroke-width={1.75} /> <span class="hidden sm:inline">Export PDF</span>
        </Button>
        <IconButton label="Delete the homework" size="sm" onClick={() => void remove()}>
          <Trash2 size={16} stroke-width={1.75} />
        </IconButton>
      </HeaderEndActions>

      {/* Phones: the exercise pinned at the top of what the keyboard leaves visible. */}
      <Show when={!wide() && showExercise() && current()}>
        {(a) => (
          <>
            <div
              class={active() ? 'fixed inset-x-0 z-30 px-2 pt-2' : 'pt-2'}
              style={active() ? { top: `${viewport().top}px` } : undefined}
            >
              <div class="overflow-hidden rounded-xl border border-line bg-surface shadow-[0_10px_30px_-18px_rgb(0_0_0/0.3)]">
                {panelHeader(a(), true)}
                <Show when={!folded()}>
                  <div class="overflow-y-auto overscroll-contain p-2" style={{ 'max-height': `${pinnedHeight()}px` }}>
                    {excerpt(a())}
                  </div>
                </Show>
              </div>
              <div
                class="flex touch-none justify-center py-1.5"
                onPointerDown={(e) => {
                  drag = { y: e.clientY, h: folded() ? 0 : pinnedHeight(), moved: false };
                  e.currentTarget.setPointerCapture(e.pointerId);
                }}
                onPointerMove={(e) => {
                  if (!drag) return;
                  const dy = e.clientY - drag.y;
                  if (Math.abs(dy) > 4) drag.moved = true;
                  if (!drag.moved) return;
                  const h = drag.h + dy;
                  setFolded(h < 50);
                  setPinnedHeight(Math.min(viewport().height * 0.65, Math.max(80, h)));
                }}
                onPointerUp={() => {
                  // A tap folds or unfolds; a drag resizes.
                  if (drag && !drag.moved) setFolded((f) => !f);
                  drag = null;
                }}
              >
                <span class="h-1 w-9 rounded-full bg-line-strong" />
              </div>
            </div>
            <Show when={active()}>
              <div aria-hidden="true" style={{ height: `${(folded() ? 40 : pinnedHeight() + 56) + 12}px` }} />
            </Show>
          </>
        )}
      </Show>

      <Show when={notice()}>
        <p class="mx-auto mt-3 max-w-3xl rounded-xl bg-soft px-3.5 py-2 text-[13px] text-muted">{notice()}</p>
      </Show>
      <input
        ref={picker}
        type="file"
        class="sr-only"
        tabindex="-1"
        multiple
        accept="image/*,.heic,.heif"
        onChange={(e) => {
          if (e.currentTarget.files?.length) addImages?.([...e.currentTarget.files]);
          e.currentTarget.value = '';
        }}
      />

      <div class="relative mt-4 flex items-start gap-0">
        <div class="relative min-w-0 flex-1 pb-24">
          <div class="mx-auto max-w-3xl">
            <Show when={props.answers.length} fallback={<p class="py-10 text-center text-muted">This sheet has no parts to answer.</p>}>
              <Suspense fallback={<div class="mx-12 h-40 animate-pulse rounded-xl bg-soft sm:mx-16" />}>
                <Show when={parts()} keyed>
                  {(list) => (
                    <Editor
                      parts={list}
                      content={textOf}
                      onInput={edit}
                      onActive={setActive}
                      onBlur={saveAll}
                      uploadImage={uploadImage}
                      onAddImages={(add) => (addImages = add)}
                      topInset={() => (!wide() && showExercise() && active() ? (folded() ? 40 : pinnedHeight() + 56) + 12 : 0)}
                    />
                  )}
                </Show>
              </Suspense>
            </Show>
          </div>
        </div>

        {/* Wide screens: the panel beside the answers, as wide as it is dragged. */}
        <Show when={wide()}>
          <SidePanel
            open={showExercise() && !!current()}
            width={panelWidth()}
            onWidth={(w) => setPanelWidth(clampWidth(w))}
            onResized={() => localStorage.setItem(PANEL_WIDTH_KEY, String(panelWidth()))}
            label="Exercise"
          >
            <Show when={current()}>
              {(a) => (
                <>
                  {panelHeader(a(), false)}
                  <div class="scroll-quiet min-h-0 flex-1 overflow-y-auto overscroll-contain p-3">
                    <Show
                      when={mode() === 'sheet' && hasPdf() && props.sheet}
                      fallback={excerpt(a())}
                    >
                      {(sheet) => <PdfViewer materialId={idString(sheet().id)} size={sheet().source_size} fileName={sheet().source_name} page={pageOf(a())} />}
                    </Show>
                  </div>
                </>
              )}
            </Show>
          </SidePanel>
        </Show>
      </div>

      <Portal ref={(el) => el.classList.add('homework-print')}>
        <PrintView title={props.title} answers={props.answers} text={text} />
      </Portal>
    </div>
  );
}

/**
 * What Export PDF prints: the answers in sheet order, under their part
 * labels, set like a LaTeX article (Computer Modern, which KaTeX ships).
 */
function PrintView(props: { title: string; answers: AnswerRow[]; text: (a: AnswerRow) => string }) {
  const auth = useAuth();
  const today = new Date().toLocaleDateString('en-US', { year: 'numeric', month: 'long', day: 'numeric' });
  return (
    <article class="homework-print-body">
      <header class="title-block">
        <h1>{props.title}</h1>
        <Show when={auth.user()?.name}>{(name) => <p class="author">{name()}</p>}</Show>
        <p class="date">{today}</p>
      </header>
      <For each={props.answers}>
        {(a, i) => (
          <>
            <Show when={i() === 0 || props.answers[i() - 1]?.question !== a.question}>
              <h2>
                <span class="number">Question {a.question}</span>
                {a.question_title ? <span class="title">{a.question_title}</span> : null}
              </h2>
            </Show>
            <section>
              <Show when={a.label}>
                <h3>{printLabel(a.label)}</h3>
              </Show>
              <Show when={props.text(a).trim()} fallback={<p class="unanswered">No answer.</p>}>
                <div class="answer" innerHTML={renderMarkdown(props.text(a))} />
              </Show>
            </section>
          </>
        )}
      </For>
    </article>
  );
}

/** A part's label as a printed sheet writes it: "b (ii)" becomes "(b) (ii)". */
const printLabel = (label: string) => label.split(' ').map((t) => (/^[a-z]{1,4}$/.test(t) ? `(${t})` : t)).join(' ');
