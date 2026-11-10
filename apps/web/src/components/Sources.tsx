import { createSignal, For, type JSX, Match, Show, Switch } from 'solid-js';
import { type GetTable, type TableModel, useDb, useQuery } from '@spooky-sync/client-solid';
import { ExternalLink, Globe, GraduationCap, RefreshCw, Settings2 } from 'lucide-solid';
import type { schema } from '../schema.gen';
import {
  type CanvasCourse,
  createSource,
  listCanvasCourses,
  previewSource,
  type PreviewStatus,
  type SourceKind,
  type SourcePreview,
  sourceMapping,
  updateSource,
} from '../lib/api';
import { idString } from '../lib/ids';
import { checkSource } from '../lib/jobs';
import { qFetchers, qFetchRuns } from '../lib/query';
import { Button, Dot, Field, formatDate, IconButton, inputClass, Loader, Segmented, type Tone } from './ui';
import { Sheet } from './Sheet';

/**
 * Sources of a course: a bCourses course or a course website that TalkToBolo
 * checks on a schedule, importing new and changed lecture PDFs (and pages)
 * as material by itself (apps/api/src/fetch). The list is the course page's
 * Sources tab; the sheet adds and edits one.
 */

type FetcherRow = TableModel<GetTable<typeof schema, 'fetcher'>>;
type RunRow = TableModel<GetTable<typeof schema, 'fetch_run'>>;

const BCOURSES = 'https://bcourses.berkeley.edu';
const EVERY = [
  { value: '1', label: 'Every hour' },
  { value: '6', label: 'Every 6 hours' },
  { value: '24', label: 'Once a day' },
  { value: '168', label: 'Once a week' },
];

const errorText = (err: unknown) => (err instanceof Error ? err.message : 'Something went wrong. Try again.');

/** "5 min ago", "3 h ago", or the date. */
function ago(v: unknown): string {
  if (!v) return '';
  const t = (v instanceof Date ? v : new Date(String(v))).getTime();
  if (Number.isNaN(t)) return '';
  const min = Math.round((Date.now() - t) / 60_000);
  if (min < 1) return 'just now';
  if (min < 60) return `${min} min ago`;
  if (min < 24 * 60) return `${Math.round(min / 60)} h ago`;
  return formatDate(v);
}

function sourceTone(f: FetcherRow): Tone {
  if (f.status === 'running') return 'running';
  if (f.status === 'failed' || f.status === 'partial') return 'attention';
  if (!f.enabled) return 'idle';
  return f.status === 'ok' ? 'ready' : 'idle';
}

function sourceLine(f: FetcherRow): string {
  if (f.status === 'running') return 'Checking for new material…';
  const imported = f.imported_count ? `${f.imported_count} imported` : 'Nothing imported yet';
  const checked = f.last_run_at ? `checked ${ago(f.last_run_at)}` : 'not checked yet';
  return `${f.enabled ? '' : 'Paused · '}${imported} · ${checked}`;
}

const KindIcon = (props: { kind: string; size?: number }) =>
  props.kind === 'canvas' ? <GraduationCap size={props.size ?? 18} stroke-width={1.6} /> : <Globe size={props.size ?? 18} stroke-width={1.6} />;

/** The course's sources, in the course page's Sources tab. */
export function SourcesSection(props: { courseId: string; onOpen: (f: FetcherRow) => void; onAdd: () => void }) {
  const db = useDb<typeof schema>();
  const fetchers = useQuery(() => qFetchers(db, props.courseId));
  const [notice, setNotice] = createSignal<string | null>(null);

  const check = async (f: FetcherRow) => {
    setNotice(null);
    await checkSource(db, idString(f.id)).catch((err) => setNotice(`${f.name}: ${errorText(err)}`));
  };

  return (
    <Show
      when={(fetchers.data() ?? []).length > 0}
      fallback={
        <Show when={fetchers.isSettled()}>
          <div class="mt-1 rounded-2xl border border-dashed border-line px-4 py-8 text-center sm:px-6 sm:py-10">
            <p class="text-[15px] font-medium">No sources yet</p>
            <p class="mx-auto mt-1 max-w-md text-[14px] text-muted">Add a bCourses course or a course website, and new lecture files come in by themselves.</p>
            <Button variant="soft" size="sm" class="mt-4" onClick={props.onAdd}>
              Add a source
            </Button>
          </div>
        </Show>
      }
    >
      <section class="-mx-3">
        <Show when={notice()}>
          <p class="mx-3 mb-2 break-words rounded-xl bg-red-soft px-3.5 py-2.5 text-[14px] text-red">{notice()}</p>
        </Show>
        <ul>
          <For each={fetchers.data() ?? []}>
            {(f) => (
              <li class="group flex items-center gap-3 rounded-xl px-3 py-2 transition-colors hover:bg-soft">
                <span class="shrink-0 text-faint">
                  <KindIcon kind={f.kind} />
                </span>
                <button type="button" class="min-w-0 flex-1 text-left" onClick={() => props.onOpen(f)}>
                  <span class="block truncate text-[15px]">{f.name}</span>
                  <span class="mt-0.5 flex min-w-0 items-center gap-2 text-[13px] text-muted">
                    <Dot tone={sourceTone(f)} />
                    <span class="truncate">{sourceLine(f)}</span>
                  </span>
                  <Show when={f.last_error && f.status !== 'running'}>
                    <span class="mt-0.5 block truncate text-[13px] text-red" title={f.last_error ?? ''}>
                      {f.last_error}
                    </span>
                  </Show>
                </button>
                <IconButton label={`Check ${f.name} now`} size="sm" disabled={f.status === 'running'} onClick={() => void check(f)}>
                  <RefreshCw size={16} stroke-width={1.75} class={f.status === 'running' ? 'animate-spin' : ''} />
                </IconButton>
                <IconButton label={`Settings of ${f.name}`} size="sm" onClick={() => props.onOpen(f)}>
                  <Settings2 size={16} stroke-width={1.75} />
                </IconButton>
              </li>
            )}
          </For>
        </ul>
      </section>
    </Show>
  );
}

const STATUS_LABEL: Record<PreviewStatus, string> = { new: 'New', changed: 'Changed', imported: 'Imported', dismissed: 'Deleted by you' };

function PreviewList(props: { preview: SourcePreview }) {
  const counts = () => {
    const by = (s: PreviewStatus) => props.preview.items.filter((i) => i.status === s).length;
    return { fresh: by('new') + by('changed'), total: props.preview.total };
  };
  return (
    <div class="rounded-2xl bg-soft p-3">
      <p class="px-1 text-[13px] text-muted">
        {props.preview.total === 0
          ? `Nothing to import among the ${props.preview.listed} items found. Try fewer words in the filter, or include pages.`
          : `${props.preview.total} of ${props.preview.listed} items match${counts().fresh ? `, ${counts().fresh} to import now` : ''}.`}
      </p>
      <Show when={props.preview.items.length}>
        <ul class="mt-2 max-h-64 overflow-y-auto">
          <For each={props.preview.items}>
            {(i) => (
              <li class="flex items-center gap-2 rounded-lg px-1 py-1.5 text-[14px]">
                <span class="w-12 shrink-0 text-[12px] uppercase text-faint">{i.kind === 'pdf' ? 'PDF' : i.kind === 'tex' ? 'TeX' : 'Page'}</span>
                <a href={i.link} target="_blank" rel="noopener noreferrer" class="min-w-0 flex-1 truncate hover:underline">
                  {i.title}
                </a>
                <span class={`shrink-0 text-[12px] ${i.status === 'new' || i.status === 'changed' ? 'text-fg' : 'text-faint'}`}>{STATUS_LABEL[i.status]}</span>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <For each={props.preview.issues.slice(0, 3)}>{(issue) => <p class="mt-1 px-1 text-[12px] text-faint">{issue}</p>}</For>
    </div>
  );
}

function Checkbox(props: { checked: boolean; onChange: (v: boolean) => void; children: JSX.Element }) {
  return (
    <label class="flex min-h-11 cursor-pointer items-center gap-3 text-[15px] md:min-h-8">
      <input type="checkbox" class="h-4 w-4 accent-[var(--color-fg)]" checked={props.checked} onChange={(e) => props.onChange(e.currentTarget.checked)} />
      <span>{props.children}</span>
    </label>
  );
}

/** Adding a source to a course. */
export function AddSourceSheet(props: { courseId: string; onClose: () => void }) {
  const db = useDb<typeof schema>();
  const [kind, setKind] = createSignal<SourceKind>('canvas');
  const [canvasUrl, setCanvasUrl] = createSignal(BCOURSES);
  const [token, setToken] = createSignal('');
  const [courses, setCourses] = createSignal<CanvasCourse[] | null>(null);
  const [canvasCourse, setCanvasCourse] = createSignal('');
  const [pageUrl, setPageUrl] = createSignal('');
  const [include, setInclude] = createSignal('');
  const [pages, setPages] = createSignal(false);
  const [tex, setTex] = createSignal(false);
  const [every, setEvery] = createSignal('6');
  const [preview, setPreview] = createSignal<SourcePreview | null>(null);
  const [busy, setBusy] = createSignal<'courses' | 'preview' | 'save' | null>(null);
  const [error, setError] = createSignal<string | null>(null);

  const settings = () =>
    kind() === 'canvas'
      ? { kind: 'canvas' as const, url: canvasUrl(), canvas_course: canvasCourse(), token: token().trim(), include: include(), import_pages: pages(), import_tex: tex() }
      : { kind: 'web' as const, url: pageUrl().trim(), include: include(), import_pages: pages(), import_tex: tex() };
  const ready = () => (kind() === 'canvas' ? !!token().trim() && !!canvasCourse() : !!pageUrl().trim());

  const attempt = async (what: 'courses' | 'preview' | 'save', fn: () => Promise<void>) => {
    setBusy(what);
    setError(null);
    try {
      await fn();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(null);
    }
  };

  const findCourses = () =>
    attempt('courses', async () => {
      const { courses } = await listCanvasCourses(db, { url: canvasUrl(), token: token().trim() });
      setCourses(courses);
      if (courses.length && !canvasCourse()) setCanvasCourse(courses[0].id);
    });

  return (
    <Sheet
      title="Add a source"
      subtitle="TalkToBolo checks it on a schedule and adds new lecture files to this course by itself."
      maxWidth="34rem"
      onClose={props.onClose}
      footer={(close) => (
        <div class="flex flex-wrap justify-end gap-2">
          <Button variant="ghost" onClick={close}>
            Cancel
          </Button>
          <Button variant="soft" disabled={!ready() || busy() !== null} onClick={() => attempt('preview', async () => void setPreview(await previewSource(db, settings())))}>
            <Show when={busy() === 'preview'} fallback="Preview">
              <Loader size={4} label="Loading the preview" />
            </Show>
          </Button>
          <Button
            disabled={!ready() || busy() !== null}
            onClick={() =>
              attempt('save', async () => {
                await createSource(db, { ...settings(), course: props.courseId, every_hours: Number(every()) });
                close();
              })
            }
          >
            <Show when={busy() === 'save'} fallback="Add source">
              <Loader size={4} label="Adding the source" class="text-solid-fg" />
            </Show>
          </Button>
        </div>
      )}
    >
      {() => (
        <div class="space-y-4">
          <Segmented
            label="Kind of source"
            value={kind()}
            options={[
              { value: 'canvas', label: 'bCourses' },
              { value: 'web', label: 'Course website' },
            ]}
            onChange={(v) => {
              setKind(v);
              setPreview(null);
              setError(null);
            }}
          />

          <Switch>
            <Match when={kind() === 'canvas'}>
              <Field
                label="Access token"
                hint="In bCourses: Account, Settings, then + New access token. It is stored encrypted and only used to read your courses."
              >
                <input
                  class={inputClass}
                  type="password"
                  autocomplete="off"
                  spellcheck={false}
                  placeholder="Paste the token"
                  value={token()}
                  onInput={(e) => {
                    setToken(e.currentTarget.value);
                    setCourses(null);
                  }}
                />
              </Field>
              <a
                href={`${canvasUrl().replace(/\/+$/, '') || BCOURSES}/profile/settings`}
                target="_blank"
                rel="noopener noreferrer"
                class="-mt-2 inline-flex items-center gap-1.5 text-[13px] text-muted hover:text-fg"
              >
                Open bCourses settings <ExternalLink size={13} stroke-width={1.75} />
              </a>
              <Show
                when={courses()}
                fallback={
                  <Button variant="soft" size="sm" disabled={!token().trim() || busy() !== null} onClick={findCourses}>
                    <Show when={busy() === 'courses'} fallback="Find my courses">
                      <Loader size={4} label="Finding your courses" />
                    </Show>
                  </Button>
                }
              >
                {(list) => (
                  <Field label="Course">
                    <select class={inputClass} value={canvasCourse()} onChange={(e) => (setCanvasCourse(e.currentTarget.value), setPreview(null))}>
                      <For each={list()} fallback={<option value="">No active courses</option>}>
                        {(c) => <option value={c.id}>{[c.term, c.code ?? c.name].filter(Boolean).join(' · ')}</option>}
                      </For>
                    </select>
                  </Field>
                )}
              </Show>
              <details class="text-[13px] text-muted">
                <summary class="cursor-pointer select-none">Another Canvas than bCourses</summary>
                <input class={`${inputClass} mt-2`} value={canvasUrl()} onInput={(e) => (setCanvasUrl(e.currentTarget.value), setCourses(null))} aria-label="Canvas address" />
              </details>
            </Match>
            <Match when={kind() === 'web'}>
              <Field label="Page address" hint="The page that links the lecture PDFs, such as the course schedule.">
                <input
                  class={inputClass}
                  type="url"
                  inputmode="url"
                  placeholder="https://cs61a.org/"
                  value={pageUrl()}
                  onInput={(e) => (setPageUrl(e.currentTarget.value), setPreview(null))}
                />
              </Field>
            </Match>
          </Switch>

          <Field label="Only files whose name contains" hint="Comma-separated, e.g. lecture, slides. Leave empty for every PDF.">
            <input class={inputClass} placeholder="lecture, slides" value={include()} onInput={(e) => (setInclude(e.currentTarget.value), setPreview(null))} />
          </Field>
          <Checkbox checked={pages()} onChange={(v) => (setPages(v), setPreview(null))}>
            {kind() === 'canvas' ? 'Also import bCourses pages as notes' : "Also import the page's own text as notes"}
          </Checkbox>
          <Checkbox checked={tex()} onChange={(v) => (setTex(v), setPreview(null))}>
            Also import LaTeX sources (.tex)
          </Checkbox>
          <Field label="Check for new material">
            <select class={inputClass} value={every()} onChange={(e) => setEvery(e.currentTarget.value)}>
              <For each={EVERY}>{(o) => <option value={o.value}>{o.label}</option>}</For>
            </select>
          </Field>

          <Show when={error()}>
            <p class="break-words rounded-xl bg-red-soft px-3.5 py-2.5 text-[14px] text-red">{error()}</p>
          </Show>
          <Show when={preview()}>{(p) => <PreviewList preview={p()} />}</Show>
        </div>
      )}
    </Sheet>
  );
}

/** A source's settings, history and mapping. Mounted per source. */
export function SourceSheet(props: { fetcher: FetcherRow; onClose: () => void }) {
  const db = useDb<typeof schema>();
  const id = idString(props.fetcher.id);
  const fetchers = useQuery(() => qFetchers(db, idString(props.fetcher.course)));
  // The live row, so status and history update while the sheet is open.
  const f = () => fetchers.data()?.find((x) => idString(x.id) === id) ?? props.fetcher;
  const runs = useQuery(() => qFetchRuns(db, id));

  const [name, setName] = createSignal(props.fetcher.name);
  const [include, setInclude] = createSignal(props.fetcher.include ?? '');
  const [pages, setPages] = createSignal(props.fetcher.import_pages);
  const [tex, setTex] = createSignal(props.fetcher.import_tex ?? false);
  const [every, setEvery] = createSignal(String(props.fetcher.every_hours));
  const [token, setToken] = createSignal('');
  const [preview, setPreview] = createSignal<SourcePreview | null>(null);
  const [mapping, setMapping] = createSignal<{ custom: boolean; text: string } | null>(null);
  const [busy, setBusy] = createSignal<string | null>(null);
  const [error, setError] = createSignal<string | null>(null);

  const attempt = async (what: string, fn: () => Promise<void>) => {
    setBusy(what);
    setError(null);
    try {
      await fn();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(null);
    }
  };

  const changes = () => ({
    ...(name().trim() && name().trim() !== props.fetcher.name ? { name: name().trim() } : {}),
    ...(include() !== (props.fetcher.include ?? '') ? { include: include() } : {}),
    ...(pages() !== props.fetcher.import_pages ? { import_pages: pages() } : {}),
    ...(tex() !== (props.fetcher.import_tex ?? false) ? { import_tex: tex() } : {}),
    ...(Number(every()) !== props.fetcher.every_hours ? { every_hours: Number(every()) } : {}),
    ...(token().trim() ? { token: token().trim() } : {}),
  });

  const loadMapping = () =>
    attempt('mapping', async () => {
      const m = await sourceMapping(db, id);
      setMapping({ custom: m.custom, text: JSON.stringify(m.mapping, null, 2) });
    });

  return (
    <Sheet
      title={f().name}
      icon={
        <span class="flex h-10 w-10 items-center justify-center rounded-2xl bg-soft text-muted">
          <KindIcon kind={f().kind} size={20} />
        </span>
      }
      subtitle={
        <a href={f().kind === 'canvas' ? `${f().url}/courses/${f().canvas_course}` : f().url} target="_blank" rel="noopener noreferrer" class="hover:text-fg">
          {f().kind === 'canvas' ? `bCourses course ${f().canvas_course}` : f().url}
        </a>
      }
      maxWidth="36rem"
      onClose={props.onClose}
      footer={(close) => (
        <div class="flex flex-wrap justify-end gap-2">
          <Button
            variant="soft"
            disabled={busy() !== null}
            onClick={() => attempt('preview', async () => void setPreview(await previewSource(db, { fetcher: id, include: include(), import_pages: pages(), import_tex: tex(), ...(token().trim() ? { token: token().trim() } : {}) })))}
          >
            <Show when={busy() === 'preview'} fallback="Preview">
              <Loader size={4} label="Loading the preview" />
            </Show>
          </Button>
          <Button
            disabled={busy() !== null || Object.keys(changes()).length === 0}
            onClick={() =>
              attempt('save', async () => {
                await updateSource(db, id, changes());
                close();
              })
            }
          >
            <Show when={busy() === 'save'} fallback="Save">
              <Loader size={4} label="Saving" class="text-solid-fg" />
            </Show>
          </Button>
        </div>
      )}
    >
      {(close) => (
        <div class="space-y-4">
          <div class="flex items-center gap-2 text-[14px] text-muted">
            <Dot tone={sourceTone(f())} />
            <span class="min-w-0 flex-1">{sourceLine(f())}</span>
            <Button variant="soft" size="sm" disabled={f().status === 'running'} onClick={() => void checkSource(db, id).catch((err) => setError(errorText(err)))}>
              <RefreshCw size={14} stroke-width={1.75} /> Check now
            </Button>
          </div>
          <Show when={f().last_error}>
            <p class="break-words rounded-xl bg-red-soft px-3.5 py-2.5 text-[14px] text-red">{f().last_error}</p>
          </Show>

          <Checkbox checked={f().enabled} onChange={(v) => db.update('fetcher', id, { enabled: v })}>
            Check on schedule
          </Checkbox>
          <Field label="Name">
            <input class={inputClass} value={name()} onInput={(e) => setName(e.currentTarget.value)} />
          </Field>
          <Show when={f().kind === 'canvas'}>
            <Field label="New access token" hint="Only when bCourses stopped accepting the saved one.">
              <input class={inputClass} type="password" autocomplete="off" spellcheck={false} placeholder="Keep the saved token" value={token()} onInput={(e) => setToken(e.currentTarget.value)} />
            </Field>
          </Show>
          <Show
            when={!f().mapping}
            fallback={<p class="rounded-xl bg-soft px-3.5 py-2.5 text-[14px] text-muted">This source runs its own mapping, so the filter below does not apply.</p>}
          >
            <Field label="Only files whose name contains" hint="Comma-separated. Leave empty for every PDF.">
              <input class={inputClass} placeholder="lecture, slides" value={include()} onInput={(e) => setInclude(e.currentTarget.value)} />
            </Field>
            <Checkbox checked={pages()} onChange={setPages}>
              {f().kind === 'canvas' ? 'Also import bCourses pages as notes' : "Also import the page's own text as notes"}
            </Checkbox>
            <Checkbox checked={tex()} onChange={setTex}>
              Also import LaTeX sources (.tex)
            </Checkbox>
          </Show>
          <Field label="Check for new material">
            <select class={inputClass} value={every()} onChange={(e) => setEvery(e.currentTarget.value)}>
              <For each={EVERY}>{(o) => <option value={o.value}>{o.label}</option>}</For>
              <Show when={!EVERY.some((o) => o.value === every())}>
                <option value={every()}>Every {every()} hours</option>
              </Show>
            </select>
          </Field>

          <Show when={error()}>
            <p class="break-words rounded-xl bg-red-soft px-3.5 py-2.5 text-[14px] text-red">{error()}</p>
          </Show>
          <Show when={preview()}>{(p) => <PreviewList preview={p()} />}</Show>

          <div>
            <h3 class="pb-1 text-[13px] text-muted">Recent checks</h3>
            <ul class="space-y-1">
              <For each={runs.data() ?? []} fallback={<li class="text-[14px] text-faint">No checks yet.</li>}>
                {(r) => <RunLine run={r} />}
              </For>
            </ul>
          </div>

          <details class="text-[14px]" onToggle={(e) => e.currentTarget.open && !mapping() && loadMapping()}>
            <summary class="cursor-pointer select-none text-[13px] text-muted">Mapping (advanced)</summary>
            <p class="mt-2 text-[13px] leading-relaxed text-muted">
              A hockeymask mapping turns what the source lists into lecture material: its rows have id, name, kind, content_type,
              ext, url, link, updated_at, module and size, and its sink writes <code>import</code> rows.
            </p>
            <Show when={mapping()} fallback={<Loader size={4} label="Loading the mapping" class="mt-2 text-faint" />}>
              {(m) => (
                <div class="mt-2 space-y-2">
                  <textarea
                    class={`${inputClass} min-h-64 resize-y font-mono text-[12px] leading-relaxed`}
                    spellcheck={false}
                    value={m().text}
                    onInput={(e) => setMapping({ ...m(), text: e.currentTarget.value })}
                    aria-label="Mapping JSON"
                  />
                  <div class="flex flex-wrap justify-end gap-2">
                    <Show when={f().mapping}>
                      <Button
                        variant="ghost"
                        size="sm"
                        disabled={busy() !== null}
                        onClick={() => attempt('mapping', async () => (await updateSource(db, id, { mapping: null }), setMapping(null), await loadMapping()))}
                      >
                        Use the template again
                      </Button>
                    </Show>
                    <Button variant="soft" size="sm" disabled={busy() !== null} onClick={() => attempt('mapping', async () => void (await updateSource(db, id, { mapping: m().text })))}>
                      Save mapping
                    </Button>
                  </div>
                </div>
              )}
            </Show>
          </details>

          <Button
            variant="ghost"
            size="sm"
            class="-ml-3 text-red"
            onClick={async () => {
              if (confirm(`Delete the source “${f().name}”? The material it imported stays in the course.`)) {
                close();
                await db.delete('fetcher', id);
              }
            }}
          >
            Delete source
          </Button>
        </div>
      )}
    </Sheet>
  );
}

function RunLine(props: { run: RunRow }) {
  const r = () => props.run;
  const tone = (): Tone => (r().status === 'running' ? 'running' : r().status === 'ok' ? 'ready' : 'attention');
  const summary = () => {
    if (r().status === 'running') return 'Checking…';
    const parts = [
      r().created ? `${r().created} new` : null,
      r().updated ? `${r().updated} updated` : null,
      r().failed ? `${r().failed} failed` : null,
      r().deferred ? `${r().deferred} next time` : null,
    ].filter(Boolean);
    return parts.length ? parts.join(', ') : r().status === 'failed' ? 'Failed' : 'Nothing new';
  };
  return (
    <li class="rounded-xl px-1 py-1">
      <div class="flex items-center gap-2 text-[14px]">
        <Dot tone={tone()} />
        <span class="w-28 shrink-0 text-muted">{formatDate(r().started_at, true)}</span>
        <span class="min-w-0 flex-1 truncate">{summary()}</span>
      </div>
      <Show when={r().detail}>
        <details class="ml-4 mt-0.5">
          <summary class="cursor-pointer text-[12px] text-faint">Details</summary>
          <pre class="mt-1 whitespace-pre-wrap break-words text-[12px] text-muted">{r().detail}</pre>
        </details>
      </Show>
    </li>
  );
}
