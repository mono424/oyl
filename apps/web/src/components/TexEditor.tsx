import { createSignal, onCleanup, onMount, Show } from 'solid-js';
import { basicSetup, EditorView } from 'codemirror';
import { EditorState } from '@codemirror/state';
import { HighlightStyle, StreamLanguage, syntaxHighlighting } from '@codemirror/language';
import { stex } from '@codemirror/legacy-modes/mode/stex';
import { tags } from '@lezer/highlight';
import { latexToMarkdown } from '../../../../shared/latex';
import { renderMarkdown } from '../lib/markdown';
import { Segmented } from './ui';

/**
 * The editor of a LaTeX material: the source with syntax highlighting on one
 * side, the rendered document (as the lecture page shows it) on the other,
 * updated as you type. On phones the two are tabs. Loaded on demand, with
 * CodeMirror, so the rest of the app stays light.
 */

const highlight = HighlightStyle.define([
  { tag: [tags.keyword, tags.tagName], color: 'var(--blue)' },
  { tag: [tags.bracket, tags.squareBracket], color: 'var(--muted)' },
  { tag: tags.comment, color: 'var(--faint)', fontStyle: 'italic' },
  { tag: [tags.string, tags.atom], color: 'var(--green)' },
  { tag: [tags.number, tags.special(tags.string)], color: '#b86c68' },
  { tag: tags.variableName, color: 'var(--fg)' },
]);

const PREVIEW_DELAY_MS = 250;

export default function TexEditor(props: { value: string; onChange: (value: string) => void }) {
  let host!: HTMLDivElement;
  const render = (tex: string) => renderMarkdown(latexToMarkdown(tex, { colors: true }));
  const [preview, setPreview] = createSignal(render(props.value));
  const [tab, setTab] = createSignal<'source' | 'preview'>('source');
  let timer: ReturnType<typeof setTimeout> | undefined;

  onMount(() => {
    const view = new EditorView({
      parent: host,
      state: EditorState.create({
        doc: props.value,
        extensions: [
          basicSetup,
          StreamLanguage.define(stex),
          syntaxHighlighting(highlight),
          EditorView.lineWrapping,
          EditorView.contentAttributes.of({ 'aria-label': 'LaTeX source', spellcheck: 'true' }),
          EditorView.updateListener.of((u) => {
            if (!u.docChanged) return;
            const text = u.state.doc.toString();
            props.onChange(text);
            clearTimeout(timer);
            timer = setTimeout(() => setPreview(render(text)), PREVIEW_DELAY_MS);
          }),
        ],
      }),
    });
    onCleanup(() => {
      clearTimeout(timer);
      view.destroy();
    });
  });

  return (
    <div class="space-y-3">
      <div class="lg:hidden">
        <Segmented
          label="Editor view"
          value={tab()}
          options={[
            { value: 'source', label: 'Source' },
            { value: 'preview', label: 'Preview' },
          ]}
          onChange={setTab}
        />
      </div>
      <div class="grid min-w-0 gap-4 lg:grid-cols-2">
        <div ref={host} class={`tex-editor h-[65dvh] min-w-0 sm:h-[72vh] ${tab() === 'source' ? '' : 'max-lg:hidden'}`} />
        <div
          class={`scroll-quiet h-[65dvh] min-w-0 overflow-y-auto rounded-[14px] border border-line px-5 py-4 sm:h-[72vh] ${tab() === 'preview' ? '' : 'max-lg:hidden'}`}
          aria-label="Preview"
        >
          <Show when={preview().trim()} fallback={<p class="text-[14px] text-muted">The rendered document shows here.</p>}>
            <div class="lecture" innerHTML={preview()} />
          </Show>
        </div>
      </div>
    </div>
  );
}
