import { parseSheet } from '../../../../shared/homework';
import { lectureMarkdown } from '../../../../shared/lecture';
import { newId } from './ids';
import type { Db } from './query';

/**
 * Homework: the student's answers to an exercise sheet, one `answer` row per
 * part of the sheet (shared/homework.ts finds the parts). Created here, in
 * the browser, like any other write; the excerpts of a PDF sheet are located
 * on its pages once (sheet-regions.ts) and saved with each answer (`region`).
 */

/** Starts answering a sheet: the homework and an empty answer per part. */
export async function createHomework(
  db: Db,
  input: { owner: string; course: string; sheet: { id: string; title: string; kind: string; content: string } },
): Promise<string> {
  const parts = parseSheet(lectureMarkdown(input.sheet.kind, input.sheet.content));
  const id = newId('homework');
  await db.create(id, { owner: input.owner, course: input.course, sheet: input.sheet.id, title: input.sheet.title });
  for (const [position, p] of parts.entries()) {
    await db.create(newId('answer'), {
      owner: input.owner,
      homework: id,
      position,
      key: p.key,
      question: p.question,
      label: p.label,
      prompt: p.prompt,
      content: '',
      ...(p.questionTitle ? { question_title: p.questionTitle } : {}),
      ...(p.points !== null ? { points: p.points } : {}),
      ...(p.context ? { context: p.context } : {}),
      ...(p.page ? { page: p.page } : {}),
      ...(p.anchor ? { anchor: p.anchor } : {}),
      ...(p.anchorPage ? { anchor_page: p.anchorPage } : {}),
    });
  }
  return id;
}
