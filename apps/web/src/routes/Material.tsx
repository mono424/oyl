import { A, useNavigate, useParams, useSearchParams } from '@solidjs/router';
import { createEffect, createMemo, createSignal, lazy, Show, Suspense, type JSX } from 'solid-js';
import { useDb, useQuery } from '@spooky-sync/client-solid';
import { Download, NotebookPen, Pencil, ScrollText, Waypoints } from 'lucide-solid';
import type { schema } from '../schema.gen';
import { PROGRESS_JOB, reindexMaterial, saveLectureProgress } from '../lib/jobs';
import { chunkMarkdown, chunkPages, lectureMarkdown } from '../../../../shared/lecture';
import { idKey, idString, whenSynced } from '../lib/ids';
import { renderMarkdown } from '../lib/markdown';
import { firstRow, qMaterial, qMaterialJobs, qMaterials, qSheetHomework, qSourceDocument } from '../lib/query';
import { createHomework } from '../lib/homework';
import { createDocumentFromMaterial } from '../lib/documents';
import { useAuth } from '../lib/auth';
import { Button, buttonClass, inputClass, Loader, PageLoader, StatusText } from '../components/ui';
import { HeaderActions, SideActions } from '../components/HeaderSlot';
import { LecturePlace } from '../components/LecturePlace';
import { PdfViewer } from '../components/PdfViewer';

// CodeMirror is large: the LaTeX editor loads when a .tex material is edited.
const TexEditor = lazy(() => import('../components/TexEditor'));

export default function MaterialPage() {
  const params = useParams();
  const [search] = useSearchParams();
  const db = useDb<typeof schema>();
  const material = firstRow(useQuery(() => qMaterial(db, `material:${params.materialKey}`)));
  // The latest progress saves, to see whether the one just sent failed.
  const progressJobs = useQuery(() => qMaterialJobs(db, `material:${params.materialKey}`, PROGRESS_JOB));
  // A PDF shows the original; its text is derived from it, for the tutor and search.
  const isPdf = () => material.data()?.kind === 'pdf';
  // LaTeX source: shown rendered, edited as source.
  const isTex = () => material.data()?.kind === 'tex';
  // ?page=N from a citation link.
  const page = () => Number(Array.isArray(search.page) ? search.page[0] : search.page) || null;

  const [editing, setEditing] = createSignal(false);
  const [draft, setDraft] = createSignal('');
  const [title, setTitle] = createSignal('');
  const [saving, setSaving] = createSignal(false);
  const [progressThrough, setProgressThrough] = createSignal<number | null>(null);
  // The job saving the place, until it succeeds or fails.
  const [progressJob, setProgressJob] = createSignal<string | null>(null);
  const [progressError, setProgressError] = createSignal<string | null>(null);

  // The Markdown sections and search use: LaTeX is converted (shared/latex.ts).
  const text = createMemo(() => lectureMarkdown(material.data()?.kind ?? 'markdown', material.data()?.content ?? ''));
  const html = createMemo(() => renderMarkdown(text()));
  const sections = createMemo(() => chunkMarkdown(text()));
  const sectionPages = createMemo(() => chunkPages(text(), sections()));
  const progressVersion = createMemo(() => {
    const m = material.data();
    return m ? `${m.id}:${m.index_rev}:${m.learned_through}` : '';
  });
  createEffect(() => {
    progressVersion();
    setProgressThrough(null);
    setProgressError(null);
    setProgressJob(null);
  });
  const selectedThrough = () => progressThrough() ?? material.data()?.learned_through ?? -1;
  createEffect(() => {
    const id = progressJob();
    const job = id ? progressJobs.data()?.find((j) => idString(j.id) === id) : undefined;
    if (job?.status === 'success') setProgressJob(null);
    if (job?.status === 'failed') {
      setProgressJob(null);
      setProgressThrough(null);
      setProgressError('Progress could not be saved. Reload the lecture and try again.');
    }
  });

  // Shown until the saved place syncs back; a failed save falls back to it.
  const saveProgress = async (through: number) => {
    const m = material.data();
    if (!m) return;
    setProgressThrough(through);
    setProgressError(null);
    try {
      setProgressJob(await saveLectureProgress(db, m.id, { through, rev: m.index_rev }));
    } catch (err) {
      setProgressThrough(null);
      setProgressError(err instanceof Error ? err.message : 'Progress could not be saved. Try again.');
    }
  };

  const edit = () => {
    setDraft(material.data()?.content ?? '');
    setTitle(material.data()?.title ?? '');
    setEditing(true);
  };

  const save = async () => {
    const m = material.data();
    if (!m) return;
    setSaving(true);
    try {
      const contentChanged = m.kind !== 'pdf' && draft() !== m.content;
      await db.update('material', m.id, { title: title().trim() || m.title, ...(contentChanged ? { content: draft() } : {}) });
      setEditing(false);
      if (contentChanged) {
        await whenSynced(db);
        await reindexMaterial(db, m.id);
      }
    } finally {
      setSaving(false);
    }
  };

  const meta = () => {
    const m = material.data();
    if (!m) return '';
    return [m.source_name, m.page_count ? `${m.page_count} pages` : null, m.image_count ? `${m.image_count} images` : null]
      .filter(Boolean)
      .join(', ');
  };

  const place = (cls?: string): JSX.Element => (
    <Show when={material.data()?.status === 'ready' && sections().length > 0}>
      <LecturePlace
        class={cls}
        sections={sections()}
        pages={sectionPages()}
        value={selectedThrough()}
        saving={progressJob() !== null}
        disabled={sections().length !== material.data()?.chunk_count}
        error={progressError()}
        onChange={saveProgress}
      />
    </Show>
  );

  return (
    <Show when={material.data()} fallback={material.isLoading() ? <PageLoader /> : <div class="p-10 text-muted">Material not found.</div>}>
      {(m) => (
        <div class="min-h-full min-w-0">
          <HeaderActions>
            {/* Lined up with the lecture column below it (the editor is wider). */}
            <div class="header-column flex min-w-0 flex-1 items-center gap-2" style={{ '--page-column': editing() ? (isTex() ? '80rem' : '56rem') : isPdf() ? '64rem' : '48rem' }}>
              <span class="pointer-events-auto hidden min-w-0 truncate md:inline-flex">
                <StatusText status={m().status} detail={m().status_detail} />
              </span>
              <div class="pointer-events-auto ml-auto flex shrink-0 items-center gap-2">
                <Show when={!editing()}>
                  <A
                    href={`/courses/${params.courseKey}/graph?lecture=${params.materialKey}`}
                    class={`${buttonClass('soft', 'sm')} max-sm:w-11 max-sm:px-0`}
                    aria-label="Knowledge graph"
                    title="Knowledge graph of this lecture"
                  >
                    <Waypoints size={14} stroke-width={1.75} /> <span class="hidden sm:inline">Graph</span>
                  </A>
                  <AnswerButton material={m()} courseKey={params.courseKey ?? ''} />
                  <WriteButton material={m()} courseKey={params.courseKey ?? ''} />
                </Show>
                <Show
                  when={editing()}
                  fallback={
                    <Button variant="soft" size="sm" class="max-sm:w-11 max-sm:px-0" onClick={edit} aria-label={isPdf() ? 'Rename' : 'Edit'}>
                      <Pencil size={14} stroke-width={1.75} /> <span class="hidden sm:inline">{isPdf() ? 'Rename' : 'Edit'}</span>
                    </Button>
                  }
                >
                  <Button variant="ghost" size="sm" onClick={() => setEditing(false)}>
                    Cancel
                  </Button>
                  <Button size="sm" disabled={saving()} onClick={save}>
                    <Show when={saving()} fallback="Save">
                      <Loader size={4} label="Saving" class="text-solid-fg" />
                    </Show>
                  </Button>
                </Show>
              </div>
            </div>
          </HeaderActions>
          {/* Wide windows keep the reading place in the recent sessions column; narrower ones under the title. */}
          <Show when={!editing()}>
            <SideActions>{place()}</SideActions>
          </Show>

          <Show
            when={editing()}
            fallback={
              <article class={`mx-auto w-full min-w-0 px-4 pb-12 pt-6 sm:px-8 sm:pt-5 ${isPdf() ? 'max-w-5xl' : 'max-w-3xl'}`}>
                <h1 class="break-words text-[22px] font-semibold leading-tight tracking-tight sm:text-[30px]">{m().title}</h1>
                <Show when={meta() || m().source_url || isTex()}>
                  <p class="mt-1.5 break-words text-[14px] text-muted">
                    {meta()}
                    <Show when={isTex()}>
                      {meta() ? ' · ' : ''}
                      <button type="button" class="inline-flex items-center gap-1 underline decoration-line-strong underline-offset-2 hover:text-fg" onClick={() => downloadTex(m().title, m().content)}>
                        <Download size={13} stroke-width={1.75} /> Download .tex
                      </button>
                    </Show>
                    <Show when={m().source_url}>
                      {(url) => (
                        <>
                          {meta() || isTex() ? ' · ' : ''}
                          <a href={url()} target="_blank" rel="noopener noreferrer" class="underline decoration-line-strong underline-offset-2 hover:text-fg">
                            Open the original
                          </a>
                        </>
                      )}
                    </Show>
                  </p>
                </Show>
                <div class="mt-2 md:hidden">
                  <StatusText status={m().status} detail={m().status_detail} />
                </div>
                {place('mt-4 max-w-sm xl:hidden')}
                <div class="mt-8">
                  <Show
                    when={isPdf()}
                    fallback={
                      <Show
                        when={m().content.trim()}
                        fallback={
                          <p class="break-words text-muted">
                            {m().status === 'failed' ? `Processing failed: ${m().error ?? 'unknown error'}. Upload the file again to retry.` : ''}
                          </p>
                        }
                      >
                        <div class="lecture" innerHTML={html()} />
                      </Show>
                    }
                  >
                    <Show when={m().status === 'failed'}>
                      <p class="mb-4 break-words text-[14px] text-muted" role="alert">
                        {`Processing failed: ${m().error ?? 'unknown error'}. Upload the file again as new material to retry.`}
                      </p>
                    </Show>
                    {/* The PDF is stored before it is converted, so it can be read right away. */}
                    <Show
                      when={m().source_size}
                      fallback={
                        <Show when={m().status !== 'failed'}>
                          <div class="flex justify-center py-10">
                            <Loader size={7} pattern="upload" label="Uploading the PDF" />
                          </div>
                        </Show>
                      }
                    >
                      <PdfViewer materialId={m().id} size={m().source_size} fileName={m().source_name} page={page()} />
                    </Show>
                  </Show>
                </div>
              </article>
            }
          >
            <div class={`mx-auto w-full min-w-0 space-y-3 px-4 py-6 sm:px-8 sm:pb-8 sm:pt-5 ${isTex() ? 'max-w-7xl' : 'max-w-4xl'}`}>
              <input class={`${inputClass} text-[18px] font-semibold`} value={title()} onInput={(e) => setTitle(e.currentTarget.value)} aria-label="Title" />
              <Show
                when={!isPdf()}
                fallback={<p class="text-[13px] text-muted">The PDF is kept as uploaded. Its text comes from the file, so only the title can change.</p>}
              >
                <Show
                  when={isTex()}
                  fallback={
                    <>
                      <textarea
                        class={`${inputClass} min-h-[65dvh] resize-y font-mono text-[16px] leading-relaxed sm:min-h-[70vh] sm:text-[13px]`}
                        value={draft()}
                        onInput={(e) => setDraft(e.currentTarget.value)}
                        aria-label="Content (Markdown)"
                      />
                      <p class="text-[13px] text-muted">
                        Markdown. Saving rebuilds the search index the tutor uses. Images look like ![caption](asset:…); keep those lines to keep the image.
                      </p>
                    </>
                  }
                >
                  <Suspense fallback={<div class="flex justify-center py-16"><Loader size={6} label="Opening the editor" /></div>}>
                    <TexEditor value={draft()} onChange={setDraft} />
                  </Suspense>
                  <p class="text-[13px] text-muted">
                    LaTeX. The preview shows what the tutor reads. Saving rebuilds the search index; \newcommand macros work in the math.
                  </p>
                </Show>
              </Show>
            </div>
          </Show>
        </div>
      )}
    </Show>
  );
}

/** Saves a LaTeX material's source as a .tex file. */
function downloadTex(title: string, content: string) {
  const url = URL.createObjectURL(new Blob([content], { type: 'text/x-tex' }));
  const a = document.createElement('a');
  a.href = url;
  a.download = `${title.replace(/[\\/:*?"<>|]+/g, ' ').trim() || 'document'}.tex`;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/**
 * Opens the document started from this material (a draft, a proposal), making
 * it the first time: the text in the editor, the papers it mentions beside it.
 */
function WriteButton(props: { material: { id: unknown; course: unknown; title: string; kind: string; content: string }; courseKey: string }) {
  const db = useDb<typeof schema>();
  const auth = useAuth();
  const navigate = useNavigate();
  const draft = firstRow(useQuery(() => qSourceDocument(db, idString(props.material.id))));
  const materials = useQuery(() => qMaterials(db, idString(props.material.course)));
  const [busy, setBusy] = createSignal(false);
  const open = async () => {
    const existing = draft.data();
    if (existing) return navigate(`/courses/${props.courseKey}/documents/${idKey(existing.id)}`);
    const owner = auth.userId();
    if (!owner || busy()) return;
    setBusy(true);
    try {
      const id = await createDocumentFromMaterial(db, {
        owner,
        material: { id: idString(props.material.id), course: idString(props.material.course), title: props.material.title, kind: props.material.kind, content: props.material.content },
        materials: (materials.data() ?? []).map((m) => ({ id: idString(m.id), title: m.title })),
      });
      navigate(`/courses/${props.courseKey}/documents/${idKey(id)}`);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Button variant="soft" size="sm" class="max-sm:w-11 max-sm:px-0" onClick={open} disabled={busy()} title="Write it as a document, with the papers it mentions beside it">
      <ScrollText size={14} stroke-width={1.75} /> <span class="hidden sm:inline">{draft.data() ? 'My document' : 'Write'}</span>
    </Button>
  );
}

/** Opens the homework answering this sheet, starting it the first time. */
function AnswerButton(props: { material: { id: unknown; course: unknown; title: string; kind: string; content: string }; courseKey: string }) {
  const db = useDb<typeof schema>();
  const auth = useAuth();
  const navigate = useNavigate();
  const homework = firstRow(useQuery(() => qSheetHomework(db, idString(props.material.id))));
  const [busy, setBusy] = createSignal(false);
  const open = async () => {
    const existing = homework.data();
    if (existing) return navigate(`/courses/${props.courseKey}/homework/${idKey(existing.id)}`);
    const owner = auth.userId();
    if (!owner || busy()) return;
    setBusy(true);
    try {
      const id = await createHomework(db, {
        owner,
        course: idString(props.material.course),
        sheet: { id: idString(props.material.id), title: props.material.title, kind: props.material.kind, content: props.material.content },
      });
      navigate(`/courses/${props.courseKey}/homework/${idKey(id)}`);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Button variant="soft" size="sm" class="max-sm:w-11 max-sm:px-0" onClick={open} disabled={busy()} title="Write your answers to this sheet">
      <NotebookPen size={14} stroke-width={1.75} /> <span class="hidden sm:inline">{homework.data() ? 'My answers' : 'Answer'}</span>
    </Button>
  );
}
