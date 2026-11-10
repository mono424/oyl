import { createEffect, on, onCleanup, onMount } from 'solid-js';
import { basicSetup, EditorView } from 'codemirror';
import { Annotation, EditorState, type Extension, Transaction } from '@codemirror/state';
import { HighlightStyle, StreamLanguage, syntaxHighlighting, type StringStream } from '@codemirror/language';
import { stex } from '@codemirror/legacy-modes/mode/stex';
import { tags } from '@lezer/highlight';
import { type CiteStyle, type Paper, texCitations, updatePapers } from '../lib/citations';
import { sourceLanguage } from '../lib/cloud-papers';
import { textEdits } from '../lib/text-edits';
import { LIVE_FIELD, type LoroDoc } from '../lib/paper-live';

/**
 * A file of a synced paper in the LaTeX editor's card and colors
 * (TexDocumentEditor.tsx): LaTeX, Typst or plain text. A live file is bound
 * to its Loro document (lib/paper-live.ts): every keystroke goes into it, and
 * what the Mac or another browser writes comes in where it changed, so the
 * cursor and selection stay where they are. LaTeX offers the paper's
 * bibliography inside `\cite{`. Any other text (an older revision) is
 * read-only. Loaded on demand, with CodeMirror. Mount it per file.
 */

const highlight = HighlightStyle.define([
  { tag: [tags.keyword, tags.tagName], color: 'var(--accent)' },
  { tag: [tags.bracket, tags.squareBracket], color: 'var(--muted)' },
  { tag: tags.comment, color: 'var(--faint)', fontStyle: 'italic' },
  { tag: [tags.string, tags.atom, tags.number, tags.labelName], color: 'var(--muted)' },
  { tag: tags.link, color: 'var(--accent)' },
  { tag: tags.heading, color: 'var(--fg)', fontWeight: '600' },
  { tag: tags.variableName, color: 'var(--fg)' },
]);

interface TypstState {
  math: boolean;
  raw: boolean;
  comment: boolean;
}

/** Enough of Typst to read it: comments, `#` code, headings, math, strings, raw blocks, references and labels. */
const typst = StreamLanguage.define<TypstState>({
  name: 'typst',
  startState: () => ({ math: false, raw: false, comment: false }),
  token(stream: StringStream, state: TypstState) {
    if (state.comment) {
      if (stream.skipTo('*/')) {
        stream.pos += 2;
        state.comment = false;
      } else stream.skipToEnd();
      return 'comment';
    }
    if (state.raw) {
      if (stream.skipTo('```')) {
        stream.pos += 3;
        state.raw = false;
      } else stream.skipToEnd();
      return 'string';
    }
    if (stream.match('//')) {
      stream.skipToEnd();
      return 'comment';
    }
    if (stream.match('/*')) {
      state.comment = true;
      return 'comment';
    }
    if (stream.match('```')) {
      state.raw = true;
      return 'string';
    }
    if (stream.match('$')) {
      state.math = !state.math;
      return 'bracket';
    }
    if (state.math) {
      if (stream.match(/^[A-Za-z]+/)) return 'atom';
      stream.next();
      return null;
    }
    if (stream.sol() && stream.match(/^\s*=+\s.*/)) return 'heading';
    if (stream.match(/^#[A-Za-z_][\w-]*/)) return 'keyword';
    if (stream.match(/^"(?:[^"\\]|\\.)*"?/)) return 'string';
    if (stream.match(/^`[^`]*`?/)) return 'string';
    if (stream.match(/^@[\w:.-]*\w/)) return 'link';
    if (stream.match(/^<[\w:.-]+>/)) return 'labelName';
    if (stream.match(/^\d+(?:\.\d+)?(?:pt|mm|cm|in|em|fr|deg|rad|%)?/)) return 'number';
    stream.next();
    return null;
  },
});

/** Changes that came from the document (other writers), not typed here. */
const synced = Annotation.define<boolean>();
/** The origin of this editor's commits to the document. */
export const EDITOR_ORIGIN = 'oyl-editor';

export default function PaperEditor(props: {
  path: string;
  /** A live file: its Loro document, kept in step with everyone writing it. */
  live?: LoroDoc;
  /** Otherwise the text shown, read-only. */
  text?: string;
  /** The paper's bibliography, offered inside `\cite{` (LaTeX, live files). */
  papers?: () => readonly Paper[];
  citeStyle?: () => CiteStyle;
  /** Something was typed. */
  onEdit?: () => void;
  label: string;
}) {
  let host!: HTMLDivElement;

  onMount(() => {
    const lang = sourceLanguage(props.path);
    const doc = props.live;
    const text = doc?.getText(LIVE_FIELD);

    const extensions: Extension[] = [
      basicSetup,
      // Lines end at \n only, as in the document: a \r stays a character of
      // its line, so positions here and in the document are the same.
      EditorState.lineSeparator.of('\n'),
      lang === 'latex' ? StreamLanguage.define(stex) : lang === 'typst' ? typst : [],
      lang === 'latex' && doc ? texCitations() : [],
      syntaxHighlighting(highlight),
      EditorView.lineWrapping,
      EditorView.contentAttributes.of({ 'aria-label': doc ? props.label : `${props.label}, read-only`, spellcheck: doc ? 'true' : 'false' }),
      doc && text
        ? EditorView.updateListener.of((u) => {
            if (!u.docChanged || u.transactions.every((t) => t.annotation(synced))) return;
            // Into the document at once, change by change, so it always holds what is shown.
            let shift = 0;
            u.changes.iterChanges((fromA, toA, _fromB, _toB, inserted) => {
              const at = fromA + shift;
              if (toA > fromA) text.delete(at, toA - fromA);
              const insert = inserted.toString();
              if (insert) text.insert(at, insert);
              shift += insert.length - (toA - fromA);
            });
            doc.commit({ origin: EDITOR_ORIGIN });
            props.onEdit?.();
          })
        : [EditorState.readOnly.of(true), EditorView.editable.of(false)],
    ];
    const view = new EditorView({ parent: host, state: EditorState.create({ doc: text ? text.toString() : (props.text ?? ''), extensions }) });

    if (doc && text) {
      // What others wrote (or the page, restoring a revision), put in where it
      // changed, so a cursor elsewhere stays put. The document holds everything
      // typed here already, so the difference is theirs.
      const reconcile = () => {
        if (view.composing) return void setTimeout(reconcile, 50);
        const next = text.toString();
        const shown = view.state.doc.toString();
        if (next !== shown) view.dispatch({ changes: textEdits(shown, next), annotations: [synced.of(true), Transaction.addToHistory.of(false)] });
      };
      const unsubscribe = doc.subscribe((batch) => {
        if (batch.origin !== EDITOR_ORIGIN) queueMicrotask(reconcile);
      });
      onCleanup(unsubscribe);
      if (lang === 'latex' && props.papers) {
        createEffect(() => updatePapers(view, props.papers!(), props.citeStyle?.() ?? 'numeric'));
      }
    } else {
      createEffect(on(() => props.text ?? '', (next) => view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: next } }), { defer: true }));
    }
    onCleanup(() => view.destroy());
  });

  return <div ref={host} class="tex-editor flat h-full min-h-0 min-w-0" />;
}
