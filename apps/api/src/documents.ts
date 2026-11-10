import { addedImage } from '../../../shared/latex';
import { lectureMarkdown } from '../../../shared/lecture';
import {
  bibtexEntry,
  countCitations,
  draftMarkdown,
  findReferences,
  makeCiteKey,
  markdownCitesToTex,
  markdownInTex,
  matchMaterials,
  texCitesToMarkdown,
} from '../../../shared/references';
import { storeAnswerImage } from './answers';
import type { Env } from './env';
import { appendText, applyEdits, assertStored, type AnswerEdit, type AnswerText } from './homework';
import { BadRequest, NotFound, recordId, retryConflicts, type Surreal } from './surreal';

/**
 * Documents for AI clients (mcp.ts): free writing in a course, such as a
 * research proposal, with the papers it cites (schema `document`, `paper`).
 * Runs with the student's own session, so SurrealDB's permissions apply as in
 * the app, whose editor follows the row live. A Markdown document cites a
 * paper as `[@key]` or `[@key, p. 4]`, several as `[@a; @b]`; a LaTeX one as
 * `\cite{key}` or `\cite[p.~4]{key}`. The key is the paper's cite_key, or
 * its record key for papers from before cite keys.
 *
 * Writes are compare-and-set against the text that was read, so text the
 * student typed meanwhile fails the write instead of being overwritten.
 *
 * Every change of the text is kept as a version, however it was written: the
 * `document_versions` event in schema/src/schema.surql records it inside the
 * write, as a changeset backwards from the current text.
 */

interface DocumentRow {
  id: string;
  title: string;
  course: string;
  source?: string | null;
  format?: string | null;
  content: string;
  cite_style?: string | null;
  updated_at: string;
}

interface PaperRow {
  id: string;
  cite_key?: string | null;
  material?: string | null;
  material_course?: string | null;
  material_kind?: string | null;
  title: string;
  authors?: string | null;
  year?: string | null;
  venue?: string | null;
  url?: string | null;
}

export interface DocumentLinks {
  material: (courseId: string, materialId: string, page?: number | null) => string;
  document: (courseId: string, documentId: string) => string;
}

const key = (id: string) => id.slice(id.indexOf(':') + 1);
const citeKey = (p: { id: string; cite_key?: string | null }) => p.cite_key?.trim() || key(p.id);
const isTex = (doc: { format?: string | null }) => doc.format === 'tex';

const present = (v: string | null | undefined) => (v?.trim() ? v.trim() : undefined);

/** How each format is written, said with every read so a client keeps to it. */
const WRITING = {
  tex: 'LaTeX source, shown and edited as such in the app. Write and edit it only as LaTeX: \\section{...}, \\textbf{...}, \\emph{...}, itemize and enumerate, \\href{url}{text}, math in $...$ or \\[...\\]. Cite with \\cite{key}, never [@key], and never write Markdown into it. Images are \\includegraphics{address}: keep those commands as they are.',
  markdown: 'Markdown: # headings, **bold**, lists, $...$ math. Cite with [@key].',
};

/**
 * Text from an AI client in the document's format: its citations in that
 * format's syntax, and no Markdown in a LaTeX document, which would land in
 * the source as it is.
 */
function inFormat(format: string | null | undefined, content: string): string {
  if (format !== 'tex') return texCitesToMarkdown(content);
  const tex = markdownCitesToTex(content);
  const found = markdownInTex(tex);
  if (found) throw new BadRequest(`This is a LaTeX document (format tex): write LaTeX source, not Markdown. Found ${found}. Cite with \\cite{key}.`);
  return tex;
}

async function loadDocument(user: Surreal, documentId: string) {
  const id = recordId('document', documentId);
  const [doc, papers, version] = await user.queryAll(
    `SELECT id, title, course, source, format, content, cite_style, updated_at FROM ONLY type::record($id);
     SELECT id, cite_key, created_at, material, material.course AS material_course, material.kind AS material_kind, title, authors, year, venue, url
       FROM paper WHERE document = type::record($id) ORDER BY created_at;
     RETURN (SELECT VALUE n FROM document_version WHERE document = type::record($id) ORDER BY n DESC LIMIT 1)[0];`,
    { id },
  ) as [DocumentRow | null, PaperRow[], number | null | undefined];
  if (!doc) throw new NotFound('Document not found.');
  return { doc, papers: papers ?? [], version: version ?? undefined };
}

/** Stores an image for a document and says the line that shows it, for the client to write where it goes. */
export async function addDocumentImage(env: Env, user: Surreal, userId: string, text: AnswerText, documentId: string, body: ReadableStream<Uint8Array> | null) {
  const { doc } = await loadDocument(user, documentId);
  const image = await storeAnswerImage(env, userId, 'document', documentId, body);
  return { document: doc.id, format: doc.format ?? 'markdown', image, write_as: text.external(addedImage(key(image), doc.format)) };
}

/** A document with its text and its papers, each with the citation to write for it. */
export async function readDocument(user: Surreal, links: DocumentLinks, text: AnswerText, documentId: string) {
  const { doc, papers, version } = await loadDocument(user, documentId);
  const counts = countCitations(doc.content ?? '', isTex(doc) ? 'tex' : 'markdown');
  return {
    id: doc.id,
    title: doc.title,
    course: doc.course,
    url: links.document(doc.course, doc.id),
    ...(doc.source ? { started_from: doc.source } : {}),
    format: doc.format ?? 'markdown',
    writing: isTex(doc) ? WRITING.tex : WRITING.markdown,
    cite_style: doc.cite_style ?? 'author-year',
    updated_at: doc.updated_at,
    version,
    content: text.external(doc.content ?? ''),
    papers: papers.map((p) => ({
      id: p.id,
      cite_key: citeKey(p),
      cite: isTex(doc) ? `\\cite{${citeKey(p)}}` : `[@${citeKey(p)}]`,
      title: p.title,
      authors: present(p.authors),
      year: present(p.year),
      venue: present(p.venue),
      url: present(p.url),
      ...(p.material ? { material: p.material, material_kind: p.material_kind ?? undefined } : {}),
      ...(p.material && p.material_course ? { material_url: links.material(p.material_course, p.material) } : {}),
      cited: (counts.get(key(p.id)) ?? 0) + (p.cite_key ? (counts.get(p.cite_key) ?? 0) : 0),
    })),
    // The references.bib the app downloads beside the .tex, for \bibliography{references}.
    ...(isTex(doc) ? { bibtex: papers.map((p) => bibtexEntry({ ...p, key: citeKey(p) })).join('\n\n') } : {}),
  };
}

/** The student's documents, newest first: of one course, or of all. */
export async function listDocuments(user: Surreal, courseId: string | null): Promise<unknown[]> {
  const rows = await user.query<unknown[]>(
    `SELECT id, title, format ?? 'markdown' AS format, course, course.title AS course_title, updated_at,
       string::len(content) AS characters,
       count((SELECT id FROM paper WHERE document = $parent.id)) AS papers
     FROM document ${courseId ? 'WHERE course = type::record($course)' : ''} ORDER BY updated_at DESC`,
    courseId ? { course: recordId('course', courseId) } : {},
  );
  return rows ?? [];
}

/** Starts a document in a course, optionally with its first text. */
export async function createDocument(user: Surreal, text: AnswerText, input: { course_id: string; title: string; content?: string; format?: 'markdown' | 'tex' }) {
  const id = `document:k${crypto.randomUUID().replaceAll('-', '')}`;
  const rows = await user.query<unknown[]>(
    `CREATE type::record($id) CONTENT { owner: $auth.id, course: type::record($course), title: $title, format: $format, content: $content } RETURN VALUE id;`,
    {
      id,
      course: recordId('course', input.course_id),
      title: input.title.trim() || 'Untitled document',
      format: input.format ?? 'markdown',
      content: text.internal(inFormat(input.format, input.content ?? '')),
    },
  );
  if (!rows?.length) throw new NotFound('Course not found.');
  return id;
}

/**
 * Starts a document from a material, as the app's Write button does: its
 * text as Markdown, and the papers it mentions as the document's papers,
 * each linked to the course material holding it. A material with a document
 * already hands that one back.
 */
export async function documentFromMaterial(user: Surreal, materialId: string): Promise<{ id: string; created: boolean }> {
  const material = recordId('material', materialId);
  const [m, existing] = await user.queryAll(
    `SELECT id, course, title, kind, status, content FROM ONLY type::record($material);
     RETURN (SELECT VALUE id FROM document WHERE source = type::record($material) LIMIT 1)[0];`,
    { material },
  ) as [{ id: string; course: string; title: string; kind: string; status: string; content: string } | null, string | null];
  if (!m) throw new NotFound('Material not found.');
  if (existing) return { id: existing, created: false };
  const tex = m.kind === 'tex';
  const markdown = draftMarkdown(lectureMarkdown(m.kind, m.content ?? ''));
  const content = tex ? (m.content ?? '') : markdown;
  if (!content.trim()) throw new BadRequest(m.status === 'ready' ? 'The material has no text to start from.' : 'The material is still being processed; list_materials shows when it is ready.');
  const others = (await user.query<{ id: string; title: string }[]>('SELECT id, title FROM material WHERE course = type::record($course) AND id != type::record($material)', {
    course: m.course,
    material,
  })) ?? [];
  const refs = findReferences(markdown);
  const matches = matchMaterials(refs, others);
  const keys: string[] = [];
  for (const r of refs) keys.push(makeCiteKey(r, keys));
  const id = `document:k${crypto.randomUUID().replaceAll('-', '')}`;
  await user.query(
    `BEGIN;
     CREATE type::record($id) CONTENT { owner: $auth.id, course: type::record($course), source: type::record($material), title: $title, format: $format, content: $content };
     FOR $p IN $papers {
       CREATE type::record($p.id) CONTENT {
         owner: $auth.id, document: type::record($id), material: IF $p.material { type::record($p.material) } ELSE { NONE },
         cite_key: $p.cite_key, title: $p.title, authors: $p.authors, year: $p.year, url: $p.url
       };
     };
     COMMIT;`,
    {
      id,
      course: m.course,
      material,
      title: m.title,
      format: tex ? 'tex' : 'markdown',
      content,
      // Null fields are dropped on the way, so they arrive as NONE.
      papers: refs.map((r, i) => ({
        id: `paper:k${crypto.randomUUID().replaceAll('-', '')}`,
        material: matches[i]?.id ?? null,
        cite_key: keys[i],
        title: r.title,
        authors: r.authors,
        year: r.year,
        url: r.url,
      })),
    },
  );
  return { id, created: true };
}

const CONFLICT = 'conflict: the student changed the document while you wrote. Read it again with get_document and retry.';

/**
 * Saves new text, only if the document still holds `before`. Checks what the
 * database kept: a field guard can leave the old value and still return the row.
 */
async function save(user: Surreal, id: string, before: string, content: string) {
  if (content === before) return 'unchanged' as const;
  const rows = await user.query<string[]>('UPDATE type::record($id) SET content = $content WHERE content = $before RETURN VALUE content;', {
    id,
    content,
    before,
  });
  if (!rows?.length) throw new BadRequest(CONFLICT);
  assertStored(rows[0], content);
  return 'saved' as const;
}

/** Replaces the whole text, or appends a paragraph to it. */
export async function writeDocument(user: Surreal, text: AnswerText, documentId: string, content: string, mode: 'replace' | 'append' = 'replace') {
  const { doc } = await loadDocument(user, documentId);
  const before = doc.content ?? '';
  const incoming = text.internal(inFormat(doc.format, content));
  const next = mode === 'append' ? appendText(before, incoming) : incoming;
  const status = await save(user, doc.id, before, next);
  return { document: doc.id, format: doc.format ?? 'markdown', status, characters: next.length };
}

/** Find-and-replace inside the text. */
export async function editDocument(user: Surreal, text: AnswerText, documentId: string, edits: AnswerEdit[]) {
  const { doc } = await loadDocument(user, documentId);
  const before = doc.content ?? '';
  const next = text.internal(applyEdits(text.external(before), edits.map((e) => ({ ...e, new_text: inFormat(doc.format, e.new_text) }))));
  const status = await save(user, doc.id, before, next);
  return { document: doc.id, format: doc.format ?? 'markdown', status, content: text.external(next) };
}

export interface PaperDetails {
  cite_key?: string;
  title?: string;
  authors?: string;
  year?: string;
  venue?: string;
  url?: string;
}

/** Adds a paper to a document: a course material, a reference by hand, or both. Returns its citation. */
export async function addPaper(user: Surreal, documentId: string, input: PaperDetails & { material_id?: string }) {
  const document = recordId('document', documentId);
  const material = input.material_id ? recordId('material', input.material_id) : null;
  let title = input.title?.trim();
  if (material) {
    const [m, existing] = await user.queryAll(
      `SELECT title FROM ONLY type::record($material);
       RETURN (SELECT id, cite_key FROM paper WHERE document = type::record($document) AND material = type::record($material) LIMIT 1)[0];`,
      { material, document },
    ) as [{ title: string } | null, { id: string; cite_key?: string | null } | null];
    if (!m) throw new NotFound('Material not found.');
    if (existing) return { id: existing.id, cite_key: citeKey(existing), created: false };
    title ||= m.title;
  }
  if (!title) throw new BadRequest('A paper needs a title, or a material_id to take it from.');
  const taken = (await user.query<string[]>('SELECT VALUE cite_key FROM paper WHERE document = type::record($document) AND cite_key != NONE', { document })) ?? [];
  const wanted = input.cite_key?.trim();
  if (wanted && !/^[A-Za-z0-9_][A-Za-z0-9_:-]*$/.test(wanted)) throw new BadRequest('A cite key has letters, digits, _ : and - only.');
  if (wanted && taken.some((k) => k.toLowerCase() === wanted.toLowerCase())) throw new BadRequest(`Another paper of this document has the cite key ${wanted}.`);
  const cite_key = wanted || makeCiteKey({ authors: input.authors, year: input.year, title }, taken);
  const id = `paper:k${crypto.randomUUID().replaceAll('-', '')}`;
  // Null fields are dropped on the way, so they arrive as NONE.
  const rows = await user.query<unknown[]>(
    `CREATE type::record($id) CONTENT {
       owner: $auth.id, document: type::record($document), material: IF $material { type::record($material) } ELSE { NONE },
       cite_key: $cite_key, title: $title, authors: $authors, year: $year, venue: $venue, url: $url
     } RETURN VALUE id;`,
    {
      id,
      cite_key,
      document,
      material,
      title,
      authors: present(input.authors) ?? null,
      year: present(input.year) ?? null,
      venue: present(input.venue) ?? null,
      url: present(input.url) ?? null,
    },
  );
  if (!rows?.length) throw new NotFound('Document not found.');
  return { id, cite_key, created: true };
}

/** Changes a paper's bibliography details. An empty string clears a field. */
export async function updatePaper(user: Surreal, paperId: string, details: PaperDetails) {
  const id = recordId('paper', paperId);
  const set: Record<string, string> = {};
  for (const field of ['cite_key', 'title', 'authors', 'year', 'venue', 'url'] as const) {
    const v = details[field];
    if (v !== undefined) set[field] = v.trim();
  }
  if (set.title === '') throw new BadRequest('A paper keeps its title.');
  if (set.cite_key !== undefined && !/^[A-Za-z0-9_][A-Za-z0-9_:-]*$/.test(set.cite_key)) throw new BadRequest('A cite key has letters, digits, _ : and - only.');
  if (!Object.keys(set).length) throw new BadRequest('Nothing to change.');
  const rows = await user.query<unknown[]>('UPDATE type::record($id) MERGE $set RETURN VALUE id;', { id, set });
  if (!rows?.length) throw new NotFound('Paper not found.');
  return { id, updated: Object.keys(set) };
}

/**
 * Changes a document's title, citation style or format. A new format needs
 * the text in it, except a Markdown copy of a LaTeX draft going back to
 * LaTeX: it takes the draft's source again.
 */
export async function updateDocument(
  user: Surreal,
  text: AnswerText,
  documentId: string,
  input: { title?: string; cite_style?: 'author-year' | 'numeric'; format?: 'markdown' | 'tex'; content?: string },
) {
  const { doc } = await loadDocument(user, documentId);
  const set: Record<string, string> = {};
  if (input.title !== undefined) {
    if (!input.title.trim()) throw new BadRequest('A document keeps its title.');
    set.title = input.title.trim();
  }
  if (input.cite_style) set.cite_style = input.cite_style;
  const before = doc.content ?? '';
  const format = input.format ?? doc.format ?? 'markdown';
  let content: string | undefined = input.content !== undefined ? text.internal(inFormat(format, input.content)) : undefined;
  if (input.format && input.format !== (doc.format ?? 'markdown')) {
    if (content === undefined && input.format === 'tex' && doc.source) {
      const m = await user.query<{ kind: string; content: string } | null>('SELECT kind, content FROM ONLY type::record($id)', { id: doc.source });
      if (m?.kind === 'tex' && m.content?.trim()) content = m.content;
    }
    if (content === undefined) throw new BadRequest(`Give content: the whole text written as ${input.format === 'tex' ? 'LaTeX' : 'Markdown'}.`);
    set.format = input.format;
  }
  if (content !== undefined) set.content = content;
  if (!Object.keys(set).length) throw new BadRequest('Nothing to change.');
  // Compare-and-set when the text changes, like write_document.
  const rows = await user.query<string[]>(
    `UPDATE type::record($id) MERGE $set ${set.content !== undefined ? 'WHERE content = $before' : ''} RETURN VALUE content;`,
    { id: doc.id, set, before },
  );
  if (!rows?.length) throw new BadRequest(set.content !== undefined ? CONFLICT : 'Document not found.');
  if (set.content !== undefined) assertStored(rows[0], set.content);
  return { document: doc.id, updated: Object.keys(set) };
}

/** Takes a paper off a document's list. The course material it reads stays. */
export async function removePaper(user: Surreal, paperId: string) {
  const id = recordId('paper', paperId);
  const rows = await user.query<unknown[]>('DELETE type::record($id) RETURN BEFORE;', { id });
  if (!rows?.length) throw new NotFound('Paper not found.');
  return { removed: id };
}

/** Deletes a document with its list of papers. The course material stays. */
export async function deleteDocument(user: Surreal, documentId: string) {
  const id = recordId('document', documentId);
  const rows = await retryConflicts(() => user.query<unknown[]>('DELETE type::record($id) RETURN BEFORE;', { id }));
  if (!rows?.length) throw new NotFound('Document not found.');
  return { deleted: id };
}

// ── Versions ──────────────────────────────────────────────────────────────

const VERSION_FIELDS = 'n, author, format, added, removed, restored_from, created_at, updated_at';

/** A document's versions, newest first, without their text. */
export async function listVersions(user: Surreal, documentId: string) {
  const id = recordId('document', documentId);
  const [doc, versions] = await user.queryAll(
    `RETURN (SELECT VALUE id FROM ONLY type::record($id));
     SELECT ${VERSION_FIELDS} FROM document_version WHERE document = type::record($id) ORDER BY n DESC;`,
    { id },
  ) as [string | null, VersionRow[]];
  if (!doc) throw new NotFound('Document not found.');
  return { document: id, versions: versions ?? [] };
}

export interface VersionRow {
  n: number;
  author: 'student' | 'assistant' | 'system';
  format: string;
  added: number;
  removed: number;
  restored_from?: number | null;
  created_at: string;
  updated_at: string;
}

/**
 * A version's text and the text of the version before it, rebuilt in the
 * database, whose offsets count code points. It starts from the nearest full
 * copy above the version, or the current text, and undoes the changesets down
 * to it. The SHA-256 taken when it was written says whether it came out right.
 */
async function rebuildVersion(user: Surreal, documentId: string, n: number) {
  if (!Number.isInteger(n) || n < 1) throw new BadRequest('A version is a whole number from 1.');
  const id = recordId('document', documentId);
  const rebuilt = await user.query<(VersionRow & { content: string; previous: string; intact: boolean }) | null>(
    `LET $doc = type::record($id);
     LET $version = (SELECT ${VERSION_FIELDS} FROM document_version WHERE document = $doc AND n = $n)[0];
     RETURN IF $version != NONE {
       LET $head = (SELECT VALUE n FROM document_version WHERE document = $doc ORDER BY n DESC LIMIT 1)[0];
       LET $copy = ($n + 49) / 50 * 50;
       LET $start = IF $copy < $head {
         (SELECT VALUE snapshot FROM ONLY type::record('document_delta', record::id($doc) + 'n' + <string> $copy))
       } ELSE {
         (SELECT VALUE content FROM ONLY $doc)
       };
       LET $undo = |$text: string, $hunks: array| array::fold(array::reverse($hunks), $text, |$t, $h|
         string::slice($t, 0, $h.at) + $h.old + string::slice($t, $h.at + $h.len)
       );
       LET $newer = SELECT VALUE hunks FROM document_delta WHERE document = $doc AND n > $n AND n <= math::min([$copy, $head]) ORDER BY n DESC;
       LET $content = array::fold($newer, $start, |$t, $hunks| $undo($t, $hunks));
       LET $own = (SELECT hunks, sha FROM ONLY type::record('document_delta', record::id($doc) + 'n' + <string> $n));
       object::from_entries(array::concat(object::entries($version), [
         ['content', $content],
         ['previous', IF $n = 1 { '' } ELSE { $undo($content, $own.hunks) }],
         ['intact', crypto::sha256($content) = $own.sha],
       ]))
     };`,
    { id, n },
  );
  if (!rebuilt) throw new NotFound(`This document has no version ${n}.`);
  if (!rebuilt.intact) throw new Error(`Version ${n} of ${id} did not rebuild to the text it was saved with.`);
  return rebuilt;
}

/** A version's text and, to compare, the text before it, both as they are stored. */
export async function readVersion(user: Surreal, documentId: string, n: number) {
  const { intact: _intact, ...version } = await rebuildVersion(user, documentId, n);
  return version;
}

/**
 * Goes back to a version: its text and format become the document's again, as
 * a new version, so nothing is lost and it can be undone in turn. The papers
 * stay as they are. Written as the database user, which starts a version of its
 * own whoever wrote last; a read through the student's session checks the owner.
 */
export async function restoreVersion(user: Surreal, system: Surreal, ownerId: string, author: 'student' | 'assistant', documentId: string, n: number) {
  const id = recordId('document', documentId);
  const { content, format } = await rebuildVersion(user, id, n);
  const { doc, version } = await loadDocument(user, id);
  if ((doc.content ?? '') === content && (doc.format ?? 'markdown') === format) return { document: id, status: 'unchanged' as const, version };
  const stored = await retryConflicts(() => system.query<{ content: string; version: number } | null>(
    `BEGIN;
     LET $doc = type::record($id);
     UPDATE $doc SET content = $content, format = $format WHERE owner = type::record($owner);
     LET $head = (SELECT VALUE n FROM document_version WHERE document = $doc ORDER BY n DESC LIMIT 1)[0];
     UPDATE document_version SET author = $author, restored_from = $n WHERE document = $doc AND n = $head;
     COMMIT;
     RETURN { content: (SELECT VALUE content FROM ONLY $doc), version: $head };`,
    { id, content, format, owner: ownerId, author, n },
  ));
  assertStored(stored?.content, content);
  return { document: id, status: 'restored' as const, restored_from: n, version: stored!.version };
}
