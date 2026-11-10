/**
 * Gives the search chunks of PDFs indexed before chunks carried page numbers
 * their `page` / `page_end`, so the tutor, search and MCP can cite pages.
 *
 * Pages are read from the `<!-- page N -->` markers in the converted text,
 * exactly as ingestion now does (shared/lecture.ts chunkPages). Only chunks
 * of the current index revision without a page are touched, so the script is
 * idempotent: run it until it reports nothing left. It never reindexes, which
 * would reset every student's lecture progress.
 *
 * Usage (from the repo root), with the Worker's database credentials:
 *   SURREAL_URL=... SURREAL_NS=... SURREAL_DB=... SURREAL_USER=... \
 *   SURREAL_PASS=... pnpm backfill:pages
 */
import type { Env } from '../apps/api/src/env';
import { Surreal } from '../apps/api/src/surreal';
import { chunkMarkdown, chunkPages } from '../shared/lecture';

const need = (name: string): string => {
  const v = process.env[name]?.trim();
  if (!v) {
    console.error(`Missing ${name}.`);
    process.exit(1);
  }
  return v;
};

const env = {
  SURREAL_URL: need('SURREAL_URL'),
  SURREAL_NS: need('SURREAL_NS'),
  SURREAL_DB: need('SURREAL_DB'),
  SURREAL_USER: need('SURREAL_USER'),
  SURREAL_PASS: need('SURREAL_PASS'),
  SURREAL_AUTH_NS: process.env.SURREAL_AUTH_NS?.trim() || undefined,
  SURREAL_AUTH_DB: process.env.SURREAL_AUTH_DB?.trim() || undefined,
} as Env;
const db = Surreal.system(env);

// Materials one at a time: converted lectures can be large.
const ids = await db.query<string[]>(
  `SELECT VALUE id FROM material WHERE kind = 'pdf' AND status = 'ready'
     AND count(SELECT id FROM chunk WHERE material = $parent.id AND rev = $parent.index_rev AND page = NONE) > 0`,
);
console.log(`${ids.length} PDF material(s) with chunks lacking pages.`);

let updated = 0;
for (const id of ids) {
  const m = await db.query<{ content: string; index_rev: number } | null>(
    'SELECT content, index_rev FROM ONLY type::record($id)',
    { id },
  );
  if (!m) continue;
  const chunks = chunkMarkdown(m.content);
  const rows = chunkPages(m.content, chunks)
    .map((p, position) => (p ? { position, page: p.page, page_end: p.pageEnd } : null))
    .filter((r) => r !== null);
  // 200 positions per request keeps each request small.
  for (let i = 0; i < rows.length; i += 200) {
    await db.query(
      `FOR $r IN $rows {
         UPDATE chunk SET page = $r.page, page_end = $r.page_end
         WHERE material = type::record($m) AND rev = $rev AND position = $r.position AND page = NONE;
       };`,
      { m: id, rev: m.index_rev, rows: rows.slice(i, i + 200) },
    );
  }
  updated++;
  console.log(`${id}: ${rows.length} of ${chunks.length} chunks have pages.`);
}
console.log(`Done: ${updated} material(s) updated.`);
