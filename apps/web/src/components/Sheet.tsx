import { createSignal, onCleanup, onMount, Show, type JSX } from 'solid-js';
import { X } from 'lucide-solid';

let nextId = 0;

/**
 * A modal glass sheet: a bottom sheet on phones (drag the header down to
 * dismiss), a centered card on wider screens. Mount it to open it; `onClose`
 * runs after the exit animation, then the caller unmounts it. Children and
 * footer receive `close`, which plays that animation.
 */
export function Sheet(props: {
  title: JSX.Element;
  /** Hidden on phones, to keep the bottom sheet header short. */
  subtitle?: JSX.Element;
  /** Shown before the title from `sm` up only, to keep the phone header short. */
  icon?: JSX.Element;
  onClose: () => void;
  children: (close: () => void) => JSX.Element;
  footer?: (close: () => void) => JSX.Element;
  /** Max width of the centered card on wider screens, default 36rem. */
  maxWidth?: string;
}) {
  let dialog!: HTMLDialogElement;
  const titleId = `sheet-title-${++nextId}`;
  const [closing, setClosing] = createSignal(false);

  // Focus the sheet itself, not the close button, so no focus ring shows on open.
  onMount(() => {
    dialog.showModal();
    dialog.focus();
  });

  // Text fields save on change, which fires on blur: blur before closing.
  // Then play the exit animation (global.css, .glass-sheet[data-closing]).
  let closeTimer: ReturnType<typeof setTimeout> | undefined;
  const finish = (ms: number) => {
    (document.activeElement as HTMLElement | null)?.blur();
    setClosing(true);
    closeTimer = setTimeout(props.onClose, ms);
  };
  const close = () => {
    if (!closing()) finish(230);
  };
  onCleanup(() => clearTimeout(closeTimer));

  // Drag the handle or header down to dismiss the bottom sheet on touch.
  let dragStart: { y: number; t: number } | null = null;
  const onDragStart = (e: PointerEvent) => {
    if (e.pointerType === 'mouse' || (e.target as HTMLElement).closest('button')) return;
    dragStart = { y: e.clientY, t: e.timeStamp };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    dialog.style.transition = 'none';
  };
  const onDragMove = (e: PointerEvent) => {
    if (!dragStart) return;
    const dy = e.clientY - dragStart.y;
    // Pulling up resists, pulling down follows the finger.
    dialog.style.transform = `translateY(${dy > 0 ? dy : dy / 6}px)`;
  };
  const onDragEnd = (e: PointerEvent) => {
    if (!dragStart) return;
    const dy = e.clientY - dragStart.y;
    const speed = dy / Math.max(1, e.timeStamp - dragStart.t);
    dragStart = null;
    if (dy > 120 || (dy > 30 && speed > 0.6)) {
      dialog.style.transition = 'transform 0.22s cubic-bezier(0.4, 0, 1, 1)';
      dialog.style.transform = 'translateY(100%)';
      finish(220);
      return;
    }
    dialog.style.transition = 'transform 0.35s cubic-bezier(0.32, 0.72, 0, 1)';
    dialog.style.transform = '';
  };

  return (
    <dialog
      ref={dialog}
      aria-labelledby={titleId}
      tabIndex={-1}
      data-closing={closing() ? '' : undefined}
      class="glass-sheet overflow-hidden p-0 text-fg open:flex open:flex-col"
      style={props.maxWidth ? { '--sheet-max': props.maxWidth } : undefined}
      onCancel={(e) => {
        e.preventDefault();
        close();
      }}
      onClick={(e) => e.target === dialog && close()}
    >
      <div
        class="shrink-0 touch-none select-none sm:touch-auto"
        onPointerDown={onDragStart}
        onPointerMove={onDragMove}
        onPointerUp={onDragEnd}
        onPointerCancel={onDragEnd}
      >
        <div class="flex justify-center pt-2 sm:hidden" aria-hidden="true">
          <span class="h-1 w-9 rounded-full bg-fg/20" />
        </div>
        <div class="flex items-center gap-3 px-5 pb-2 pt-1.5 sm:items-start sm:px-7 sm:pb-4 sm:pt-7">
          <Show when={props.icon}>
            <span class="hidden shrink-0 sm:block">{props.icon}</span>
          </Show>
          <div class="min-w-0 flex-1">
            <h2 id={titleId} class="text-[17px] font-semibold leading-tight tracking-tight sm:text-[22px]">
              {props.title}
            </h2>
            <Show when={props.subtitle}>
              <p class="mt-1 hidden text-[14px] leading-relaxed text-muted sm:block">{props.subtitle}</p>
            </Show>
          </div>
          <button
            type="button"
            class="-mr-1 inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-[var(--glass-fill)] text-muted transition-colors hover:bg-[var(--glass-fill-strong)] hover:text-fg sm:h-9 sm:w-9"
            aria-label="Close"
            onClick={close}
          >
            <X size={16} stroke-width={2.25} />
          </button>
        </div>
      </div>

      <div class="glass-sheet-scroll scroll-quiet min-h-0 flex-1 overflow-y-auto overscroll-contain px-5 pb-6 pt-2 sm:px-7">
        {props.children(close)}
      </div>

      <Show when={props.footer}>
        <div class="glass-sheet-footer shrink-0 px-5 pt-4 pb-5 sm:px-7 sm:pb-6">{props.footer!(close)}</div>
      </Show>
    </dialog>
  );
}
