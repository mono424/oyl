import assert from 'node:assert/strict';
import { test } from 'node:test';
import { bibtexEntry } from '../../../shared/references.ts';
import { imagesInside, imagesOutside } from '../src/answers.ts';
import { updateDocument, writeDocument } from '../src/documents.ts';
import { writeAnswers } from '../src/homework.ts';
import type { Surreal } from '../src/surreal.ts';

const plain = { external: (md: string) => md, internal: (md: string) => md };

/** A document or answer in a database whose UPDATE answers as `stored` says. */
function fakeUser(content: string, stored: (wanted: string) => string[]) {
  const doc = { id: 'document:d1', title: 'T', course: 'course:c1', format: 'tex', content };
  const part = { id: 'answer:a1', key: '1', question: '1', label: '', prompt: '', content };
  return {
    queryAll: async (sql: string, vars: Record<string, string>) => {
      if (sql.includes('FROM ONLY type::record($id);') && sql.includes('FROM paper')) return [doc, []];
      if (sql.includes('FROM answer')) return [{ id: 'homework:h1', title: 'H', course: 'course:c1' }, [part]];
      return [stored(vars.c0)];
    },
    query: async (_sql: string, vars: { content?: string; set?: { content?: string } }) => stored(vars.content ?? vars.set?.content ?? ''),
  } as unknown as Surreal;
}

// What a `@crdt` field's VALUE guard does: the row comes back with the old text.
const keepsOldValue = () => ['old'];
const storesIt = (wanted: string) => [wanted];

test('a LaTeX image goes out as its address and comes back as its app reference', async () => {
  const origin = 'https://api.example';
  const text = { external: (md: string) => imagesOutside(md, origin), internal: (md: string) => imagesInside(md, origin) };
  const inside = String.raw`\includegraphics[width=\linewidth]{answer-image:ka} and ![](answer-image:kb)`;
  const outside = String.raw`\includegraphics[width=\linewidth]{https://api.example/answer-images/ka} and ![](https://api.example/answer-images/kb)`;
  assert.equal(text.external(inside), outside);
  assert.equal(text.internal(outside), inside);

  const tex = String.raw`\includegraphics{https://api.example/answer-images/ka}`;
  const saved: string[] = [];
  await writeDocument(fakeUser('old', (wanted) => (saved.push(wanted), [wanted])), text, 'document:d1', tex);
  assert.deepEqual(saved, [String.raw`\includegraphics{answer-image:ka}`]);
});

test('write_document reports saved only when the text was stored', async () => {
  const result = await writeDocument(fakeUser('old', storesIt), plain, 'document:d1', 'new');
  assert.equal(result.status, 'saved');
  await assert.rejects(writeDocument(fakeUser('old', keepsOldValue), plain, 'document:d1', 'new'), /not stored/);
});

test('update_document fails when new text was not stored, not for a title alone', async () => {
  await assert.rejects(updateDocument(fakeUser('old', keepsOldValue), plain, 'document:d1', { content: 'new' }), /not stored/);
  const renamed = await updateDocument(fakeUser('old', keepsOldValue), plain, 'document:d1', { title: 'New title' });
  assert.deepEqual(renamed.updated, ['title']);
});

test('write_answers reports saved only when the text was stored', async () => {
  const saved = await writeAnswers(fakeUser('old', storesIt), plain, 'homework:h1', [{ part: '1', content: 'new' }]);
  assert.equal(saved.results[0].status, 'saved');
  await assert.rejects(writeAnswers(fakeUser('old', keepsOldValue), plain, 'homework:h1', [{ part: '1', content: 'new' }]), /not stored/);
});

test('write_answers still reports a conflict when the student changed the text', async () => {
  const result = await writeAnswers(fakeUser('old', () => []), plain, 'homework:h1', [{ part: '1', content: 'new' }]);
  assert.equal(result.results[0].status, 'conflict');
});

test('bibtex: "et al." becomes one "others", as a separate author or at the end of one', () => {
  const author = (authors: string) => bibtexEntry({ key: 'k', title: 'T', authors }).match(/author = \{(.*)\}/)?.[1];
  assert.equal(author('Genitsaridi, I.; Tomic, D.; et al.'), 'Genitsaridi, I. and Tomic, D. and others');
  assert.equal(author('Stark et al.'), 'Stark and others');
  assert.equal(author('Padmanabhan, S.; Prakash, J.'), 'Padmanabhan, S. and Prakash, J.');
});

test('bibtex: LaTeX special characters are escaped once, the url stays as it is', () => {
  const entry = bibtexEntry({
    key: 'k',
    title: '50% of H_2O & more',
    venue: 'Lancet Diabetes & Endocrinology, already \\& escaped',
    url: 'https://x.org/a_b%20c',
  });
  assert.match(entry, /50\\% of H\\_2O \\& more/);
  assert.match(entry, /journal = \{Lancet Diabetes \\& Endocrinology, already \\& escaped\}/);
  assert.match(entry, /url = \{https:\/\/x\.org\/a_b%20c\}/);
});
