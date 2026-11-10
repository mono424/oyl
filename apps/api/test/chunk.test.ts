import assert from 'node:assert/strict';
import { test } from 'node:test';
import { chunkMarkdown, chunkPages } from '../src/chunk.ts';

const para = (word: string, n = 30) => Array.from({ length: n }, (_, i) => `${word}${i}`).join(' ') + '.';

test('chunks of converted PDFs carry the pages they come from', () => {
  const md = [
    '<!-- page 1 -->',
    '## Groups',
    para('alpha'),
    '<!-- page 2 -->',
    para('beta'),
    '![A diagram](asset:a1)',
    '<!-- page 3 -->',
    '## Rings',
    para('gamma'),
  ].join('\n\n');
  const chunks = chunkMarkdown(md);
  const before = JSON.stringify(chunks);
  const pages = chunkPages(md, chunks);
  assert.equal(JSON.stringify(chunks), before, 'chunks are only located, never changed');
  assert.equal(pages.length, chunks.length);
  assert.deepEqual(pages, [
    { page: 1, pageEnd: 2 },
    { page: 3, pageEnd: 3 },
  ]);
});

test('a page marker inside a paragraph and the text-layer fallback both map', () => {
  // The text-layer fallback writes a marker, then the raw page text.
  const md = `<!-- page 4 -->\n\n${para('delta', 300)}\n<!-- page 5 -->\n${para('eps', 300)}`;
  const chunks = chunkMarkdown(md);
  assert.ok(chunks.length >= 3);
  const pages = chunkPages(md, chunks);
  assert.equal(pages[0]?.page, 4);
  assert.equal(pages.at(-1)?.page, 5);
  assert.equal(pages.at(-1)?.pageEnd, 5);
  for (const p of pages) assert.ok(p && p.page <= p.pageEnd);
});

test('repeated text on later pages does not pull chunks back', () => {
  const footer = 'Course footer that repeats on every page of the slides deck here.';
  const md = [1, 2, 3].map((n) => `<!-- page ${n} -->\n\n## Slide ${n}\n\n${footer}\n\n${para(`w${n}x`)}`).join('\n\n');
  const pages = chunkPages(md, chunkMarkdown(md));
  assert.deepEqual(pages.map((p) => p?.page), [1, 2, 3]);
});

test('material without page markers has no pages', () => {
  const md = `# Notes\n\n${para('zeta')}\n\n## More\n\n${para('eta')}`;
  const chunks = chunkMarkdown(md);
  assert.deepEqual(chunkPages(md, chunks), chunks.map(() => null));
});
