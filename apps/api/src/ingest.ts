import { WorkflowEntrypoint, type WorkflowEvent, type WorkflowStep } from '@spooky-sync/backend';
import type { Env } from './env';
import { chunkMarkdown, chunkPages, embedText, lectureMarkdown } from './chunk';
import { extractPageImages, loadPdf, pageCount, pagesToMarkdown, type PageImageRef } from './pdf';
import { buildMissingGraphs, embed, ensureServerSchema, type MaterialRow } from './service';
import { Surreal, UserError, toBase64 } from './surreal';
import { getMemoryDocument, indexMemorySection, memoryConfigured, sectionGraph, type SectionGraph } from './memory';
import { mapConcepts } from './understanding';

export interface IngestParams {
  materialId: string;
  /** The material's `index_rev` when this run was started. */
  rev: number;
  /** Turn the uploaded PDF into Markdown first (a new upload), or only re-index the current Markdown (an edit). */
  convert: boolean;
  /**
   * Only build the course knowledge graph of a ready material, for its current
   * text: no new revision, so the search index and lecture progress stay.
   */
  memoryOnly?: boolean;
}

const PAGES_PER_IMAGE_STEP = 20;
const PAGES_PER_TEXT_STEP = 50;
const CHUNKS_PER_EMBED_STEP = 48;
const EMBED_BATCH = 16;
/** Sections uploaded, checked or read back per Agent Memory step. */
const MEMORY_BATCH = 10;
/** Status rounds 15 s apart before extraction counts as stuck (an hour). */
const MEMORY_ROUNDS = 240;

const RETRY = { retries: { limit: 3, delay: '10 seconds', backoff: 'exponential' }, timeout: '15 minutes' } as const;
/** Agent Memory refuses bursts for a while: back off for up to half an hour. */
const MEMORY_RETRY = { retries: { limit: 6, delay: '30 seconds', backoff: 'exponential' }, timeout: '15 minutes' } as const;

/** Step result when a newer upload or edit has taken over this material. */
const STOP = '__superseded__' as const;
type Stop = typeof STOP;

/**
 * Turns a material into something the tutor can teach from, durably, one
 * retryable step at a time:
 *
 *   PDF:  extract images -> read the pages' text layer as markdown -> save
 *   all:  chunk the markdown -> embed with Workers AI -> write the search index
 *
 * The material is ready then, and a memory-only run (`memoryOnly`) follows
 * for the rest: the course knowledge graph in Agent Memory, when configured,
 * and its concepts in the course's concept map (the graph view). Agent
 * Memory can take long or be over quota, so it never holds up or fails the
 * material: it stays ready, without a graph (`memory_rev` 0).
 *
 * Progress is written to `material.status_detail`, which the web app shows
 * live through sync. A newer upload or edit bumps `index_rev`; this run then
 * stops at its next step.
 */
export class IngestMaterial extends WorkflowEntrypoint<Env, IngestParams> {
  async run(event: WorkflowEvent<IngestParams>, step: WorkflowStep) {
    const { materialId, rev, convert, memoryOnly = false } = event.payload;
    const db = Surreal.system(this.env);

    // The material, or null once a newer run owns it.
    const current = async (): Promise<MaterialRow | null> => {
      const m = await db.query<MaterialRow | null>('SELECT * FROM ONLY type::record($id)', { id: materialId });
      return m && m.index_rev === rev ? m : null;
    };
    const progress = (detail: string) =>
      db.query('UPDATE type::record($id) SET status_detail = $detail WHERE index_rev = $rev', { id: materialId, detail, rev });
    const stopped = { superseded: true };

    try {
      const meta = await step.do('load', RETRY, async () => {
        await ensureServerSchema(this.env);
        const m = await current();
        if (!m || (memoryOnly && m.status !== 'ready')) return STOP;
        return { kind: m.kind, owner: m.owner, course: m.course };
      });
      if (meta === STOP) return stopped;

      if (meta.kind === 'pdf' && convert && !memoryOnly) {
        const pages = await step.do('count pages', RETRY, async (): Promise<number | Stop> => {
          if (!(await current())) return STOP;
          const n = await pageCount(await loadPdf(this.env, materialId));
          await db.query('UPDATE type::record($id) SET page_count = $n; DELETE asset WHERE material = type::record($id);', {
            id: materialId,
            n,
          });
          return n;
        });
        if (pages === STOP) return stopped;

        for (let from = 1; from <= pages; from += PAGES_PER_IMAGE_STEP) {
          const to = Math.min(pages, from + PAGES_PER_IMAGE_STEP - 1);
          const r = await step.do(`images ${from}-${to}`, RETRY, async (): Promise<number | Stop> => {
            if (!(await current())) return STOP;
            await progress(`Extracting images, pages ${from}-${to} of ${pages}`);
            const images = await extractPageImages(await loadPdf(this.env, materialId), from, to);
            // Extraction takes a while: make sure no newer upload took over meanwhile.
            if (!(await current())) return STOP;
            // A retried step must not duplicate its images.
            await db.query('DELETE asset WHERE material = type::record($m) AND page >= $from AND page <= $to', {
              m: materialId,
              from,
              to,
            });
            for (const im of images) {
              // The PNG goes to the bucket first: a row never points at a
              // missing file (a file without a row is overwritten on retry).
              await db.query(
                `type::file('materials', '/assets/' + <string>record::id(type::record($id))).put(encoding::base64::decode($b64));
                 CREATE type::record($id) CONTENT {
                   owner: type::record($owner), material: type::record($m), page: $page,
                   mime: 'image/png', width: $w, height: $h, hash: $hash
                 };`,
                {
                  id: `asset:a${crypto.randomUUID().replaceAll('-', '')}`,
                  owner: meta.owner,
                  m: materialId,
                  page: im.page,
                  w: im.width,
                  h: im.height,
                  hash: im.hash,
                  b64: toBase64(im.png),
                },
              );
            }
            return images.length;
          });
          if (r === STOP) return stopped;
        }

        const images = await step.do('drop repeated images', RETRY, async () => {
          // A picture on three or more pages is a logo or decoration, not content.
          await db.query(
            `LET $per_page = SELECT hash, page FROM asset WHERE material = type::record($m) GROUP BY hash, page;
             LET $repeated = (SELECT hash, count() AS n FROM $per_page GROUP BY hash).filter(|$r| $r.n >= 3).hash;
             DELETE asset WHERE material = type::record($m) AND hash IN $repeated;`,
            { m: materialId },
          );
          return db.query<PageImageRef[]>(
            'SELECT record::id(id) AS id, page, width, height FROM asset WHERE material = type::record($m) ORDER BY page',
            { m: materialId },
          );
        });

        const parts: string[] = [];
        for (let from = 1; from <= pages; from += PAGES_PER_TEXT_STEP) {
          const to = Math.min(pages, from + PAGES_PER_TEXT_STEP - 1);
          const md = await step.do(`text ${from}-${to}`, RETRY, async (): Promise<string | Stop> => {
            if (!(await current())) return STOP;
            await progress(`Reading pages ${from}-${to} of ${pages}`);
            const pageImages = images.filter((im) => im.page >= from && im.page <= to);
            return pagesToMarkdown(await loadPdf(this.env, materialId), from, to, pageImages);
          });
          if (md === STOP) return stopped;
          parts.push(md);
        }

        const saved = await step.do('save markdown', RETRY, async (): Promise<true | Stop> => {
          if (!(await current())) return STOP;
          await db.query('UPDATE type::record($id) SET content = $content, image_count = $n', {
            id: materialId,
            content: parts.join('\n\n'),
            n: images.length,
          });
          return true;
        });
        if (saved === STOP) return stopped;
      }

      const total = await step.do('chunk', RETRY, async (): Promise<number | Stop> => {
        const m = await current();
        if (!m) return STOP;
        if (!memoryOnly) await progress('Building the search index');
        return chunkMarkdown(lectureMarkdown(m.kind, m.content)).length;
      });
      if (total === STOP) return stopped;

      for (let from = 0; !memoryOnly && from < total; from += CHUNKS_PER_EMBED_STEP) {
        const to = Math.min(total, from + CHUNKS_PER_EMBED_STEP);
        const r = await step.do(`embed ${from}-${to}`, RETRY, async (): Promise<true | Stop> => {
          const m = await current();
          if (!m) return STOP;
          await progress(`Indexing sections ${from + 1}-${to} of ${total}`);
          const text = lectureMarkdown(m.kind, m.content);
          const all = chunkMarkdown(text);
          const pages = chunkPages(text, all).slice(from, to);
          const chunks = all.slice(from, to);
          for (let i = 0; i < chunks.length; i += EMBED_BATCH) {
            const batch = chunks.slice(i, i + EMBED_BATCH);
            const vectors = await embed(this.env, batch.map(embedText), 'documents');
            await db.query(
              `FOR $c IN $rows {
                 UPSERT type::record($c.id) MERGE {
                   owner: type::record($owner), course: type::record($course), material: type::record($m),
                   rev: $rev, position: $c.position, heading: $c.heading, text: $c.text, embedding: $c.embedding,
                   page: $c.page, page_end: $c.page_end
                 };
               };`,
              {
                owner: meta.owner,
                course: meta.course,
                m: materialId,
                rev,
                rows: batch.map((c, j) => ({
                  id: `chunk:${materialId.split(':')[1]}_${rev}_${from + i + j}`,
                  position: from + i + j,
                  heading: c.heading,
                  text: c.text,
                  embedding: vectors[j],
                  page: pages[i + j]?.page,
                  page_end: pages[i + j]?.pageEnd,
                })),
              },
            );
          }
          return true;
        });
        if (r === STOP) return stopped;
      }

      // Each section is its own document with its own known-time stamp: one
      // document for a whole lecture would make its later concepts visible at
      // the first section. All sections are uploaded first, so Agent Memory
      // extracts them side by side, then read back once all are ready.
      const graphs: SectionGraph[] = [];
      let withMemory = memoryOnly && memoryConfigured(this.env);
      if (withMemory) {
        try {
          const ids: string[] = [];
          for (let from = 0; from < total; from += MEMORY_BATCH) {
            const to = Math.min(total, from + MEMORY_BATCH);
            const batch = await step.do(`memory upload ${from}-${to}`, MEMORY_RETRY, async (): Promise<string[] | Stop> => {
              const m = await current();
              if (!m) return STOP;
              await progress(`Building the knowledge graph, sections ${from + 1}-${to} of ${total}`);
              const chunks = chunkMarkdown(lectureMarkdown(m.kind, m.content));
              const out: string[] = [];
              for (let position = from; position < to; position++) {
                const id = await indexMemorySection(this.env, m, position, chunks[position]);
                if (!id) throw new Error('Agent Memory was disabled during ingestion.');
                out.push(id);
              }
              return out;
            });
            if (batch === STOP) return stopped;
            ids.push(...batch);
          }

          let pending = ids.map((_, position) => position);
          for (let round = 0; pending.length; round++) {
            if (round >= MEMORY_ROUNDS) throw new Error('Agent Memory extraction did not finish in time.');
            if (round) await step.sleep(`memory wait ${round}`, '15 seconds');
            const waiting = pending;
            const left = await step.do(`memory status ${round}`, MEMORY_RETRY, async (): Promise<number[] | Stop> => {
              if (!(await current())) return STOP;
              await progress(`Building the knowledge graph, ${total - waiting.length} of ${total} sections read`);
              const still: number[] = [];
              for (let i = 0; i < waiting.length; i += MEMORY_BATCH) {
                const states = await Promise.all(waiting.slice(i, i + MEMORY_BATCH).map((p) => getMemoryDocument(this.env, ids[p])));
                states.forEach((state, j) => {
                  const position = waiting[i + j];
                  if (state?.status === 'failed') throw new Error(`Agent Memory extraction failed for section ${position}.`);
                  if (state?.status !== 'ready') still.push(position);
                });
              }
              return still;
            });
            if (left === STOP) return stopped;
            pending = left;
          }

          for (let from = 0; from < total; from += MEMORY_BATCH) {
            const to = Math.min(total, from + MEMORY_BATCH);
            const batch = await step.do(`memory concepts ${from}-${to}`, MEMORY_RETRY, async (): Promise<SectionGraph[] | Stop> => {
              const m = await current();
              if (!m) return STOP;
              const chunks = chunkMarkdown(lectureMarkdown(m.kind, m.content));
              const out: SectionGraph[] = [];
              for (let position = from; position < to; position++) out.push(await sectionGraph(this.env, m, position, chunks[position]));
              return out;
            });
            if (batch === STOP) return stopped;
            graphs.push(...batch);
          }
        } catch (err) {
          // The lecture is still searchable and studyable without its graph.
          console.error(`[ingest] ${materialId} knowledge graph failed`, err);
          withMemory = false;
          graphs.length = 0;
        }
      }

      await step.do('finish', RETRY, async () => {
        if (memoryOnly) {
          await db.query(
            `UPDATE type::record($m) SET status_detail = NONE, memory_rev = IF $with_memory THEN $rev ELSE memory_rev END
               WHERE index_rev = $rev AND status = 'ready'`,
            { m: materialId, rev, with_memory: withMemory },
          );
          return;
        }
        // Only while this run still owns the material: a newer run's chunks
        // must survive an older run finishing late.
        await db.query(
          `BEGIN TRANSACTION;
           IF (SELECT VALUE index_rev FROM ONLY type::record($m)) = $rev {
             DELETE chunk WHERE material = type::record($m) AND rev != $rev;
             UPDATE type::record($m) SET status = 'ready', status_detail = NONE, error = NONE, chunk_count = $n,
               memory_rev = IF $with_memory THEN $rev ELSE 0 END;
           };
           COMMIT TRANSACTION;`,
          { m: materialId, rev, n: total, with_memory: withMemory },
        );
      });

      // The concepts each section names, and how they link, become the
      // course's concept map, which ChatGPT's record of the student builds on.
      if (graphs.some((g) => g.concepts.length)) {
        try {
          await step.do('concept map', RETRY, async () => {
            if (!(await current())) return 0;
            return mapConcepts(this.env, meta.owner, meta.course, materialId, graphs);
          });
        } catch (err) {
          console.error(`[ingest] ${materialId} concept map failed`, err);
        }
      }
      // A fresh material's graph comes next, in its own run.
      await this.nextGraph(step, event.instanceId, memoryOnly ? null : meta);
      return { ok: true, chunks: total, ...(memoryOnly ? { memoryOnly: true } : {}) };
    } catch (err) {
      if (memoryOnly) {
        // The material itself is fine: only its graph was being built.
        console.error(`[ingest] ${materialId} knowledge graph failed`, err);
        await step.do('clear progress', async () => {
          await db.query('UPDATE type::record($m) SET status_detail = NONE WHERE index_rev = $rev AND status = \'ready\'', { m: materialId, rev });
        });
        await this.nextGraph(step, event.instanceId);
        return { failed: 'knowledge graph' };
      }
      const message = err instanceof UserError ? err.message : 'Processing failed. Upload the file again to retry.';
      console.error(`[ingest] ${materialId} failed`, err);
      await step.do('mark failed', async () => {
        await db.query(
          `UPDATE type::record($m) SET status = 'failed', status_detail = NONE, error = $error WHERE index_rev = $rev`,
          { m: materialId, rev, error: message.slice(0, 500) },
        );
      });
      return { failed: message };
    }
  }

  /**
   * A graph build hands its place to the next lecture still without a graph;
   * a fresh material puts its own course first.
   */
  private async nextGraph(step: WorkflowStep, self: string, fresh: { owner: string; course: string } | null = null) {
    try {
      await step.do('next graph build', RETRY, () => buildMissingGraphs(this.env, fresh?.owner ?? null, fresh?.course ?? null, self));
    } catch (err) {
      console.error('[ingest] next graph build', err);
    }
  }
}
