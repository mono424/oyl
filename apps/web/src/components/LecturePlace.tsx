import { createSignal, For, Show } from 'solid-js';
import { Check, ChevronDown, LoaderCircle } from 'lucide-solid';
import type { Chunk, ChunkPages } from '../../../../shared/lecture';

const excerpt = (text: string, length: number) => {
  const line = text.replace(/\s+/g, ' ').trim();
  return line.length > length ? `${line.slice(0, length).trimEnd()}…` : line;
};

/**
 * How far the student has got in a lecture: a quiet select button whose
 * popover lists the sections. Choosing one saves it; -1 is "Not started".
 */
export function LecturePlace(props: {
  sections: Chunk[];
  pages: (ChunkPages | null)[];
  value: number;
  saving: boolean;
  disabled: boolean;
  error: string | null;
  onChange: (through: number) => void;
  class?: string;
}) {
  const [open, setOpen] = createSignal(false);
  let trigger!: HTMLButtonElement;
  let list: HTMLDivElement | undefined;

  const heading = (i: number) => props.sections[i]?.heading || 'Introduction';
  const label = () => (props.value < 0 ? 'Not started' : `${props.value + 1}. ${heading(props.value)}`);

  const options = () => [...(list?.querySelectorAll<HTMLElement>('[role="option"]') ?? [])];
  const show = () => {
    if (props.disabled) return;
    setOpen(true);
    (list?.querySelector<HTMLElement>('[aria-selected="true"]') ?? options()[0])?.focus();
  };
  const close = () => {
    setOpen(false);
    trigger.focus();
  };
  const choose = (through: number) => {
    close();
    if (through !== props.value) props.onChange(through);
  };
  const onListKey = (e: KeyboardEvent) => {
    const all = options();
    const at = all.indexOf(document.activeElement as HTMLElement);
    const to = { ArrowDown: at + 1, ArrowUp: at - 1, Home: 0, End: all.length - 1 }[e.key];
    if (to !== undefined) {
      e.preventDefault();
      all[Math.max(0, Math.min(all.length - 1, to))]?.focus();
    } else if (e.key === 'Escape' || e.key === 'Tab') {
      e.preventDefault();
      close();
    }
  };

  const Option = (p: { through: number; title: string; detail: string; page?: number }) => (
    <button
      type="button"
      role="option"
      aria-selected={props.value === p.through}
      tabIndex={-1}
      class={`flex w-full items-start gap-2.5 rounded-xl px-3 py-2 text-left transition-colors hover:bg-soft focus:bg-soft focus:outline-none ${
        props.value === p.through ? 'bg-soft' : ''
      }`}
      onClick={() => choose(p.through)}
    >
      <span class="min-w-0 flex-1">
        <span class="flex items-baseline gap-2">
          <span class="min-w-0 flex-1 truncate text-[13px] font-medium text-fg">{p.title}</span>
          <Show when={p.page}>
            <span class="shrink-0 text-[12px] tabular-nums text-faint">p. {p.page}</span>
          </Show>
        </span>
        <span class="mt-0.5 line-clamp-2 block text-[12px] leading-snug text-muted">{p.detail}</span>
      </span>
      <span class="mt-0.5 w-3.5 shrink-0 text-fg">
        <Show when={props.value === p.through}>
          <Check size={14} stroke-width={2} />
        </Show>
      </span>
    </button>
  );

  return (
    <div class={`relative ${props.class ?? ''}`}>
      <button
        ref={trigger}
        type="button"
        class="flex h-11 w-full min-w-0 items-center gap-2 rounded-xl bg-soft px-3 text-left text-[13px] transition-colors hover:bg-hover disabled:pointer-events-none disabled:opacity-40 md:h-9"
        aria-haspopup="listbox"
        aria-expanded={open()}
        aria-busy={props.saving}
        disabled={props.disabled}
        title={props.disabled ? 'Available once the lecture has been indexed again' : 'Where Bolo picks up in this lecture'}
        onClick={() => (open() ? close() : show())}
        onKeyDown={(e) => {
          if (!open() && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
            e.preventDefault();
            show();
          }
        }}
      >
        <span class="shrink-0 text-muted">Reached</span>
        <span class="min-w-0 flex-1 truncate font-medium text-fg">{label()}</span>
        {props.saving ? (
          <LoaderCircle size={14} stroke-width={2} class="shrink-0 animate-spin text-muted" aria-label="Saving" />
        ) : (
          <ChevronDown size={14} stroke-width={2} class={`shrink-0 text-muted transition-transform ${open() ? 'rotate-180' : ''}`} />
        )}
      </button>

      <Show when={props.error}>
        <p class="mt-1.5 break-words text-[12px] text-red" role="alert">
          {props.error}
        </p>
      </Show>

      <Show when={open()}>
        <div class="fixed inset-0 z-40" onClick={() => close()} />
        <div
          ref={list}
          role="listbox"
          aria-label="Reached in lecture"
          class="scroll-quiet absolute inset-x-0 top-full z-50 mt-2 max-h-[min(28rem,60dvh)] overflow-y-auto overscroll-contain rounded-2xl border border-line bg-surface p-1.5 shadow-card"
          onKeyDown={onListKey}
        >
          <p class="px-3 pb-1.5 pt-1 text-[12px] leading-snug text-faint">
            Bolo paces explanations from here. Reached does not mean mastered.
          </p>
          <Option through={-1} title="Not started" detail="Bolo introduces the material from the beginning." />
          <For each={props.sections}>
            {(section, i) => (
              <Option through={i()} title={`${i() + 1}. ${heading(i())}`} detail={excerpt(section.text, 120)} page={props.pages[i()]?.page} />
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
