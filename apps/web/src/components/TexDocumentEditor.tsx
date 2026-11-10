import { createEffect, on, onCleanup, onMount } from 'solid-js';
import { basicSetup, EditorView } from 'codemirror';
import { Annotation, EditorSelection, EditorState, Transaction } from '@codemirror/state';
import { Decoration } from '@codemirror/view';
import { HighlightStyle, StreamLanguage, syntaxHighlighting } from '@codemirror/language';
import { stex } from '@codemirror/legacy-modes/mode/stex';
import { tags } from '@lezer/highlight';
import { texCitations } from '../lib/citations';
import { replacedStretch } from '../lib/drafts';
import { addImagesAt, guardFileDrops, imagesOf, isFileDrag, uploading, uploadsField } from '../lib/editor-images';
import { isImage } from '../lib/images';

/**
 * The editor of a LaTeX document (routes/Document.tsx): the source with
 * syntax highlighting and `\cite{` offering the document's papers. Its
 * rendering is a tab of the panel beside it. The text is the
 * page's, saved as you type: it follows changes from elsewhere (another
 * device, ChatGPT). Images come in pasted, dropped where they go, or picked
 * with the page's button. Loaded on demand, with CodeMirror.
 */

const highlight = HighlightStyle.define([
  { tag: [tags.keyword, tags.tagName], color: 'var(--blue)' },
  { tag: [tags.bracket, tags.squareBracket], color: 'var(--muted)' },
  { tag: tags.comment, color: 'var(--faint)', fontStyle: 'italic' },
  { tag: [tags.string, tags.atom], color: 'var(--green)' },
  { tag: [tags.number, tags.special(tags.string)], color: '#b86c68' },
  { tag: tags.variableName, color: 'var(--fg)' },
]);

/** Changes made by the app (text from elsewhere), not typed. */
const synced = Annotation.define<boolean>();

/** Where an image goes: outside the document's body it would never show. */
function inBody(text: string, pos: number) {
  const begin = /\\begin\{document\}(\s*\\maketitle\b)?/.exec(text);
  if (begin && pos < begin.index + begin[0].length) return begin.index + begin[0].length;
  const end = text.indexOf('\\end{document}');
  return end >= 0 && pos > end ? end : pos;
}

export default function TexDocumentEditor(props: {
  content: () => string;
  onInput: (text: string) => void;
  onBlur: () => void;
  onView?: (view: EditorView) => void;
  /** Uploads a pasted or dropped image, resolving to its LaTeX. */
  uploadImage: (file: File) => Promise<string | null>;
  /** Once made: adds images at the cursor, as if pasted (the page's Add image button). */
  onAddImages?: (add: (files: File[]) => void) => void;
  label: string;
}) {
  let host!: HTMLDivElement;

  onMount(() => {
    let alive = true;
    const add = (v: EditorView, pos: number, files: File[]) => addImagesAt(v, inBody(v.state.doc.toString(), pos), files, props.uploadImage, () => alive);
    const view: EditorView = new EditorView({
      parent: host,
      state: EditorState.create({
        doc: props.content(),
        extensions: [
          basicSetup,
          StreamLanguage.define(stex),
          syntaxHighlighting(highlight),
          EditorView.lineWrapping,
          texCitations(),
          uploadsField,
          EditorView.decorations.compute([uploadsField], (state) => Decoration.set(state.field(uploadsField).map((u) => uploading.range(u.pos)), true)),
          EditorView.domEventHandlers({
            paste(e, v) {
              const files = imagesOf(e.clipboardData);
              if (!files.length) return false;
              e.preventDefault();
              add(v, v.state.selection.main.head, files);
              return true;
            },
            dragover(e) {
              if (!isFileDrag(e)) return false;
              e.preventDefault();
              if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
              return false;
            },
            drop(e, v) {
              const files = imagesOf(e.dataTransfer);
              if (!files.length) return false;
              e.preventDefault();
              const pos = v.posAtCoords({ x: e.clientX, y: e.clientY }) ?? v.state.selection.main.head;
              // The image goes where it was dropped, and the writer with it.
              v.focus();
              v.dispatch({ selection: EditorSelection.cursor(pos), userEvent: 'select' });
              add(v, pos, files);
              return true;
            },
          }),
          EditorView.contentAttributes.of({ 'aria-label': props.label, spellcheck: 'true' }),
          EditorView.updateListener.of((u) => {
            if (u.docChanged && !u.transactions.every((t) => t.annotation(synced))) props.onInput(u.state.doc.toString());
            if (u.focusChanged && !u.view.hasFocus) {
              props.onBlur();
              queueMicrotask(reconcile);
            }
          }),
        ],
      }),
    });

    // While something is being written `content` is that text, so this only
    // ever brings in a change from elsewhere, also with the cursor in the editor:
    // left out, the next keystroke would save the old text over that change.
    const reconcile = () => {
      if (!alive || view.composing) return;
      const text = props.content();
      const doc = view.state.doc.toString();
      if (doc === text) return;
      view.dispatch({ changes: replacedStretch(doc, text), annotations: [synced.of(true), Transaction.addToHistory.of(false)] });
    };
    createEffect(on(props.content, () => queueMicrotask(reconcile), { defer: true }));

    props.onView?.(view);
    props.onAddImages?.((files) => {
      const images = files.filter(isImage);
      if (!images.length || !alive) return;
      const pos = inBody(view.state.doc.toString(), view.state.selection.main.head);
      view.focus();
      view.dispatch({ selection: EditorSelection.cursor(pos), effects: EditorView.scrollIntoView(pos, { y: 'center' }) });
      add(view, pos, images);
    });

    const unguard = guardFileDrops(view);
    onCleanup(() => {
      alive = false;
      unguard();
      view.destroy();
    });
  });

  return <div ref={host} class="tex-editor flat h-full min-h-0 min-w-0" />;
}
