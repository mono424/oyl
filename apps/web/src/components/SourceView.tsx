import { createEffect, on, onCleanup, onMount } from 'solid-js';
import { basicSetup, EditorView } from 'codemirror';
import { EditorState, type Extension } from '@codemirror/state';
import { HighlightStyle, StreamLanguage, syntaxHighlighting, type StringStream } from '@codemirror/language';
import { stex } from '@codemirror/legacy-modes/mode/stex';
import { tags } from '@lezer/highlight';
import { sourceLanguage } from '../lib/cloud-papers';

/**
 * A file of a synced paper, read-only: the source as the LaTeX editor shows
 * it (CodeMirror, the same card and colors), for LaTeX, Typst or plain text.
 * Loaded on demand, with CodeMirror.
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

function languageOf(path: string): Extension[] {
  const lang = sourceLanguage(path);
  return lang === 'latex' ? [StreamLanguage.define(stex)] : lang === 'typst' ? [typst] : [];
}

export default function SourceView(props: { text: string; path: string }) {
  let host!: HTMLDivElement;

  const state = (text: string, path: string) =>
    EditorState.create({
      doc: text,
      extensions: [
        basicSetup,
        ...languageOf(path),
        syntaxHighlighting(highlight),
        EditorView.lineWrapping,
        EditorState.readOnly.of(true),
        EditorView.editable.of(false),
        EditorView.contentAttributes.of({ 'aria-label': `${path}, read-only` }),
      ],
    });

  onMount(() => {
    const view = new EditorView({ parent: host, state: state(props.text, props.path) });
    createEffect(on(() => [props.text, props.path] as const, ([text, path]) => view.setState(state(text, path)), { defer: true }));
    onCleanup(() => view.destroy());
  });

  return <div ref={host} class="tex-editor h-[60dvh] min-w-0 sm:h-[70vh]" />;
}
