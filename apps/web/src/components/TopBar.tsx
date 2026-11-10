import { A, useLocation } from '@solidjs/router';
import { createSignal, onCleanup, onMount, Show } from 'solid-js';
import { LogOut, Menu, MessageCircle, X } from 'lucide-solid';
import { useAuth } from '../lib/auth';
import { useChatGPTGuide } from './ChatGPTGuide';
import { HeaderEndSlot, HeaderSlot, HeaderTitleSlot, useFocused, useHeaderTitleUsed } from './HeaderSlot';

/** Bolo, the approved TalkToBolo mark. */
export function Logo(props: { class?: string }) {
  return (
    <svg viewBox="60 36 210 258" class={`shrink-0 ${props.class ?? 'h-8 w-10'}`} aria-hidden="true">
      <image href="/studycall-logo.svg" x="60" y="36" width="210" height="258" />
    </svg>
  );
}

/** ChatGPT's mark, in the current color. */
function ChatGPTMark(props: { size: number; class?: string }) {
  return (
    <svg viewBox="0 0 24 24" width={props.size} height={props.size} fill="currentColor" class={`shrink-0 ${props.class ?? ''}`} aria-hidden="true">
      <path d="M22.282 9.821a5.985 5.985 0 0 0-.516-4.911 6.046 6.046 0 0 0-6.51-2.9A6.065 6.065 0 0 0 4.981 4.182a5.985 5.985 0 0 0-3.998 2.9 6.046 6.046 0 0 0 .743 7.097 5.98 5.98 0 0 0 .51 4.911 6.051 6.051 0 0 0 6.515 2.9A5.985 5.985 0 0 0 13.26 24a6.056 6.056 0 0 0 5.772-4.206 5.99 5.99 0 0 0 3.997-2.9 6.056 6.056 0 0 0-.747-7.073zM13.26 22.43a4.476 4.476 0 0 1-2.876-1.04l.141-.081 4.779-2.758a.795.795 0 0 0 .392-.681v-6.737l2.02 1.168a.071.071 0 0 1 .038.052v5.583a4.504 4.504 0 0 1-4.494 4.494zM3.6 18.304a4.47 4.47 0 0 1-.535-3.014l.142.085 4.783 2.759a.771.771 0 0 0 .78 0l5.843-3.369v2.332a.08.08 0 0 1-.033.062L9.74 19.95a4.5 4.5 0 0 1-6.14-1.646zM2.34 7.896a4.485 4.485 0 0 1 2.366-1.973V11.6a.766.766 0 0 0 .388.676l5.815 3.355-2.02 1.168a.076.076 0 0 1-.071 0l-4.83-2.786A4.504 4.504 0 0 1 2.34 7.872zm16.597 3.855-5.833-3.387L15.119 7.2a.076.076 0 0 1 .071 0l4.83 2.791a4.494 4.494 0 0 1-.676 8.105v-5.678a.79.79 0 0 0-.407-.667zm2.01-3.023-.141-.085-4.774-2.782a.776.776 0 0 0-.785 0L9.409 9.23V6.897a.066.066 0 0 1 .028-.061l4.83-2.787a4.5 4.5 0 0 1 6.68 4.66zm-12.64 4.135-2.02-1.164a.08.08 0 0 1-.038-.057V6.075a4.5 4.5 0 0 1 7.375-3.453l-.142.08L8.704 5.46a.795.795 0 0 0-.393.681zm1.097-2.365 2.602-1.5 2.607 1.5v2.999l-2.597 1.5-2.607-1.5z" />
    </svg>
  );
}

export function TopBar(props: { onMenu: () => void; menuOpen: boolean }) {
  const auth = useAuth();
  const guide = useChatGPTGuide();
  const [menu, setMenu] = createSignal(false);
  const location = useLocation();
  // An editor (a document, homework) keeps the screen to itself.
  const inEditor = () => /^\/courses\/[^/]+\/(documents|homework)\//.test(location.pathname);
  const titled = useHeaderTitleUsed();
  const focused = useFocused();
  const initial = () => auth.user()?.name?.trim().charAt(0).toUpperCase() || '?';

  // How much of the bar's right end the account menu takes: page controls in
  // the slot keep clear of it (.header-column in global.css).
  let header!: HTMLElement;
  let account!: HTMLDivElement;
  onMount(() => {
    const measure = () =>
      header.style.setProperty('--account-w', `${header.getBoundingClientRect().right - account.getBoundingClientRect().left}px`);
    const observer = new ResizeObserver(measure);
    observer.observe(header);
    observer.observe(account);
    onCleanup(() => observer.disconnect());
  });

  return (
    <>
      <header ref={header} class="relative flex h-16 shrink-0 items-center gap-2 px-3 sm:px-5">
        {/* A page in focus has no sidebar to open: its own way back stands here. */}
        <Show when={!focused()}>
          <button
            type="button"
            class="inline-flex h-11 w-11 items-center justify-center rounded-full text-muted hover:bg-soft hover:text-fg md:hidden"
            aria-label={props.menuOpen ? 'Close navigation' : 'Open navigation'}
            aria-expanded={props.menuOpen}
            aria-controls="app-navigation"
            onClick={props.onMenu}
          >
            {props.menuOpen ? <X size={20} stroke-width={1.75} /> : <Menu size={20} stroke-width={1.75} />}
          </button>
        </Show>
        <Show when={!titled()}>
          <A href="/" class="mr-1 inline-flex shrink-0 items-center text-fg sm:mr-3" aria-label="TalkToBolo home">
            <Logo />
          </A>
        </Show>
        {/* A page's own title, in place of the logo: within the sidebar's width beside it, unless the sidebar is hidden. */}
        <HeaderTitleSlot class={`flex min-w-0 items-center empty:hidden ${focused() ? '' : 'md:max-w-[252px]'}`} />

        {/* Beside the sidebar, the slot spans the page below it, so pages can line controls up with their content.
            Wide windows end it at the recent sessions column, clear of the account menu above it. */}
        <HeaderSlot class="flex min-w-0 flex-1 items-center gap-2 md:pointer-events-none md:absolute md:inset-y-0 md:left-[288px] md:right-0 xl:right-[288px] xl:[--account-w:0px]" />

        <div ref={account} class="relative ml-auto flex shrink-0 items-center gap-2">
          <HeaderEndSlot class="flex items-center gap-1 empty:hidden" />
        </div>
      </header>

      {/* The account, at the screen's lower left on every page but the editors, its menu opening upwards. */}
      <Show when={!inEditor()}>
        <div class="fixed bottom-[max(1rem,env(safe-area-inset-bottom))] left-4 z-30 sm:bottom-5 sm:left-5">
          <button
            type="button"
            onClick={() => setMenu(!menu())}
            class="inline-flex h-11 w-11 items-center justify-center rounded-full border border-line bg-surface shadow-card transition-colors hover:bg-soft"
            aria-label="Account menu"
            aria-haspopup="menu"
            aria-expanded={menu()}
          >
            <span class="relative inline-flex h-8 w-8 items-center justify-center rounded-full bg-solid text-[13px] font-semibold text-solid-fg">
              {initial()}
              <Show when={guide.connected()}>
                <span class="absolute -bottom-1 -right-1 grid h-[18px] w-[18px] place-items-center rounded-full bg-surface text-fg ring-2 ring-soft" title="ChatGPT connected">
                  <ChatGPTMark size={11} />
                </span>
              </Show>
            </span>
          </button>
          <Show when={menu()}>
            <div class="fixed inset-0 z-40" onClick={() => setMenu(false)} />
            <div role="menu" class="absolute bottom-full left-0 z-50 mb-2 w-60 rounded-2xl border border-line bg-surface p-1.5 shadow-card">
              <div class="px-3 py-2">
                <p class="truncate text-[14px] font-medium">{auth.user()?.name ?? 'Account'}</p>
                <p class="truncate text-[13px] text-muted">{auth.user()?.email}</p>
              </div>
              {/* The ChatGPT guide: to connect, or once connected, how to study there. */}
              <button
                type="button"
                role="menuitem"
                class="flex h-11 w-full items-center gap-2.5 rounded-xl px-3 text-[15px] hover:bg-soft md:h-10"
                onClick={() => {
                  setMenu(false);
                  guide.open();
                }}
              >
                <ChatGPTMark size={16} class="text-muted" /> {guide.connected() ? 'How to study in ChatGPT' : 'Connect ChatGPT'}
              </button>
              <A
                href="/settings"
                role="menuitem"
                class="flex h-11 w-full items-center gap-2.5 rounded-xl px-3 text-[15px] hover:bg-soft md:h-10"
                onClick={() => setMenu(false)}
              >
                <MessageCircle size={17} stroke-width={1.75} class="text-muted" /> ChatGPT settings
              </A>
              <button
                type="button"
                role="menuitem"
                class="flex h-11 w-full items-center gap-2.5 rounded-xl px-3 text-[15px] hover:bg-soft md:h-10"
                onClick={() => auth.signOut()}
              >
                <LogOut size={17} stroke-width={1.75} class="text-muted" /> Sign out
              </button>
            </div>
          </Show>
        </div>
      </Show>
    </>
  );
}
