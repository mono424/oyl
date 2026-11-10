import { createEffect, createMemo, createSignal, For, type JSX, Show } from 'solid-js';
import { type GetTable, type TableModel, useDb, useQuery } from '@spooky-sync/client-solid';
import { ArrowLeft, ExternalLink, Pencil, Plus, Quote, Search, Trash2, Upload, X } from 'lucide-solid';
import type { schema } from '../schema.gen';
import { lectureMarkdown } from '../../../../shared/lecture';
import { materialPath } from '../lib/api';
import type { CiteStyle } from '../lib/citations';
import { makeCiteKey } from '../../../../shared/references';
import { addPdfMaterial } from '../lib/documents';
import { idKey, idString, newId } from '../lib/ids';
import { renderMarkdown } from '../lib/markdown';
import { firstRow, qMaterial, qMaterials } from '../lib/query';
import { Button, Field, IconButton, inputClass, Segmented } from './ui';
import { PdfViewer } from './PdfViewer';

/**
 * The papers beside a document (schema `paper`): the list to cite from, a
 * paper open to read (its PDF, or the material's text) with its page to
 * cite, and adding papers: from the course's material, a PDF uploaded to
 * the course, or a reference typed in by hand.
 */

export type PaperRow = TableModel<GetTable<typeof schema, 'paper'>>;

export interface Reading {
  key: string;
  page: number | null;
}

interface Details {
  cite_key: string;
  title: string;
  authors: string;
  year: string;
  venue: string;
  url: string;
}

const byline = (p: { authors?: string | null; year?: string | null; venue?: string | null }) =>
  [p.authors, p.year, p.venue].map((v) => v?.trim()).filter(Boolean).join(' · ');

export function PapersPanel(props: {
  owner: string;
  documentId: string;
  courseId: string;
  papers: PaperRow[];
  /** How often the text cites each paper, by key. */
  cited: ReadonlyMap<string, number>;
  style: CiteStyle;
  onStyle: (style: CiteStyle) => void;
  reading: Reading | null;
  onRead: (reading: Reading | null) => void;
  onCite: (key: string, locator?: string) => void;
  /** Without it the panel's own tabs close it. */
  onClose?: () => void;
  /** What starts a citation in the text: `[@` or `\cite{`. */
  citeStart?: string;
}) {
  const db = useDb<typeof schema>();
  const [view, setView] = createSignal<'list' | 'add' | { edit: string }>('list');
  const open = createMemo(() => {
    const r = props.reading;
    return r ? (props.papers.find((p) => idKey(p.id) === r.key) ?? null) : null;
  });
  const editing = createMemo(() => {
    const v = view();
    return typeof v === 'object' ? (props.papers.find((p) => idString(p.id) === v.edit) ?? null) : null;
  });

  const remove = async (p: PaperRow) => {
    const n = props.cited.get(idKey(p.id)) ?? 0;
    const warning = n ? ` The text cites it ${n === 1 ? 'once' : `${n} times`}; those citations will show as missing.` : '';
    if (!confirm(`Remove “${p.title}” from this document?${warning}`)) return;
    if (props.reading?.key === idKey(p.id)) props.onRead(null);
    await db.delete('paper', p.id);
  };

  const close = () => (
    <Show when={props.onClose}>
      {(onClose) => (
        <IconButton label="Hide the papers (⌘E)" size="sm" onClick={onClose()}>
          <X size={15} stroke-width={1.9} />
        </IconButton>
      )}
    </Show>
  );

  return (
    <div class="flex h-full min-h-0 flex-col">
      <Show
        when={open()}
        keyed
        fallback={
          <Show
            when={view() === 'list'}
            fallback={
              <>
                <div class="flex shrink-0 items-center gap-1 border-b border-line px-2 py-1.5">
                  <IconButton label="Back to the papers" size="sm" onClick={() => setView('list')}>
                    <ArrowLeft size={15} stroke-width={1.9} />
                  </IconButton>
                  <span class="min-w-0 flex-1 truncate text-[13px] font-medium">{editing() ? 'Paper details' : 'Add papers'}</span>
                  {close()}
                </div>
                <div class="scroll-quiet min-h-0 flex-1 overflow-y-auto overscroll-contain p-3">
                  <Show
                    when={editing()}
                    keyed
                    fallback={
                      <AddPapers owner={props.owner} documentId={props.documentId} courseId={props.courseId} papers={props.papers} onDone={() => setView('list')} />
                    }
                  >
                    {(p) => (
                      <PaperForm
                        initial={p}
                        taken={props.papers.filter((o) => o.id !== p.id).map((o) => o.cite_key ?? '')}
                        submit="Save"
                        onCancel={() => setView('list')}
                        onSave={async (d) => {
                          await db.update('paper', p.id, d);
                          setView('list');
                        }}
                      />
                    )}
                  </Show>
                </div>
              </>
            }
          >
            <div class="flex shrink-0 items-center gap-2 border-b border-line py-1.5 pl-3.5 pr-2">
              <span class="min-w-0 flex-1 truncate text-[13px] font-medium">
                <Show when={props.onClose} fallback={<span class="font-normal text-muted">Cite from these, or type {props.citeStart ?? '[@'} in the text</span>}>
                  Papers <span class="font-normal text-faint">{props.papers.length || ''}</span>
                </Show>
              </span>
              <Button variant="ghost" size="sm" onClick={() => setView('add')}>
                <Plus size={15} stroke-width={1.9} /> Add
              </Button>
              {close()}
            </div>
            <div class="scroll-quiet min-h-0 flex-1 overflow-y-auto overscroll-contain p-2">
              <Show
                when={props.papers.length}
                fallback={
                  <div class="px-3 py-8 text-center">
                    <p class="text-[14px] font-medium">No papers yet</p>
                    <p class="mx-auto mt-1 max-w-72 text-[13px] text-muted">
                      Add the papers you want to cite: from the course's material, a PDF, or typed in by hand. Then cite them here or type [@ in the text.
                    </p>
                    <Button variant="soft" size="sm" class="mt-4" onClick={() => setView('add')}>
                      <Plus size={15} stroke-width={1.9} /> Add papers
                    </Button>
                  </div>
                }
              >
                <ul>
                  <For each={props.papers}>
                    {(p) => {
                      const n = () => props.cited.get(idKey(p.id)) ?? 0;
                      return (
                        <li class="rounded-xl px-3 py-2.5 transition-colors hover:bg-soft">
                          <button type="button" class="block w-full text-left" onClick={() => props.onRead({ key: idKey(p.id), page: null })} title="Open the paper">
                            <span class="line-clamp-2 text-[14px] font-medium leading-snug">{p.title}</span>
                            <span class="mt-0.5 block truncate text-[12px] text-muted">
                              <Show when={p.cite_key}>
                                <span class="mr-1.5 font-mono text-faint">{p.cite_key}</span>
                              </Show>
                              {byline(p) || 'No authors or year yet'}
                            </span>
                          </button>
                          <div class="mt-1.5 flex items-center gap-1.5">
                            <Button variant="soft" size="sm" onClick={() => props.onCite(idKey(p.id))} title="Cite it at the cursor">
                              <Quote size={13} stroke-width={1.9} /> Cite
                            </Button>
                            <span class="min-w-0 flex-1 truncate text-[12px] text-faint">{n() ? `cited ${n()}×` : 'not cited yet'}</span>
                            <IconButton label="Edit the details" size="sm" onClick={() => setView({ edit: idString(p.id) })}>
                              <Pencil size={14} stroke-width={1.8} />
                            </IconButton>
                            <IconButton label="Remove from this document" size="sm" onClick={() => void remove(p)}>
                              <Trash2 size={14} stroke-width={1.8} />
                            </IconButton>
                          </div>
                        </li>
                      );
                    }}
                  </For>
                </ul>
              </Show>
            </div>
            <div class="flex shrink-0 flex-wrap items-center gap-2 border-t border-line px-3.5 py-2 text-[12px] text-muted">
              <span>Citations print as</span>
              <Segmented
                label="Citation style"
                value={props.style}
                options={[
                  { value: 'author-year', label: '(Author, year)' },
                  { value: 'numeric', label: '[1]' },
                ]}
                onChange={props.onStyle}
              />
            </div>
          </Show>
        }
      >
        {(p) => (
          <Reader
            paper={p}
            page={props.reading?.page ?? null}
            onBack={() => props.onRead(null)}
            onCite={(locator) => props.onCite(idKey(p.id), locator)}
            onEdit={() => {
              props.onRead(null);
              setView({ edit: idString(p.id) });
            }}
            close={close}
          />
        )}
      </Show>
    </div>
  );
}

/** One paper open beside the text: its PDF or text, and its page to cite. */
function Reader(props: {
  paper: PaperRow;
  page: number | null;
  onBack: () => void;
  onCite: (locator?: string) => void;
  onEdit: () => void;
  close: () => JSX.Element;
}) {
  const [page, setPage] = createSignal(props.page ?? 1);
  const [isPdf, setIsPdf] = createSignal(false);
  return (
    <>
      <div class="flex shrink-0 items-center gap-1 border-b border-line px-2 py-1.5">
        <IconButton label="Back to the papers" size="sm" onClick={props.onBack}>
          <ArrowLeft size={15} stroke-width={1.9} />
        </IconButton>
        <button type="button" class="min-w-0 flex-1 truncate text-left text-[13px] font-medium hover:text-muted" onClick={props.onEdit} title="Edit the details">
          {props.paper.title}
        </button>
        <Button variant="soft" size="sm" onClick={() => props.onCite()} title="Cite the paper at the cursor">
          <Quote size={13} stroke-width={1.9} /> Cite
        </Button>
        <Show when={isPdf()}>
          <Button variant="soft" size="sm" onClick={() => props.onCite(`p. ${page()}`)} title="Cite the page you are reading">
            p. {page()}
          </Button>
        </Show>
        {props.close()}
      </div>
      <div class="scroll-quiet min-h-0 flex-1 overflow-y-auto overscroll-contain p-3">
        <Show
          when={props.paper.material ? idString(props.paper.material) : null}
          keyed
          fallback={
            <div class="space-y-2 px-1 py-2 text-[14px]">
              <p class="font-medium">{props.paper.title}</p>
              <Show when={byline(props.paper)}>
                <p class="text-muted">{byline(props.paper)}</p>
              </Show>
              <Show when={props.paper.url?.trim()}>
                {(url) => (
                  <a href={url()} target="_blank" rel="noopener noreferrer" class="inline-flex items-center gap-1 break-all text-blue underline underline-offset-2">
                    {url()} <ExternalLink size={13} stroke-width={1.9} />
                  </a>
                )}
              </Show>
              <p class="pt-2 text-[13px] text-faint">A reference typed in by hand. Add its PDF to the course to read it here.</p>
            </div>
          }
        >
          {(id) => <MaterialBody materialId={id} page={props.page} onPage={setPage} onPdf={setIsPdf} />}
        </Show>
      </div>
    </>
  );
}

export function MaterialBody(props: { materialId: string; page: number | null; onPage: (page: number) => void; onPdf: (pdf: boolean) => void }) {
  const db = useDb<typeof schema>();
  const material = firstRow(useQuery(() => qMaterial(db, props.materialId)));
  createEffect(() => props.onPdf(material.data()?.kind === 'pdf' && !!material.data()?.source_size));
  const html = createMemo(() => {
    const m = material.data();
    return m && m.kind !== 'pdf' ? renderMarkdown(lectureMarkdown(m.kind, m.content)) : '';
  });
  return (
    <Show when={material.data()} fallback={<p class="py-8 text-center text-[14px] text-muted">{material.isLoading() ? 'Loading…' : 'This material is no longer in the course.'}</p>}>
      {(m) => (
        <>
          <Show when={m().kind === 'pdf'} fallback={<div class="lecture" innerHTML={html()} />}>
            <Show when={m().source_size} fallback={<p class="py-8 text-center text-[14px] text-muted">The PDF is still being uploaded.</p>}>
              <PdfViewer materialId={idString(m().id)} size={m().source_size} fileName={m().source_name} page={props.page} onPage={props.onPage} />
            </Show>
          </Show>
          <p class="pt-3 text-center text-[12px]">
            <a href={materialPath(idString(m().course), idString(m().id))} class="text-muted underline underline-offset-2 hover:text-fg">
              Open in the course
            </a>
          </p>
        </>
      )}
    </Show>
  );
}

/** Adding papers: from the course's material, a PDF, or by hand. */
function AddPapers(props: { owner: string; documentId: string; courseId: string; papers: PaperRow[]; onDone: () => void }) {
  const db = useDb<typeof schema>();
  const materials = useQuery(() => qMaterials(db, props.courseId));
  const [query, setQuery] = createSignal('');
  const [manual, setManual] = createSignal(false);
  const [notice, setNotice] = createSignal<string | null>(null);
  let input!: HTMLInputElement;

  const added = createMemo(() => new Set(props.papers.map((p) => (p.material ? idString(p.material) : '')).filter(Boolean)));
  const choices = createMemo(() => {
    const q = query().trim().toLowerCase();
    return (materials.data() ?? []).filter((m) => !added().has(idString(m.id)) && (!q || m.title.toLowerCase().includes(q)));
  });

  const taken = () => props.papers.map((p) => p.cite_key ?? '').filter(Boolean);
  const create = (fields: Partial<Details> & { title: string; material?: string }) => {
    const clean = Object.fromEntries(Object.entries(fields).filter(([, v]) => typeof v === 'string' && v.trim()));
    const cite_key = fields.cite_key?.trim() || makeCiteKey(fields, taken());
    return db.create(newId('paper'), { owner: props.owner, document: props.documentId, ...clean, cite_key, title: fields.title.trim() });
  };

  const upload = async (files: FileList) => {
    setNotice(null);
    for (const file of Array.from(files)) {
      if (!/\.pdf$/i.test(file.name) && file.type !== 'application/pdf') {
        setNotice(`${file.name} was skipped: add PDFs.`);
        continue;
      }
      const { id, title, uploaded } = await addPdfMaterial(db, { owner: props.owner, course: props.courseId, file });
      await create({ title, material: id });
      uploaded.catch((err) => setNotice(`${file.name}: ${err instanceof Error ? err.message : 'upload failed'}`));
    }
    props.onDone();
  };

  return (
    <Show
      when={!manual()}
      fallback={
        <PaperForm
          taken={taken()}
          submit="Add paper"
          onCancel={() => setManual(false)}
          onSave={async (d) => {
            await create(d);
            props.onDone();
          }}
        />
      }
    >
      <div class="space-y-3">
        <div class="flex flex-wrap gap-2">
          <Button variant="soft" size="sm" onClick={() => input.click()}>
            <Upload size={14} stroke-width={1.9} /> Upload a PDF
          </Button>
          <Button variant="soft" size="sm" onClick={() => setManual(true)}>
            <Pencil size={14} stroke-width={1.9} /> Enter by hand
          </Button>
          <input
            ref={input}
            type="file"
            class="sr-only"
            tabindex="-1"
            multiple
            accept="application/pdf,.pdf"
            onChange={(e) => {
              if (e.currentTarget.files?.length) void upload(e.currentTarget.files);
              e.currentTarget.value = '';
            }}
          />
        </div>
        <Show when={notice()}>
          <p class="rounded-xl bg-red-soft px-3 py-2 text-[13px] text-red">{notice()}</p>
        </Show>
        <p class="pt-1 text-[12px] text-muted">An uploaded PDF joins the course's material too, so ChatGPT can search it.</p>
        <div>
          <p class="pb-1.5 pt-2 text-[13px] text-muted">From the course</p>
          <div class="relative">
            <Search size={15} stroke-width={1.9} class="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-faint" />
            <input class={`${inputClass} pl-9 text-[14px]`} placeholder="Find material" value={query()} onInput={(e) => setQuery(e.currentTarget.value)} aria-label="Find material" />
          </div>
          <ul class="mt-2">
            <For each={choices()} fallback={<li class="px-3 py-4 text-center text-[13px] text-faint">{(materials.data() ?? []).length ? 'Nothing else matches.' : 'This course has no material yet.'}</li>}>
              {(m) => (
                <li>
                  <button
                    type="button"
                    class="flex w-full items-center gap-2 rounded-xl px-3 py-2 text-left text-[14px] transition-colors hover:bg-soft"
                    onClick={() => void create({ title: m.title, material: idString(m.id) })}
                  >
                    <span class="min-w-0 flex-1 truncate">{m.title}</span>
                    <Plus size={15} stroke-width={1.9} class="shrink-0 text-faint" />
                  </button>
                </li>
              )}
            </For>
          </ul>
        </div>
      </div>
    </Show>
  );
}

/** A paper's bibliography details. Cleared fields are saved empty. */
function PaperForm(props: {
  initial?: Partial<Record<keyof Details, string | null | undefined>>;
  /** The cite keys of the document's other papers. */
  taken: string[];
  submit: string;
  onSave: (details: Details) => Promise<void>;
  onCancel: () => void;
}) {
  const [d, setD] = createSignal<Details>({
    cite_key: props.initial?.cite_key ?? '',
    title: props.initial?.title ?? '',
    authors: props.initial?.authors ?? '',
    year: props.initial?.year ?? '',
    venue: props.initial?.venue ?? '',
    url: props.initial?.url ?? '',
  });
  const [saving, setSaving] = createSignal(false);
  const keyProblem = () => {
    const k = d().cite_key.trim();
    if (!k) return null;
    if (!/^[A-Za-z0-9_][A-Za-z0-9_:-]*$/.test(k)) return 'Letters, digits, _ : and - only.';
    if (props.taken.some((t) => t.toLowerCase() === k.toLowerCase())) return 'Another paper has this key.';
    return null;
  };
  const set = (k: keyof Details) => (e: { currentTarget: HTMLInputElement }) => setD((v) => ({ ...v, [k]: e.currentTarget.value }));
  return (
    <form
      class="space-y-3"
      onSubmit={async (e) => {
        e.preventDefault();
        if (!d().title.trim() || keyProblem()) return;
        setSaving(true);
        try {
          const details = { title: d().title.trim(), authors: d().authors.trim(), year: d().year.trim(), venue: d().venue.trim(), url: d().url.trim() };
          await props.onSave({ ...details, cite_key: d().cite_key.trim() || makeCiteKey(details, props.taken) });
        } finally {
          setSaving(false);
        }
      }}
    >
      <Field label="Title">
        <input class={`${inputClass} text-[14px]`} value={d().title} onInput={set('title')} required placeholder="Attention Is All You Need" />
      </Field>
      <Field label="Authors" hint="Separate them with semicolons.">
        <input class={`${inputClass} text-[14px]`} value={d().authors} onInput={set('authors')} placeholder="Vaswani, A.; Shazeer, N.; Parmar, N." />
      </Field>
      <div class="grid grid-cols-[6rem_1fr] gap-3">
        <Field label="Year">
          <input class={`${inputClass} text-[14px]`} value={d().year} onInput={set('year')} inputmode="numeric" placeholder="2017" />
        </Field>
        <Field label="Published in">
          <input class={`${inputClass} text-[14px]`} value={d().venue} onInput={set('venue')} placeholder="NeurIPS" />
        </Field>
      </div>
      <Field label="Link">
        <input class={`${inputClass} text-[14px]`} value={d().url} onInput={set('url')} type="url" placeholder="https://arxiv.org/abs/1706.03762" />
      </Field>
      <Field label="Cite key" hint={keyProblem() ?? 'What the text cites it by: \\cite{key} or [@key]. Left empty, it is made from the first author and year. Changing it does not change citations already written.'}>
        <input class={`${inputClass} font-mono text-[14px]`} value={d().cite_key} onInput={set('cite_key')} placeholder="vaswani2017" spellcheck={false} autocapitalize="off" />
      </Field>
      <div class="flex justify-end gap-2 pt-1">
        <Button variant="ghost" size="sm" onClick={props.onCancel}>
          Cancel
        </Button>
        <Button type="submit" size="sm" disabled={saving() || !d().title.trim() || !!keyProblem()}>
          {props.submit}
        </Button>
      </div>
    </form>
  );
}
