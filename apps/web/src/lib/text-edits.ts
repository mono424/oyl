import { type ChangeObject, diffChars, diffLines } from 'diff';
import { replacedStretch } from './drafts';

/**
 * A text change as the small edits it is made of, for an editor taking in
 * text written elsewhere (components/PaperEditor.tsx): put in where it
 * changed, a cursor elsewhere stays where it is.
 */

/** Replaces `from` to `to` of a text with `insert`. */
export interface Edit {
  from: number;
  to: number;
  insert: string;
}

/** How long a character diff may take before lines are compared instead, in ms. */
const CHAR_TIMEOUT_MS = 40;
/** And lines, before the whole changed stretch counts as one edit. */
const LINE_TIMEOUT_MS = 80;

/** The edits that turn `before` into `after`: in `before`'s positions, in order, none touching another. */
export function textEdits(before: string, after: string): Edit[] {
  if (before === after) return [];
  // Most changes are one stretch: compare only that.
  const stretch = replacedStretch(before, after);
  const removed = before.slice(stretch.from, stretch.to);
  if (!removed || !stretch.insert) return [stretch];
  const parts =
    diffChars(removed, stretch.insert, { timeout: CHAR_TIMEOUT_MS }) ?? diffLines(removed, stretch.insert, { timeout: LINE_TIMEOUT_MS });
  return parts ? editsOf(parts, stretch.from) : [stretch];
}

function editsOf(parts: ChangeObject<string>[], offset: number): Edit[] {
  const edits: Edit[] = [];
  let at = offset;
  for (const part of parts) {
    const last = edits.at(-1);
    if (part.added) {
      if (last && last.to === at) last.insert += part.value;
      else edits.push({ from: at, to: at, insert: part.value });
    } else if (part.removed) {
      const end = at + part.value.length;
      if (last && last.to === at) last.to = end;
      else edits.push({ from: at, to: end, insert: '' });
      at = end;
    } else {
      at += part.value.length;
    }
  }
  return edits;
}
