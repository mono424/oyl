import type { Db } from './query';
import { newId } from './ids';

/**
 * Background work for the API (apps/api/openapi.yml), written as rows of the
 * `job` outbox. sp00ky's job runner calls the API with the payload, retries
 * failures and records the outcome on the row, which syncs back like any
 * other. The outbox permission only accepts a job about the student's own
 * material (or source), with that record as both `assigned_to` and `payload.id`.
 * Not `db.run`: it stores the payload as a JSON string the permission cannot
 * read.
 */
async function enqueue(
  db: Db,
  path: string,
  target: string,
  payload: Record<string, unknown> = {},
  opts: { maxRetries?: number } = {},
): Promise<string> {
  const id = newId('job');
  await db.create(id, {
    path,
    assigned_to: target,
    payload: { ...payload, id: target },
    // Explicit, as `db.run` does: the server's DEFAULT does not apply to the
    // optimistic local row.
    status: 'pending',
    max_retries: opts.maxRetries ?? 3,
    retry_strategy: 'linear',
  });
  return id;
}

export const PROGRESS_JOB = '/jobs/materials/progress';

/** (Re)builds the search index of a text or markdown material after an edit. */
export const reindexMaterial = (db: Db, materialId: string) => enqueue(db, '/jobs/materials/index', materialId);

/** Marks the last reached lecture section without claiming it is understood. */
export const saveLectureProgress = (db: Db, materialId: string, input: { through: number; rev: number }) =>
  enqueue(db, PROGRESS_JOB, materialId, input, { maxRetries: 1 });

/** Checks a source now and imports what is new or changed. */
export const checkSource = (db: Db, fetcherId: string) => enqueue(db, '/jobs/fetchers/run', fetcherId, {}, { maxRetries: 1 });
