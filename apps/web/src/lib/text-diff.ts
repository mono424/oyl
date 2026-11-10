import { diffLines, diffWordsWithSpace } from 'diff';

/**
 * What changed between two versions of a text, for reading: whole lines where
 * lines came or went, single words inside a line that changed (a paragraph is
 * one long line in Markdown), and only a few lines around each change.
 */

export type Piece = { kind: 'same' | 'added' | 'removed'; text: string } | { kind: 'skip'; lines: number };

/** Lines kept on each side of a change. */
const CONTEXT = 3;
/** Above this, two changed stretches show as removed and added, not word by word. */
const MAX_WORD_DIFF = 40_000;

const lines = (text: string) => text.match(/[^\n]*\n|[^\n]+$/g) ?? [];

export function textChanges(before: string, after: string): Piece[] {
  const parts = diffLines(before, after);
  const pieces: Piece[] = [];
  parts.forEach((part, i) => {
    if (!part.added && !part.removed) {
      const all = lines(part.value);
      const head = i === 0 ? 0 : CONTEXT;
      const tail = i === parts.length - 1 ? 0 : CONTEXT;
      if (all.length <= head + tail + 1) pieces.push({ kind: 'same', text: part.value });
      else {
        if (head) pieces.push({ kind: 'same', text: all.slice(0, head).join('') });
        pieces.push({ kind: 'skip', lines: all.length - head - tail });
        if (tail) pieces.push({ kind: 'same', text: all.slice(-tail).join('') });
      }
    } else if (part.added && parts[i - 1]?.removed && part.value.length + parts[i - 1].value.length <= MAX_WORD_DIFF) {
      // The other half of a changed stretch: the removed part came first, and waits for it here.
      pieces.pop();
      for (const word of diffWordsWithSpace(parts[i - 1].value, part.value)) {
        pieces.push({ kind: word.added ? 'added' : word.removed ? 'removed' : 'same', text: word.value });
      }
    } else {
      pieces.push({ kind: part.added ? 'added' : 'removed', text: part.value });
    }
  });
  return pieces;
}
