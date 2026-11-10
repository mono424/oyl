import { createContext, createSignal, onCleanup, Show, useContext, type ParentProps } from 'solid-js';
import { Portal } from 'solid-js/web';

/**
 * Page controls outside the page: in the app's top bar (in place of the logo,
 * over the page, or next to the account menu), and at the top of the recent
 * sessions column. The layout renders the slots; a page fills them with
 * <HeaderTitle>, <HeaderActions>, <HeaderEndActions> and <SideActions>, which
 * disappear with the page. <Focus> hides the sidebars.
 */
type Slot = {
  el: () => HTMLElement | undefined;
  set: (el: HTMLElement | undefined) => void;
  /** Whether a page fills the slot. */
  used: () => boolean;
  use: (delta: number) => void;
};
type Slots = { title: Slot; header: Slot; end: Slot; side: Slot; focus: Slot };
const SlotContext = createContext<Slots>();

function createSlot(): Slot {
  const [el, set] = createSignal<HTMLElement>();
  const [count, setCount] = createSignal(0);
  return { el, set, used: () => count() > 0, use: (delta) => setCount((n) => n + delta) };
}

export function HeaderSlotProvider(props: ParentProps) {
  return (
    <SlotContext.Provider value={{ title: createSlot(), header: createSlot(), end: createSlot(), side: createSlot(), focus: createSlot() }}>
      {props.children}
    </SlotContext.Provider>
  );
}

/** Whether the current page shows its own title in place of the logo. */
export function useHeaderTitleUsed() {
  const slots = useContext(SlotContext);
  return () => slots?.title.used() ?? false;
}

/** Whether the current page takes the whole window, without the sidebars. */
export function useFocused() {
  const slots = useContext(SlotContext);
  return () => slots?.focus.used() ?? false;
}

/** Hides both sidebars while the page shows: its own title and way back stand in the top bar. */
export function Focus() {
  const slot = useContext(SlotContext)?.focus;
  slot?.use(1);
  onCleanup(() => slot?.use(-1));
  return null;
}

function Outlet(props: { slot?: Slot; class?: string }) {
  onCleanup(() => props.slot?.set(undefined));
  return <div ref={(el) => props.slot?.set(el)} class={props.class} />;
}

function Fill(props: ParentProps<{ slot?: Slot }>) {
  const slot = props.slot;
  slot?.use(1);
  onCleanup(() => slot?.use(-1));
  return (
    <Show when={props.slot?.el()}>
      {(mount) => (
        // The portal's wrapper must not break the slot's flex row.
        <Portal mount={mount()} ref={(el) => (el.style.display = 'contents')}>
          {props.children}
        </Portal>
      )}
    </Show>
  );
}

export function HeaderSlot(props: { class?: string }) {
  return <Outlet slot={useContext(SlotContext)?.header} class={props.class} />;
}

export function HeaderActions(props: ParentProps) {
  return <Fill slot={useContext(SlotContext)?.header}>{props.children}</Fill>;
}

/** Where the logo sits: a page's own title replaces it. */
export function HeaderTitleSlot(props: { class?: string }) {
  return <Outlet slot={useContext(SlotContext)?.title} class={props.class} />;
}

export function HeaderTitle(props: ParentProps) {
  return <Fill slot={useContext(SlotContext)?.title}>{props.children}</Fill>;
}

/** Just before the account menu. */
export function HeaderEndSlot(props: { class?: string }) {
  return <Outlet slot={useContext(SlotContext)?.end} class={props.class} />;
}

export function HeaderEndActions(props: ParentProps) {
  return <Fill slot={useContext(SlotContext)?.end}>{props.children}</Fill>;
}

/** Top of the recent sessions column, shown on wide windows only. */
export function SideSlot(props: { class?: string }) {
  return <Outlet slot={useContext(SlotContext)?.side} class={props.class} />;
}

export function SideActions(props: ParentProps) {
  return <Fill slot={useContext(SlotContext)?.side}>{props.children}</Fill>;
}
