import { RecordId } from '@spooky-sync/client-solid';
import type { Db } from './query';

/**
 * A new record id. The key starts with a letter and has no dashes, so it is a
 * plain SurrealQL identifier: the SDK never wraps it in ⟨⟩ and the string form
 * always round-trips.
 */
export const newId = <T extends string>(table: T): `${T}:${string}` =>
  `${table}:k${crypto.randomUUID().replaceAll('-', '')}`;

/** `table:key` for a record id or its string form. */
export const idString = (v: unknown): string =>
  typeof v === 'string' ? v : v instanceof RecordId ? `${v.table.name}:${v.id}` : v ? String(v) : '';

/** The key part of `table:key`, for URLs. */
export const idKey = (v: unknown): string => {
  const s = idString(v);
  return s.slice(s.indexOf(':') + 1);
};

/**
 * Resolves once every local write has reached SurrealDB, so a request to the
 * API that reads those rows sees them. Gives up quietly after `timeoutMs`.
 */
export function whenSynced(db: Db, timeoutMs = 8000): Promise<void> {
  if (db.pendingMutationCount === 0) return Promise.resolve();
  return new Promise((resolve) => {
    const timer = setTimeout(done, timeoutMs);
    const unsub = db.subscribeToPendingMutations((n) => {
      if (n === 0) done();
    });
    function done() {
      clearTimeout(timer);
      unsub();
      resolve();
    }
  });
}
