import { lectureMarkdown } from '../../../../shared/lecture';
import { draftMarkdown, findReferences, makeCiteKey, matchMaterials } from '../../../../shared/references';
import { ApiError, uploadPdf } from './api';
import { newId, whenSynced } from './ids';
import type { Db } from './query';

/**
 * Documents: free writing in a course (a research proposal, an essay), with
 * the papers it draws on (schema `document`, `paper`). Written here, in the
 * browser, like any other write.
 */

const TEX_START = '\\documentclass{article}\n\\usepackage{natbib}\n\n\\title{}\n\\author{}\n\n\\begin{document}\n\\maketitle\n\n\n\n\\bibliographystyle{plainnat}\n\\bibliography{references}\n\\end{document}\n';

/** A new, empty document: Markdown, or a LaTeX article to fill in. */
export async function createDocument(db: Db, input: { owner: string; course: string; title?: string; format?: 'markdown' | 'tex' }): Promise<string> {
  const id = newId('document');
  const tex = input.format === 'tex';
  await db.create(id, {
    owner: input.owner,
    course: input.course,
    title: input.title?.trim() || 'Untitled document',
    format: tex ? 'tex' : 'markdown',
    content: tex ? TEX_START : '',
  });
  return id;
}

/**
 * Starts a document from a material of the course (a draft uploaded as
 * LaTeX, Markdown or a PDF): its text on the left (LaTeX stays LaTeX source,
 * the rest becomes Markdown), and on the right the papers it mentions
 * ("Stark et al. (2019): [title](link)"), each with a cite key and linked to
 * the course material holding it when there is one.
 */
export async function createDocumentFromMaterial(
  db: Db,
  input: {
    owner: string;
    material: { id: string; course: string; title: string; kind: string; content: string };
    materials: readonly { id: string; title: string }[];
  },
): Promise<string> {
  const tex = input.material.kind === 'tex';
  const markdown = draftMarkdown(lectureMarkdown(input.material.kind, input.material.content));
  const id = newId('document');
  await db.create(id, {
    owner: input.owner,
    course: input.material.course,
    source: input.material.id,
    title: input.material.title,
    format: tex ? 'tex' : 'markdown',
    content: tex ? input.material.content : markdown,
  });
  const refs = findReferences(markdown);
  const keys: string[] = [];
  const matches = matchMaterials(refs, input.materials.filter((m) => m.id !== input.material.id));
  for (const [i, ref] of refs.entries()) {
    const material = matches[i];
    const cite_key = makeCiteKey(ref, keys);
    keys.push(cite_key);
    await db.create(newId('paper'), {
      cite_key,
      owner: input.owner,
      document: id,
      ...(material ? { material: material.id } : {}),
      title: ref.title,
      authors: ref.authors,
      year: ref.year,
      url: ref.url,
    });
  }
  return id;
}

/**
 * Adds a PDF to a course as material: the row at once, the file once the
 * row has synced, then the API processes it. `uploaded` rejects when the
 * request itself fails; a file the API refuses (400) is marked failed on
 * its row instead.
 */
export async function addPdfMaterial(db: Db, input: { owner: string; course: string; file: File }) {
  const title = input.file.name.replace(/\.pdf$/i, '') || 'Paper';
  const id = newId('material');
  await db.create(id, { owner: input.owner, course: input.course, title, kind: 'pdf', content: '', status: 'pending' });
  const uploaded = (async () => {
    await whenSynced(db);
    try {
      await uploadPdf(db, id, input.file);
    } catch (err) {
      if (!(err instanceof ApiError && err.status === 400)) throw err;
    }
  })();
  return { id, title, uploaded };
}
