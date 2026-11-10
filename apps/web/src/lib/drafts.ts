import { RecordId } from '@spooky-sync/client-solid';
import { applyPatch, structuredPatch } from 'diff';
import { createEffect, createSignal, onCleanup } from 'solid-js';
import { idString } from './ids';
import type { Db } from './query';

/**
 * Text saved as you type: what the editor holds is a draft per row until it
 * is written, a moment after the last keystroke, on leaving the editor, or
 * when the page closes. Used for homework answers (a row per part) and for
 * documents (one row).
 *
 * A write can take many seconds to reach the database, and the row receives
 * the server's copies of earlier saves for a long time after, in no reliable
 * order or time. So a draft stays after it is saved, and hides those copies
 * from the editor: only text this page did not write takes its place, which
 * is a change from elsewhere. And only one save is on its way at a time: the
 * next one sends the latest text, so that saves cannot pile up behind a slow
 * one and land long after.
 */

const SAVE_DELAY_MS = 500;
/** Texts remembered per row to recognize the server's copies of them. */
const REMEMBERED = 30;
/** The database's refusal of a save built on text that has changed (`document_stale_save`). */
const STALE_SAVE = 'document_changed_elsewhere';

/** SHA-256 of a text, as the database computes it. */
export async function textHash(text: string) {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
}

/**
 * Clears out what a browser queued before only one save went out at a time:
 * every save held the whole text, and a queue too long to go through before
 * the push deadline was sent again from the start, again and again, putting
 * old text back each time. Only the last save of a row is still needed. The
 * outbox has no API for this, so this reaches into the client. Once per page
 * load, and a failure only leaves the queue as it was.
 */
let swept = false;
async function dropSupersededSaves(db: Db) {
  if (swept) return;
  swept = true;
  try {
    const { local } = (db.getSp00ky() as unknown as { services: { local: { query: (sql: string, vars?: Record<string, unknown>) => Promise<unknown[]> } } }).services;
    const result = await local.query('SELECT * FROM _00_pending_mutations ORDER BY id ASC');
    const rows = (Array.isArray(result[0]) ? result[0] : []) as { id: unknown; mutationType?: string; recordId?: unknown; tableName?: string; data?: Record<string, unknown> }[];
    const saves = rows.filter((r) => r.mutationType === 'update' && (r.tableName === 'document' || r.tableName === 'answer') && Object.keys(r.data ?? {}).join() === 'content');
    const last = new Map(saves.map((r) => [idString(r.recordId), r]));
    for (const row of saves) {
      if (last.get(idString(row.recordId)) === row) continue;
      const key = idString(row.id).replace(/^[^:]*:/, '').replace(/^⟨|⟩$/g, '');
      await local.query('DELETE $mid', { mid: new RecordId('_00_pending_mutations', key) });
    }
  } catch (err) {
    console.warn('Could not shorten the queue of saves', err);
  }
}

/**
 * Of `texts`, oldest first, the one sharing the most with `text` at both ends:
 * what it was most likely built on. A tie goes to the older one, so that a change
 * of this page that the other text also rewrote still counts as a change.
 */
function closest(texts: readonly string[], text: string): string | undefined {
  let best: string | undefined;
  let score = -1;
  for (const candidate of texts) {
    const { from, to } = replacedStretch(candidate, text);
    const shared = from + candidate.length - to;
    if (shared > score) {
      best = candidate;
      score = shared;
    }
  }
  return best;
}

/** `mine`'s changes since `base`, made to `theirs`. Null where both changed the same lines. */
function mergeChanges(base: string, mine: string, theirs: string): string | null {
  const merged = applyPatch(theirs, structuredPatch('', '', base, mine, '', '', { context: 2 }));
  return merged === false ? null : merged;
}

export function createDrafts<R extends { id: unknown; content?: string | null }>(opts: {
  db: Db;
  rows: () => readonly R[];
  /** `parent` is the text this one was built on, when that is known. */
  save: (id: string, value: string, parent?: string) => Promise<unknown>;
  /** The text as the database holds it right now, to merge with after a save was refused. */
  current?: (id: string) => Promise<string>;
  /** The text changed elsewhere in the same place as what is being written, which is kept. */
  onOverlap?: () => void;
}) {
  const { db } = opts;
  void dropSupersededSaves(db);
  const [drafts, setDrafts] = createSignal<Record<string, string>>({});
  const [rejected, setRejected] = createSignal(false);
  const timers = new Map<string, ReturnType<typeof setTimeout>>();
  // Past the delay, waiting for the save before them to get through.
  const waiting = new Set<string>();
  // The text last handed to `save`, per row.
  const sent = new Map<string, string>();
  // The texts this page has written or built on, per row.
  const ours = new Map<string, string[]>();
  // The text a draft started from, until it has been saved.
  const seen = new Map<string, string>();
  // Saves that were refused, while the text that changed is being read.
  const rebasing = new Set<string>();
  // Calls of `save` still writing locally: not in the pending count yet.
  let writing = 0;
  // A save was just refused: which one is not known yet, so nothing goes out.
  let sorting = false;
  const [progress, bump] = createSignal(0);

  const rowOf = (id: string) => opts.rows().find((r) => idString(r.id) === id);
  const text = (row: R) => drafts()[idString(row.id)] ?? row.content ?? '';
  const textOf = (id: string) => {
    const row = rowOf(id);
    return row ? text(row) : '';
  };

  const remember = (id: string, value: string) => {
    const list = ours.get(id) ?? [];
    if (list.includes(value)) return;
    list.push(value);
    if (list.length > REMEMBERED) list.shift();
    ours.set(id, list);
  };

  const release = (id: string) => {
    sent.delete(id);
    ours.delete(id);
    seen.delete(id);
    setDrafts(({ [id]: _released, ...rest }) => rest);
  };

  /**
   * Takes in text from elsewhere, keeping what was written here. It may be built
   * on an older text than the last one saved here, so what this page wrote since
   * then is put back, saved or not.
   */
  const takeIn = (id: string, held: string, settled = false) => {
    const value = drafts()[id];
    if (value === undefined) return;
    const combined = mergeChanges(closest(ours.get(id) ?? [], held) ?? seen.get(id) ?? held, value, held);
    remember(id, held);
    seen.set(id, held);
    sent.delete(id);
    if (combined === held) {
      if (settled) release(id);
      else hold(id, held);
      return;
    }
    if (combined === null) opts.onOverlap?.();
    edit(id, combined ?? value);
  };

  const check = () => {
    if (rejected()) return;
    const busy = writing > 0 || db.pendingMutationCount > 0;
    for (const [id, value] of Object.entries(drafts())) {
      if (rebasing.has(id)) continue;
      const row = rowOf(id);
      const held = row?.content ?? '';
      if (row && (held === value || ours.get(id)?.includes(held))) continue;
      const settled = !busy && !timers.has(id) && !waiting.has(id) && sent.get(id) === value;
      if (row) takeIn(id, held, settled);
      else if (settled) release(id);
    }
  };
  createEffect(() => {
    opts.rows();
    drafts();
    check();
  });

  /** Hands what waits to `save`, once nothing is on its way, or at once when the page is going away. */
  const send = (now = false) => {
    if (!now && (writing > 0 || sorting || db.pendingMutationCount > 0)) return;
    for (const id of [...waiting]) {
      if (rebasing.has(id)) continue;
      waiting.delete(id);
      const value = drafts()[id];
      if (value === undefined || value === sent.get(id)) continue;
      const parent = sent.get(id) ?? seen.get(id);
      sent.set(id, value);
      remember(id, value);
      bump((n) => n + 1);
      writing++;
      void opts
        .save(id, value, parent)
        .catch(() => {
          sent.delete(id);
          setRejected(true);
        })
        .finally(() => {
          writing--;
          bump((n) => n + 1);
          send();
          check();
        });
    }
  };

  /** Reads the text that changed and saves again on top of it. Never saves without having seen it. */
  const rebase = async (id: string) => {
    if (rebasing.has(id) || !opts.current) return;
    rebasing.add(id);
    waiting.add(id);
    bump((n) => n + 1);
    for (let attempt = 0; ; attempt++) {
      try {
        const current = await opts.current(id);
        rebasing.delete(id);
        takeIn(id, current);
        send();
        return;
      } catch {
        await new Promise((resolve) => setTimeout(resolve, Math.min(15_000, 1000 * 2 ** attempt)));
      }
    }
  };

  // A save the database refused is taken back from the row. The text stays in
  // the editor. One built on outdated text is merged and saved again, any other
  // goes out again with the next edit.
  const client = db.getSp00ky();
  let failed = client.failedMutationCount;
  const since = Date.now();
  const refused = async () => {
    let other = false;
    sorting = true;
    for (const row of await client.listFailedMutations()) {
      if (row.failedAt < since) continue;
      if (!row.error.message.includes(STALE_SAVE)) {
        other = true;
        continue;
      }
      void client.discardFailedMutation(row.id);
      void rebase(row.recordId);
    }
    sorting = false;
    if (other) {
      sent.clear();
      setRejected(true);
    }
    send();
    check();
  };
  onCleanup(
    client.subscribeToFailedMutations((n) => {
      if (n > failed) void refused();
      failed = n;
    }),
  );

  onCleanup(
    db.subscribeToPendingMutations(() => {
      // A refusal is announced after the queue empties: the next save must not overtake it.
      if (client.failedMutationCount > failed) sorting = true;
      send();
      check();
    }),
  );

  const enqueue = () => {
    for (const [id, timer] of [...timers]) {
      clearTimeout(timer);
      timers.delete(id);
      if (drafts()[id] !== undefined) waiting.add(id);
    }
  };
  const saveAll = () => {
    enqueue();
    send();
  };
  const saveNow = () => {
    enqueue();
    send(true);
  };

  // A closing tab cannot wait its turn: the outbox keeps these for the next visit.
  const onHidden = () => {
    if (document.visibilityState === 'hidden') saveNow();
  };
  document.addEventListener('visibilitychange', onHidden);
  window.addEventListener('pagehide', saveNow);
  onCleanup(() => {
    document.removeEventListener('visibilitychange', onHidden);
    window.removeEventListener('pagehide', saveNow);
    saveNow();
  });

  const edit = (id: string, value: string) => {
    setRejected(false);
    // What the row holds now is what this edit builds on.
    remember(id, rowOf(id)?.content ?? '');
    if (drafts()[id] === undefined) seen.set(id, rowOf(id)?.content ?? '');
    setDrafts((d) => ({ ...d, [id]: value }));
    clearTimeout(timers.get(id));
    timers.set(
      id,
      setTimeout(() => {
        timers.delete(id);
        waiting.add(id);
        send();
      }, SAVE_DELAY_MS),
    );
  };

  /** Shows text that is already written some other way, until the row has it too. */
  const hold = (id: string, value: string) => {
    clearTimeout(timers.get(id));
    timers.delete(id);
    waiting.delete(id);
    remember(id, rowOf(id)?.content ?? '');
    remember(id, value);
    sent.set(id, value);
    setRejected(false);
    setDrafts((d) => ({ ...d, [id]: value }));
  };

  /** Resolves once everything typed has reached the database. Throws rather than go on without it. */
  const whenSaved = async (timeoutMs = 120_000) => {
    saveAll();
    const deadline = Date.now() + timeoutMs;
    while (waiting.size > 0 || writing > 0 || rebasing.size > 0 || sorting || db.pendingMutationCount > 0) {
      if (Date.now() > deadline) throw new Error('Your latest changes are still being saved. Try again in a moment.');
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  };

  /** Some text has not gone out yet. */
  const unsaved = () => {
    progress();
    return Object.entries(drafts()).some(([id, value]) => timers.has(id) || waiting.has(id) || rebasing.has(id) || sent.get(id) !== value);
  };

  return { text, textOf, edit, hold, saveAll, whenSaved, unsaved, rejected };
}

/** The one stretch to replace to turn `before` into `after`, so that a cursor elsewhere stays where it is. */
export function replacedStretch(before: string, after: string, offset = 0) {
  const max = Math.min(before.length, after.length);
  let start = 0;
  while (start < max && before.charCodeAt(start) === after.charCodeAt(start)) start++;
  let end = 0;
  while (end < max - start && before.charCodeAt(before.length - 1 - end) === after.charCodeAt(after.length - 1 - end)) end++;
  return { from: offset + start, to: offset + before.length - end, insert: after.slice(start, after.length - end) };
}
