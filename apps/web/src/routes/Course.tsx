import { A, useNavigate, useParams, useSearchParams } from '@solidjs/router';
import { createEffect, createMemo, createSignal, For, type JSX, Match, onCleanup, Show, Switch } from 'solid-js';
import { type GetTable, type TableModel, useDb, useQuery } from '@spooky-sync/client-solid';
import { ArrowDownUp, FileCode2, FileText, FileType2, Folder, FolderUp, Globe, NotebookPen, PenLine, Plus, Radar, ScrollText, Settings2, Trash2, Upload, Waypoints } from 'lucide-solid';
import type { schema } from '../schema.gen';
import { useAuth } from '../lib/auth';
import { addPdfMaterial, createDocument } from '../lib/documents';
import { reindexMaterial } from '../lib/jobs';
import { idKey, newId, whenSynced } from '../lib/ids';
import { COURSE_COLORS, courseColor, LANGUAGES, type CourseColor } from '../lib/palette';
import { qDocumentList, qHomeworkList, qMaterials, qMaterialsPage, qSiteList, useCourse } from '../lib/query';
import { type SiteBundle, siteFromDrop, siteFromFiles, uploadSite } from '../lib/sites';
import { Button, buttonClass, Field, formatDate, IconButton, inputClass, PageLoader, StatusText } from '../components/ui';
import { type ListWindow, scrollParent, VirtualList } from '../components/VirtualList';
import { CourseSearch } from '../components/CourseSearch';
import { Sheet } from '../components/Sheet';
import { HeaderEndActions, HeaderTitle } from '../components/HeaderSlot';
import { AddSourceSheet, SourceSheet, SourcesSection } from '../components/Sources';

/** The course page's tabs, kept in the address (`?tab=`) so Back returns to the same one. */
const TABS = [
  { value: 'material', label: 'Material' },
  { value: 'homework', label: 'Homework' },
  { value: 'documents', label: 'Documents' },
  { value: 'sites', label: 'Sites' },
  { value: 'sources', label: 'Sources' },
] as const;
type Tab = (typeof TABS)[number]['value'];
type SortedTab = Exclude<Tab, 'sources'>;

/** By date added, or by name. */
type Sort = 'newest' | 'oldest' | 'name';
const SORTS: { value: Sort; label: string }[] = [
  { value: 'newest', label: 'Newest first' },
  { value: 'oldest', label: 'Oldest first' },
  { value: 'name', label: 'Name' },
];
const SORT_KEY = 'course-list-sort';
const DEFAULT_SORTS: Record<SortedTab, Sort> = { material: 'oldest', homework: 'newest', documents: 'newest', sites: 'newest' };

/** Each tab's order, as the student last picked it on this device. */
function loadSorts(): Record<SortedTab, Sort> {
  const sorts = { ...DEFAULT_SORTS };
  try {
    const saved = JSON.parse(localStorage.getItem(SORT_KEY) ?? 'null') as Partial<Record<SortedTab, unknown>> | null;
    for (const tab of Object.keys(sorts) as SortedTab[]) {
      const v = saved?.[tab];
      if (SORTS.some((s) => s.value === v)) sorts[tab] = v as Sort;
    }
  } catch {
    // An unreadable value keeps the defaults.
  }
  return sorts;
}

/** Names in reading order: "Lecture 2" before "Lecture 10", capitals aside. */
const names = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });
const timeOf = (v: unknown) => (v instanceof Date ? v : new Date(String(v))).getTime() || 0;

function sortRows<T extends { id: unknown; title: string; created_at: unknown }>(rows: readonly T[], sort: Sort): T[] {
  const tie = (a: T, b: T) => String(a.id).localeCompare(String(b.id));
  return [...rows].sort((a, b) => {
    if (sort === 'name') return names.compare(a.title, b.title) || tie(a, b);
    const d = timeOf(a.created_at) - timeOf(b.created_at);
    return (sort === 'newest' ? -d : d) || tie(a, b);
  });
}

export default function CoursePage() {
  const params = useParams();
  const db = useDb<typeof schema>();
  const auth = useAuth();
  const navigate = useNavigate();
  const courseId = () => `course:${params.courseKey}`;

  const course = useCourse(db, auth.userId, courseId);

  const [search, setSearch] = useSearchParams();
  const tab = (): Tab => {
    const v = Array.isArray(search.tab) ? search.tab[0] : search.tab;
    return TABS.find((t) => t.value === v)?.value ?? 'material';
  };
  const showTab = (t: Tab) => setSearch({ tab: t === 'material' ? undefined : t }, { replace: true });

  const [sorts, setSorts] = createSignal(loadSorts());
  const setSort = (t: SortedTab, sort: Sort) => {
    setSorts({ ...sorts(), [t]: sort });
    localStorage.setItem(SORT_KEY, JSON.stringify(sorts()));
  };

  const [writing, setWriting] = createSignal(false);
  const [addingSource, setAddingSource] = createSignal(false);
  const [openSource, setOpenSource] = createSignal<FetcherRow | null>(null);
  const [dragging, setDragging] = createSignal(false);
  const [notice, setNotice] = createSignal<string | null>(null);
  const [siteProgress, setSiteProgress] = createSignal<string | null>(null);
  const [siteNotice, setSiteNotice] = createSignal<string | null>(null);

  // ── Header: the course's icon and name sit in the top bar where the logo
  // was, its actions next to the account menu. The search sticks under it.

  const [page, setPage] = createSignal<HTMLElement>();
  const [scroller, setScroller] = createSignal<HTMLElement | null>(null);
  const [scrolled, setScrolled] = createSignal(false);

  createEffect(() => {
    const el = page() && scrollParent(page()!);
    setScroller(el ?? null);
    if (!el) return;
    const onScroll = () => setScrolled(el.scrollTop > 0);
    onScroll();
    el.addEventListener('scroll', onScroll, { passive: true });
    onCleanup(() => el.removeEventListener('scroll', onScroll));
  });

  const write = () => {
    showTab('material');
    setWriting(true);
    scroller()?.scrollTo({ top: 0, behavior: 'smooth' });
  };

  // ── Adding material ────────────────────────────────────────────────────

  async function addFiles(files: FileList | File[]) {
    const uid = auth.userId();
    if (!uid) return;
    setNotice(null);
    // New material shows in its own tab.
    showTab('material');
    for (const file of Array.from(files)) {
      const name = file.name.replace(/\.(pdf|md|markdown|txt|tex)$/i, '');
      if (/\.pdf$/i.test(file.name) || file.type === 'application/pdf') {
        const { uploaded } = await addPdfMaterial(db, { owner: uid, course: courseId(), file });
        uploaded.catch((err) => setNotice(`${file.name}: ${err instanceof Error ? err.message : 'upload failed'}`));
      } else if (/\.tex$/i.test(file.name) || file.type === 'text/x-tex' || file.type === 'application/x-tex') {
        await addText(name, await file.text(), 'tex');
      } else if (/\.(md|markdown|txt)$/i.test(file.name) || file.type.startsWith('text/')) {
        await addText(name, await file.text(), /\.txt$/i.test(file.name) ? 'text' : 'markdown');
      } else {
        setNotice(`${file.name} was skipped: add PDFs, LaTeX, Markdown or text files.`);
      }
    }
  }

  async function addText(title: string, content: string, kind: 'text' | 'markdown' | 'tex') {
    const uid = auth.userId();
    if (!uid || !content.trim()) return;
    const id = newId('material');
    await db.create(id, { owner: uid, course: courseId(), title: title || 'Notes', kind, content, status: 'pending' });
    await whenSynced(db);
    await reindexMaterial(db, id).catch((err) => setNotice(`${title}: ${err instanceof Error ? err.message : 'indexing failed'}`));
  }

  /** Uploads a site read from a pick or a drop; one at a time. */
  async function addSite(reading: Promise<SiteBundle>) {
    const uid = auth.userId();
    if (!uid || siteProgress()) return;
    setSiteNotice(null);
    setSiteProgress('Reading the site');
    try {
      const bundle = await reading;
      await uploadSite(db, { owner: uid, course: courseId(), bundle, onProgress: (done, total) => setSiteProgress(`Uploading ${done} of ${total} files`) });
    } catch (err) {
      setSiteNotice(err instanceof Error ? err.message : 'The upload failed.');
    } finally {
      setSiteProgress(null);
    }
  }

  const newDocument = async (format: 'markdown' | 'tex') => {
    const uid = auth.userId();
    if (!uid) return;
    const id = await createDocument(db, { owner: uid, course: courseId(), format });
    navigate(`/courses/${params.courseKey}/documents/${idKey(id)}`);
  };

  const removeMaterial = async (id: string, title: string) => {
    if (confirm(`Delete “${title}”? Its images and search index go with it.`)) await db.delete('material', id);
  };

  return (
    <Show when={course.data()} fallback={course.isLoading() ? <PageLoader /> : <div class="p-10 text-muted">Course not found.</div>}>
      {(c) => (
        <div
          ref={setPage}
          class="mx-auto w-full max-w-3xl px-4 sm:px-8"
          onDragOver={(e) => {
            e.preventDefault();
            setDragging(true);
          }}
          onDragLeave={(e) => e.currentTarget === e.target && setDragging(false)}
          onDrop={(e) => {
            e.preventDefault();
            setDragging(false);
            if (!e.dataTransfer?.files.length) return;
            if (tab() === 'sites') void addSite(siteFromDrop(e.dataTransfer));
            else void addFiles(e.dataTransfer.files);
          }}
        >
          <HeaderTitle>
            <div class="flex min-w-0 items-center gap-2.5">
              <Folder
                size={20}
                stroke-width={1.6}
                class="h-[18px] w-[18px] shrink-0 sm:h-5 sm:w-5"
                style={{ color: courseColor(c().color) }}
                fill={courseColor(c().color)}
                fill-opacity="0.18"
                aria-hidden="true"
              />
              <EditableTitle value={c().title} onSave={(title) => db.update('course', c().id, { title })} />
            </div>
          </HeaderTitle>
          <HeaderEndActions>
            <A
              href={`/courses/${params.courseKey}/graph`}
              class={`${buttonClass('ghost', 'sm')} max-sm:h-9 max-sm:w-9 max-sm:px-0`}
              aria-label="Knowledge graph"
              title="Knowledge graph"
            >
              <Waypoints size={18} stroke-width={1.75} class="sm:h-4 sm:w-4" />
              <span class="hidden lg:inline">Graph</span>
            </A>
            <AddMaterial
              onFiles={(files) => void addFiles(files)}
              onWrite={write}
              onSource={() => {
                showTab('sources');
                setAddingSource(true);
              }}
              onDocument={(format) => void newDocument(format)}
            />
            <CourseSettings course={c()} />
          </HeaderEndActions>

          <Show when={c().description}>
            <p class="max-w-2xl break-words pt-3 text-[15px] leading-snug text-muted sm:pt-5 sm:text-[16px]">{c().description}</p>
          </Show>
          <div class="sticky top-0 z-10 -mx-3 mb-3 bg-surface px-3 pb-2 pt-3 sm:pt-5">
            <CourseSearch courseId={c().id} />
            <div class="mt-2 flex items-center gap-2">
              <div role="tablist" aria-label="Course content" class="scroll-quiet -mx-1 flex min-w-0 flex-1 items-center gap-0.5 overflow-x-auto px-1">
                <For each={TABS}>
                  {(t) => (
                    <button
                      type="button"
                      role="tab"
                      id={`course-tab-${t.value}`}
                      aria-selected={tab() === t.value}
                      aria-controls="course-tab-panel"
                      class={`flex h-11 shrink-0 items-center rounded-full px-2 text-[13px] transition-colors sm:px-3.5 sm:text-[14px] md:h-8 ${
                        tab() === t.value ? 'bg-soft font-medium text-fg' : 'text-muted hover:bg-soft hover:text-fg'
                      }`}
                      onClick={() => showTab(t.value)}
                    >
                      {t.label}
                    </button>
                  )}
                </For>
              </div>
              <Show when={tab() !== 'sources' && (tab() as SortedTab)} keyed>
                {(t) => <SortPicker value={sorts()[t]} onChange={(sort) => setSort(t, sort)} />}
              </Show>
            </div>
            {/* The list fades out as it slides under the search. */}
            <div
              class={`pointer-events-none absolute inset-x-0 top-full h-6 bg-linear-to-b from-surface to-surface/0 transition-opacity ${scrolled() ? 'opacity-100' : 'opacity-0'}`}
            />
          </div>

          <Show when={addingSource()}>
            <AddSourceSheet courseId={c().id} onClose={() => setAddingSource(false)} />
          </Show>
          {/* Remounted per source, so its live queries never switch in place. */}
          <Show when={openSource()} keyed>
            {(f) => <SourceSheet fetcher={f} onClose={() => setOpenSource(null)} />}
          </Show>

          <div role="tabpanel" id="course-tab-panel" aria-labelledby={`course-tab-${tab()}`}>
            <Switch>
              <Match when={tab() === 'homework'}>
                <HomeworkSection courseId={c().id} courseKey={params.courseKey ?? ''} sort={sorts().homework} />
              </Match>
              <Match when={tab() === 'documents'}>
                <DocumentsSection courseId={c().id} courseKey={params.courseKey ?? ''} sort={sorts().documents} onNew={(format) => void newDocument(format)} />
              </Match>
              <Match when={tab() === 'sites'}>
                <SitesSection
                  courseId={c().id}
                  courseKey={params.courseKey ?? ''}
                  sort={sorts().sites}
                  dragging={dragging()}
                  progress={siteProgress()}
                  notice={siteNotice()}
                  onFiles={(files) => void addSite(siteFromFiles(files))}
                />
              </Match>
              <Match when={tab() === 'sources'}>
                <SourcesSection courseId={c().id} onOpen={setOpenSource} onAdd={() => setAddingSource(true)} />
              </Match>
              <Match when={tab() === 'material'}>
                <section class={`-mx-3 rounded-2xl ${dragging() ? 'ring-2 ring-blue/40' : ''}`}>
                  <Show when={notice()}>
                    <p class="mx-3 mb-2 shrink-0 break-words rounded-xl bg-red-soft px-3.5 py-2.5 text-[14px] text-red">{notice()}</p>
                  </Show>

                  <Show when={writing()}>
                    <TextMaterialForm
                      onCancel={() => setWriting(false)}
                      onSave={async (title, content) => {
                        setWriting(false);
                        await addText(title, content, 'markdown');
                      }}
                    />
                  </Show>

                  {/* Remounted per course and order, so its live queries never switch in place. */}
                  <Show when={`${courseId()}|${sorts().material}`} keyed>
                    {(key) => {
                      const [id, sort] = key.split('|') as [string, Sort];
                      return <MaterialList courseId={id} courseKey={params.courseKey ?? ''} sort={sort} onDelete={removeMaterial} />;
                    }}
                  </Show>
                </section>
              </Match>
            </Switch>
          </div>

          {/* The page's bottom space. */}
          <div class="h-16" aria-hidden="true" />
        </div>
      )}
    </Show>
  );
}

function EditableTitle(props: { value: string; onSave: (v: string) => void }) {
  const [editing, setEditing] = createSignal(false);
  return (
    <Show
      when={editing()}
      fallback={
        <h1 class="min-w-0 cursor-text truncate text-[16px] font-semibold tracking-tight sm:text-[17px]" onClick={() => setEditing(true)} title="Click to rename">
          {props.value}
        </h1>
      }
    >
      <input
        class="-mx-2 h-9 w-full min-w-0 rounded-lg bg-soft px-2 text-[16px] font-semibold tracking-tight outline-none sm:text-[17px]"
        value={props.value}
        aria-label="Course name"
        ref={(el) => queueMicrotask(() => el.select())}
        onKeyDown={(e) => {
          if (e.key === 'Enter') e.currentTarget.blur();
          if (e.key === 'Escape') setEditing(false);
        }}
        onBlur={(e) => {
          const v = e.currentTarget.value.trim();
          if (v && v !== props.value) props.onSave(v);
          setEditing(false);
        }}
      />
    </Show>
  );
}

/** Upload files, write notes or start a document, from one button in the top bar. */
function AddMaterial(props: { onFiles: (files: FileList) => void; onWrite: () => void; onSource: () => void; onDocument: (format: 'markdown' | 'tex') => void }) {
  const [open, setOpen] = createSignal(false);
  let input!: HTMLInputElement;
  const item = 'flex h-11 w-full items-center gap-2.5 rounded-xl px-3 text-[15px] hover:bg-soft md:h-10';
  return (
    <div class="relative">
      <Button variant="ghost" size="sm" class="max-sm:h-9 max-sm:w-9 max-sm:px-0" aria-label="Add material" aria-haspopup="menu" aria-expanded={open()} onClick={() => setOpen(!open())}>
        <Plus size={18} stroke-width={1.9} class="sm:h-4 sm:w-4" />
        <span class="hidden sm:inline">Add</span>
      </Button>
      {/* Outside the menu, so closing the menu cannot drop the file picker's result. */}
      <input
        ref={input}
        type="file"
        class="sr-only"
        tabindex="-1"
        multiple
        accept="application/pdf,.pdf,.tex,.md,.markdown,.txt,text/plain,text/markdown,text/x-tex"
        onChange={(e) => {
          if (e.currentTarget.files?.length) props.onFiles(e.currentTarget.files);
          e.currentTarget.value = '';
        }}
      />
      <Show when={open()}>
        <div class="fixed inset-0 z-40" onClick={() => setOpen(false)} />
        <div
          role="menu"
          class="absolute right-0 z-50 mt-2 w-56 rounded-2xl border border-line bg-surface p-1.5 shadow-card"
          onKeyDown={(e) => e.key === 'Escape' && setOpen(false)}
        >
          <button
            type="button"
            role="menuitem"
            class={item}
            onClick={() => {
              setOpen(false);
              input.click();
            }}
          >
            <Upload size={16} stroke-width={1.75} class="text-muted" /> Upload files
          </button>
          <button
            type="button"
            role="menuitem"
            class={item}
            onClick={() => {
              setOpen(false);
              props.onWrite();
            }}
          >
            <PenLine size={16} stroke-width={1.75} class="text-muted" /> Write notes
          </button>
          <button
            type="button"
            role="menuitem"
            class={item}
            onClick={() => {
              setOpen(false);
              props.onSource();
            }}
          >
            <Radar size={16} stroke-width={1.75} class="text-muted" /> Add a source
          </button>
          <button
            type="button"
            role="menuitem"
            class={item}
            onClick={() => {
              setOpen(false);
              props.onDocument('markdown');
            }}
          >
            <ScrollText size={16} stroke-width={1.75} class="text-muted" /> New document
          </button>
          <button
            type="button"
            role="menuitem"
            class={item}
            onClick={() => {
              setOpen(false);
              props.onDocument('tex');
            }}
          >
            <FileCode2 size={16} stroke-width={1.75} class="text-muted" /> New LaTeX document
          </button>
        </div>
      </Show>
    </div>
  );
}

function TextMaterialForm(props: { onSave: (title: string, content: string) => void; onCancel: () => void }) {
  const [title, setTitle] = createSignal('');
  const [content, setContent] = createSignal('');
  return (
    <form
      class="mx-3 mb-3 shrink-0 space-y-3 rounded-2xl bg-soft p-3"
      onSubmit={(e) => {
        e.preventDefault();
        props.onSave(title().trim(), content());
      }}
    >
      <input class={inputClass} placeholder="Title, e.g. Lecture 4 notes" value={title()} onInput={(e) => setTitle(e.currentTarget.value)} aria-label="Title" required />
      <textarea
        class={`${inputClass} min-h-56 resize-y font-mono text-[16px] leading-relaxed sm:text-[13px]`}
        placeholder="Paste or write your notes. Markdown works: # headings, lists, $math$."
        value={content()}
        onInput={(e) => setContent(e.currentTarget.value)}
        aria-label="Content"
        required
      />
      <div class="flex flex-wrap justify-end gap-2">
        <Button variant="ghost" size="sm" onClick={props.onCancel}>
          Cancel
        </Button>
        <Button type="submit" size="sm" disabled={!title().trim() || !content().trim()}>
          Add material
        </Button>
      </div>
    </form>
  );
}

type CourseRow = TableModel<GetTable<typeof schema, 'course'>>;
type FetcherRow = TableModel<GetTable<typeof schema, 'fetcher'>>;

/** The gear button in the top bar and the course settings dialog it opens. */
function CourseSettings(props: { course: CourseRow }) {
  const db = useDb<typeof schema>();
  const navigate = useNavigate();
  const save = (patch: Record<string, unknown>) => db.update('course', props.course.id, patch);
  const [open, setOpen] = createSignal(false);

  return (
    <>
      <Button
        variant="ghost"
        size="sm"
        class="w-11 max-sm:h-9 max-sm:w-9 max-sm:px-0 sm:px-0 md:w-8"
        aria-label="Course settings"
        title="Course settings"
        onClick={() => setOpen(true)}
      >
        <Settings2 size={18} stroke-width={1.75} class="sm:h-4 sm:w-4" />
      </Button>
      <Show when={open()}>
        <Sheet title="Course settings" maxWidth="28rem" onClose={() => setOpen(false)}>
          {(close) => (
            <div class="space-y-4">
              <Field label="The tutor speaks">
                <select class={inputClass} value={props.course.language} onChange={(e) => save({ language: e.currentTarget.value })}>
                  <For each={LANGUAGES}>{(l) => <option value={l.code}>{l.label}</option>}</For>
                </select>
              </Field>
              <Field label="Notes for the tutor" hint="ChatGPT follows them in every study session of this course.">
                <textarea
                  class={`${inputClass} min-h-24 resize-y text-[16px] sm:text-[14px]`}
                  placeholder="e.g. The exam is oral. Push me on proofs."
                  value={props.course.tutor_notes ?? ''}
                  onChange={(e) => save({ tutor_notes: e.currentTarget.value.trim() || undefined })}
                />
              </Field>
              <Field label="Description">
                <textarea
                  class={`${inputClass} min-h-16 resize-y text-[16px] sm:text-[14px]`}
                  value={props.course.description ?? ''}
                  onChange={(e) => save({ description: e.currentTarget.value.trim() || undefined })}
                />
              </Field>
              <div>
                <span class="mb-1.5 block text-[13px] text-muted">Color</span>
                <div class="flex flex-wrap gap-1">
                  <For each={Object.keys(COURSE_COLORS) as CourseColor[]}>
                    {(key) => (
                      <button
                        type="button"
                        aria-label={key}
                        aria-pressed={props.course.color === key}
                        onClick={() => save({ color: key })}
                        class="flex h-11 w-11 shrink-0 items-center justify-center rounded-full"
                      >
                        <span class={`h-7 w-7 rounded-full ring-offset-2 ring-offset-surface ${props.course.color === key ? 'ring-2 ring-fg' : ''}`} style={{ background: COURSE_COLORS[key] }} />
                      </button>
                    )}
                  </For>
                </div>
              </div>
              <Button
                variant="ghost"
                size="sm"
                class="-ml-3 text-red"
                onClick={async () => {
                  if (confirm(`Delete “${props.course.title}” with all its material and study sessions? This cannot be undone.`)) {
                    close();
                    await db.delete('course', props.course.id);
                    navigate('/');
                  }
                }}
              >
                Delete course
              </Button>
            </div>
          )}
        </Sheet>
      </Show>
    </>
  );
}

/** How a tab's list is ordered: a pill in the tab row that opens the native picker. */
function SortPicker(props: { value: Sort; onChange: (sort: Sort) => void }) {
  return (
    <label class={`${buttonClass('ghost', 'sm')} relative shrink-0 cursor-pointer text-muted hover:text-fg max-sm:w-11 max-sm:px-0`} title="Sort">
      <ArrowDownUp size={16} stroke-width={1.75} aria-hidden="true" />
      <span class="hidden sm:inline">{SORTS.find((s) => s.value === props.value)?.label}</span>
      <select
        class="absolute inset-0 cursor-pointer appearance-none opacity-0"
        aria-label="Sort by"
        onChange={(e) => props.onChange(e.currentTarget.value as Sort)}
      >
        <For each={SORTS}>
          {(s) => (
            <option value={s.value} selected={s.value === props.value}>
              {s.label}
            </option>
          )}
        </For>
      </select>
    </label>
  );
}

/** A tab with nothing in it yet, and how to change that. */
function EmptyTab(props: { title: JSX.Element; children: JSX.Element; actions?: JSX.Element }) {
  return (
    <div class="mt-1 rounded-2xl border border-dashed border-line px-4 py-8 text-center sm:px-6 sm:py-10">
      <p class="text-[15px] font-medium">{props.title}</p>
      <p class="mx-auto mt-1 max-w-md text-[14px] text-muted">{props.children}</p>
      <Show when={props.actions}>
        <div class="mt-4 flex flex-wrap justify-center gap-2">{props.actions}</div>
      </Show>
    </div>
  );
}

type MaterialRow = TableModel<GetTable<typeof schema, 'material'>>;

/**
 * The course's material, virtualized and scrolling with the page. By date it
 * comes in live windows in the database's order. By name it is one list
 * sorted here: the database's order would put "Lecture 10" before "Lecture 2".
 */
function MaterialList(props: { courseId: string; courseKey: string; sort: Sort; onDelete: (id: string, title: string) => void }) {
  const db = useDb<typeof schema>();
  const byName = props.sort === 'name';
  const dir = props.sort === 'newest' ? 'desc' : 'asc';
  return (
    <VirtualList
      pageScroll
      aria-label="Lecture material"
      // By name, one window holds every row.
      pageSize={byName ? Infinity : undefined}
      window={(page, size): ListWindow<MaterialRow> => {
        if (!byName) return useQuery(() => qMaterialsPage(db, props.courseId, page, size, dir), { deregisterOnCleanup: true });
        const all = useQuery(() => qMaterials(db, props.courseId), { deregisterOnCleanup: true });
        const sorted = createMemo(() => sortRows(all.data() ?? [], 'name'));
        return { data: sorted, isSettled: () => all.isSettled() };
      }}
      estimateSize={56}
      key={(m) => m.id}
      empty={
        <div class="mx-3">
          <EmptyTab
            title={
              <>
                <span class="sm:hidden">No lecture material yet</span>
                <span class="max-sm:hidden">Drop lecture files here</span>
              </>
            }
          >
            <span class="sm:hidden">Tap + to add PDF slides, Markdown or text notes</span>
            <span class="max-sm:hidden">PDF slides and scripts, LaTeX, Markdown or text notes</span>
          </EmptyTab>
        </div>
      }
    >
      {(m) => (
        <div class="group flex items-center gap-3 rounded-xl px-3 py-2 transition-colors hover:bg-soft">
          <span class="shrink-0 text-faint">
            {m().kind === 'pdf' ? (
              <FileType2 size={18} stroke-width={1.6} />
            ) : m().kind === 'tex' ? (
              <FileCode2 size={18} stroke-width={1.6} />
            ) : (
              <FileText size={18} stroke-width={1.6} />
            )}
          </span>
          <A href={`/courses/${props.courseKey}/materials/${idKey(m().id)}`} class="flex min-w-0 flex-1 items-center gap-3">
            <span class="min-w-0 flex-1">
              <span class="block truncate text-[15px]">{m().title}</span>
              <span class="mt-0.5 flex flex-wrap items-center gap-x-3">
                <StatusText status={m().status} detail={m().status_detail} />
                <Show when={m().page_count}>
                  <span class="text-[13px] text-faint">{m().page_count} pages</span>
                </Show>
                <Show when={m().status === 'ready' && m().chunk_count > 0}>
                  <span class="text-[13px] text-faint">
                    {(m().learned_through ?? -1) < 0 ? 'Not started' : `Reached ${m().learned_through + 1} of ${m().chunk_count} sections`}
                  </span>
                </Show>
                <Show when={m().status === 'failed' && m().error}>
                  <span class="min-w-0 max-w-full truncate text-[13px] text-red sm:max-w-md" title={m().error ?? ''}>
                    {m().error}
                  </span>
                </Show>
              </span>
            </span>
            <span class="shrink-0 text-[13px] text-faint" title={`Added ${formatDate(m().created_at, true)}`}>
              {formatDate(m().created_at)}
            </span>
          </A>
          <IconButton
            label={`Delete ${m().title}`}
            size="sm"
            class="shrink-0 sm:opacity-0 sm:group-hover:opacity-100 sm:focus:opacity-100"
            onClick={() => props.onDelete(m().id, m().title)}
          >
            <Trash2 size={16} stroke-width={1.75} />
          </IconButton>
        </div>
      )}
    </VirtualList>
  );
}

/** The course's documents: a proposal, an essay, written with papers beside them. */
function DocumentsSection(props: { courseId: string; courseKey: string; sort: Sort; onNew: (format: 'markdown' | 'tex') => void }) {
  const db = useDb<typeof schema>();
  const documents = useQuery(() => qDocumentList(db, props.courseId));
  const rows = createMemo(() => sortRows(documents.data() ?? [], props.sort));
  const remove = async (id: string, title: string) => {
    if (confirm(`Delete “${title}”? Its text and its list of papers go with it. The papers stay in the course.`)) await db.delete('document', id);
  };
  return (
    <Show
      when={rows().length > 0}
      fallback={
        <Show when={documents.isSettled()}>
          <EmptyTab
            title="No documents yet"
            actions={
              <>
                <Button variant="soft" size="sm" onClick={() => props.onNew('markdown')}>
                  New document
                </Button>
                <Button variant="ghost" size="sm" onClick={() => props.onNew('tex')}>
                  New LaTeX document
                </Button>
              </>
            }
          >
            Write a proposal or an essay with the papers it draws on beside it.
          </EmptyTab>
        </Show>
      }
    >
      <section class="-mx-3">
        <For each={rows()}>
          {(d) => (
            <div class="group flex items-center gap-3 rounded-xl px-3 py-2 transition-colors hover:bg-soft">
              <A href={`/courses/${props.courseKey}/documents/${idKey(d.id)}`} class="flex min-w-0 flex-1 items-center gap-3">
                <span class="shrink-0 text-faint">
                  <ScrollText size={18} stroke-width={1.6} />
                </span>
                <span class="min-w-0 flex-1 truncate text-[15px]">{d.title}</span>
                <span class="shrink-0 text-[13px] text-faint" title={`Added ${formatDate(d.created_at, true)}, edited ${formatDate(d.updated_at, true)}`}>
                  {formatDate(d.created_at)}
                </span>
              </A>
              <IconButton
                label={`Delete ${d.title}`}
                size="sm"
                class="shrink-0 sm:opacity-0 sm:group-hover:opacity-100 sm:focus:opacity-100"
                onClick={() => void remove(d.id, d.title)}
              >
                <Trash2 size={16} stroke-width={1.75} />
              </IconButton>
            </div>
          )}
        </For>
      </section>
    </Show>
  );
}

const sizeLabel = (bytes: number) => (bytes < 1024 * 1024 ? `${Math.max(1, Math.round(bytes / 1024))} KB` : `${(bytes / 1024 / 1024).toFixed(1)} MB`);

/** The course's uploaded static sites, audio and all. */
function SitesSection(props: {
  courseId: string;
  courseKey: string;
  sort: Sort;
  dragging: boolean;
  progress: string | null;
  notice: string | null;
  onFiles: (files: File[]) => void;
}) {
  const db = useDb<typeof schema>();
  const sites = useQuery(() => qSiteList(db, props.courseId));
  const rows = createMemo(() => sortRows(sites.data() ?? [], props.sort));
  const remove = async (id: string, title: string) => {
    if (confirm(`Delete “${title}” and all its files? Its link stops working.`)) await db.delete('site', id);
  };
  let folder!: HTMLInputElement;
  let file!: HTMLInputElement;
  const picked = (e: { currentTarget: HTMLInputElement }) => {
    const files = Array.from(e.currentTarget.files ?? []);
    e.currentTarget.value = '';
    if (files.length) props.onFiles(files);
  };
  const actions = () => (
    <>
      <Button variant="soft" size="sm" class="max-sm:hidden" disabled={!!props.progress} onClick={() => folder.click()}>
        <FolderUp size={16} stroke-width={1.75} /> Upload a folder
      </Button>
      <Button variant="ghost" size="sm" disabled={!!props.progress} onClick={() => file.click()}>
        <Upload size={16} stroke-width={1.75} /> Upload .zip or .html
      </Button>
    </>
  );
  return (
    <section class={`-mx-3 rounded-2xl ${props.dragging ? 'ring-2 ring-blue/40' : ''}`}>
      <input ref={(el) => { folder = el; el.webkitdirectory = true; }} type="file" class="sr-only" tabindex="-1" onChange={picked} />
      <input ref={file} type="file" class="sr-only" tabindex="-1" accept=".zip,.html,.htm,application/zip,text/html" onChange={picked} />
      <Show when={props.progress}>
        <p class="mx-3 mb-2 rounded-xl bg-soft px-3.5 py-2.5 text-[14px] text-muted" role="status">{props.progress}</p>
      </Show>
      <Show when={props.notice}>
        <p class="mx-3 mb-2 break-words rounded-xl bg-red-soft px-3.5 py-2.5 text-[14px] text-red">{props.notice}</p>
      </Show>
      <Show
        when={rows().length > 0}
        fallback={
          <Show when={sites.isSettled()}>
            <div class="mx-3">
              <EmptyTab title="No sites yet" actions={actions()}>
                Drop a built website here, its HTML, scripts and audio together. It opens inside the course and has a link you can share.
              </EmptyTab>
            </div>
          </Show>
        }
      >
        <div class="mx-3 mb-2 flex flex-wrap justify-end gap-2">{actions()}</div>
        <For each={rows()}>
          {(site) => (
            <div class="group flex items-center gap-3 rounded-xl px-3 py-2 transition-colors hover:bg-soft">
              <A href={`/courses/${props.courseKey}/sites/${idKey(site.id)}`} class="flex min-w-0 flex-1 items-center gap-3">
                <span class="shrink-0 text-faint">
                  <Globe size={18} stroke-width={1.6} />
                </span>
                <span class="min-w-0 flex-1">
                  <span class="block truncate text-[15px]">{site.title}</span>
                  <span class="mt-0.5 block text-[13px] text-faint">
                    {site.status === 'ready' ? `${site.file_count} ${site.file_count === 1 ? 'file' : 'files'} · ${sizeLabel(site.size)}` : 'Uploading'}
                  </span>
                </span>
                <span class="shrink-0 text-[13px] text-faint" title={`Added ${formatDate(site.created_at, true)}`}>
                  {formatDate(site.created_at)}
                </span>
              </A>
              <IconButton
                label={`Delete ${site.title}`}
                size="sm"
                class="shrink-0 sm:opacity-0 sm:group-hover:opacity-100 sm:focus:opacity-100"
                onClick={() => void remove(site.id, site.title)}
              >
                <Trash2 size={16} stroke-width={1.75} />
              </IconButton>
            </div>
          )}
        </For>
      </Show>
    </section>
  );
}

/** The course's homework: the sheets the student is answering. */
function HomeworkSection(props: { courseId: string; courseKey: string; sort: Sort }) {
  const db = useDb<typeof schema>();
  const homework = useQuery(() => qHomeworkList(db, props.courseId));
  const rows = createMemo(() => sortRows(homework.data() ?? [], props.sort));
  const remove = async (id: string, title: string) => {
    if (confirm(`Delete your answers to “${title}”? The exercise sheet stays in the course.`)) await db.delete('homework', id);
  };
  return (
    <Show
      when={rows().length > 0}
      fallback={
        <Show when={homework.isSettled()}>
          <EmptyTab title="No homework yet">
            Open an exercise sheet under Material and choose Answer to work on it part by part. ChatGPT can start one for you too.
          </EmptyTab>
        </Show>
      }
    >
      <section class="-mx-3">
        <For each={rows()}>
          {(h) => (
            <div class="group flex items-center gap-3 rounded-xl px-3 py-2 transition-colors hover:bg-soft">
              <A href={`/courses/${props.courseKey}/homework/${idKey(h.id)}`} class="flex min-w-0 flex-1 items-center gap-3">
                <span class="shrink-0 text-faint">
                  <NotebookPen size={18} stroke-width={1.6} />
                </span>
                <span class="min-w-0 flex-1 truncate text-[15px]">{h.title}</span>
                <span class="shrink-0 text-[13px] text-faint" title={`Added ${formatDate(h.created_at, true)}, edited ${formatDate(h.updated_at, true)}`}>
                  {formatDate(h.created_at)}
                </span>
              </A>
              <IconButton
                label={`Delete ${h.title}`}
                size="sm"
                class="shrink-0 sm:opacity-0 sm:group-hover:opacity-100 sm:focus:opacity-100"
                onClick={() => void remove(h.id, h.title)}
              >
                <Trash2 size={16} stroke-width={1.75} />
              </IconButton>
            </div>
          )}
        </For>
      </section>
    </Show>
  );
}
