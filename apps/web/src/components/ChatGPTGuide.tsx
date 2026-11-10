import { useLocation } from '@solidjs/router';
import { createContext, createEffect, createSignal, For, onCleanup, Show, useContext, type JSX } from 'solid-js';
import { useDb, useQuery } from '@spooky-sync/client-solid';
import { ArrowUpRight, Check, Copy } from 'lucide-solid';
import type { schema } from '../schema.gen';
import { MCP_URL } from '../lib/api';
import { useAuth } from '../lib/auth';
import { qApiKeys } from '../lib/query';
import { Sheet } from './Sheet';
import { Button, Dot } from './ui';

const PLUGINS_URL = 'https://chatgpt.com/plugins';

/** What ChatGPT's "Add custom MCP server" form asks for. */
const PLUGIN_NAME = 'TalkToBolo';
const PLUGIN_DESCRIPTION = 'Study from my TalkToBolo courses: lectures, progress and past study sessions.';

/** What to say to ChatGPT once TalkToBolo is connected. */
export const VOICE_PROMPTS = [
  'What should I study today on TalkToBolo?',
  'Continue my lecture where I left off.',
  'Quiz me on what I have covered so far.',
  'Save this session to TalkToBolo.',
];

interface GuideContext {
  open: () => void;
  /** An unrevoked connection exists. A ChatGPT connection is an `api_key` row. */
  connected: () => boolean;
}

const Ctx = createContext<GuideContext>();

export const useChatGPTGuide = () => {
  const ctx = useContext(Ctx);
  if (!ctx) throw new Error('useChatGPTGuide outside ChatGPTGuideProvider');
  return ctx;
};

/**
 * The step by step guide to connecting ChatGPT, as a dialog any page can open.
 * It opens by itself once per account and browser until ChatGPT is connected.
 */
export function ChatGPTGuideProvider(props: { children: JSX.Element }) {
  const db = useDb<typeof schema>();
  const auth = useAuth();
  const location = useLocation();
  const keys = useQuery(() => (auth.userId() ? qApiKeys(db, auth.userId()!) : null));
  const connected = () => (keys.data() ?? []).some((k) => !k.revoked_at);
  const [isOpen, setOpen] = createSignal(false);

  const seenKey = () => `talktobolo:chatgpt-guide-seen:${auth.userId()}`;
  const seen = () => {
    try {
      return localStorage.getItem(seenKey()) === '1';
    } catch {
      return true;
    }
  };
  const markSeen = () => {
    try {
      localStorage.setItem(seenKey(), '1');
    } catch {
      // Private mode: the guide may open again next time, which is fine.
    }
  };

  // Wait a moment before opening by itself: on a new device the connections
  // arrive from the server shortly after the local store answers empty. Never
  // over the OAuth approval page, which is part of connecting.
  createEffect(() => {
    if (!keys.data() || connected() || isOpen() || seen() || location.pathname === '/connect') return;
    const timer = setTimeout(() => {
      if (!connected() && !seen() && location.pathname !== '/connect') setOpen(true);
    }, 1500);
    onCleanup(() => clearTimeout(timer));
  });

  const close = () => {
    markSeen();
    setOpen(false);
  };

  return (
    <Ctx.Provider value={{ open: () => setOpen(true), connected }}>
      {props.children}
      <Show when={isOpen()}>
        <GuideDialog connected={connected()} onClose={close} />
      </Show>
    </Ctx.Provider>
  );
}

function GuideDialog(props: { connected: boolean; onClose: () => void }) {
  const [urlCopied, setUrlCopied] = createSignal(false);

  const openChatGPT = () => {
    // Open during the click so browsers do not block the new tab while the
    // asynchronous clipboard operation is pending. The address is public.
    window.open(PLUGINS_URL, '_blank', 'noopener,noreferrer');
    void navigator.clipboard.writeText(MCP_URL).then(() => setUrlCopied(true)).catch(() => setUrlCopied(false));
  };

  return (
    <Sheet
      title="Connect TalkToBolo to ChatGPT"
      subtitle="About two minutes, once, in a browser. Then study your courses by voice in ChatGPT, on your phone too."
      icon={<img src="/studycall-logo.svg" alt="" class="h-11 w-9" />}
      onClose={props.onClose}
      footer={(dismiss) => (
        <>
          <div class="flex flex-col-reverse gap-2 sm:flex-row sm:items-center sm:justify-end">
            <Show
              when={!props.connected}
              fallback={
                <button
                  type="button"
                  class="cta-glow inline-flex h-14 w-full items-center justify-center rounded-full text-[16px] font-semibold sm:h-12 sm:w-auto sm:px-8"
                  onClick={dismiss}
                >
                  Done
                </button>
              }
            >
              <Button variant="ghost" class="h-12 sm:h-12" onClick={dismiss}>
                Not now
              </Button>
              <button
                type="button"
                class="cta-glow inline-flex h-14 w-full items-center gap-3 rounded-full pl-6 pr-2 text-[16px] font-semibold sm:h-12 sm:w-auto sm:pl-5 sm:text-[15px]"
                onClick={openChatGPT}
              >
                <span class="flex-1 text-left">Open ChatGPT Plugins</span>
                <span class="cta-arrow inline-flex h-10 w-10 items-center justify-center rounded-full bg-solid-fg/15 sm:h-8 sm:w-8">
                  <ArrowUpRight size={18} stroke-width={2.25} />
                </span>
              </button>
            </Show>
          </div>
          <Show when={urlCopied() && !props.connected}>
            <p class="mt-2.5 flex items-center justify-center gap-1.5 text-[13px] text-muted sm:justify-end" role="status">
              <Check size={15} class="text-green" /> Server address copied. Paste it in step 3.
            </p>
          </Show>
        </>
      )}
    >
      {() => (
        <>
          <div
            class="flex items-center gap-2.5 rounded-2xl border border-[var(--glass-line)] bg-[var(--glass-fill)] px-3.5 py-2.5 text-[14px]"
            role="status"
          >
            <Show
              when={props.connected}
              fallback={
                <>
                  <Dot tone="running" />
                  <span class="text-muted">Not connected yet. This updates by itself once you approve access.</span>
                </>
              }
            >
              <Dot tone="ready" />
              <span class="font-medium">ChatGPT is connected. Jump to the last step to start studying.</span>
            </Show>
          </div>

          <ol class="mt-6 space-y-6">
            <Step n={1} title="Open ChatGPT Plugins" done={props.connected}>
              <p>Sign in to ChatGPT if it asks. The button below also copies the server address you need in step 3.</p>
            </Step>

            <Step n={2} title="Add a custom MCP server" done={props.connected}>
              <p>
                Choose the <Kbd>+</Kbd> button, then <Kbd>Add custom MCP server</Kbd>.
              </p>
            </Step>

            <Step n={3} title="Fill in the form" done={props.connected}>
              <div class="mt-1 space-y-2.5">
                <CopyField label="Name" text={PLUGIN_NAME} glass />
                <CopyField label="Description" text={PLUGIN_DESCRIPTION} glass />
                <CopyField label="Server URL" text={MCP_URL} mono glass />
                <div class="flex items-center justify-between gap-3 rounded-2xl border border-[var(--glass-line)] px-3.5 py-2.5">
                  <span class="text-[13px] text-muted">Authentication</span>
                  <span class="text-[14px] font-medium text-fg">OAuth</span>
                </div>
              </div>
            </Step>

            <Step n={4} title="Create the plugin" done={props.connected}>
              <p>
                Read the warning and choose <Kbd>I understand and want to continue</Kbd>, then <Kbd>Create as a plugin</Kbd>.
              </p>
            </Step>

            <Step n={5} title="Install and approve access" done={props.connected}>
              <p>
                Install the plugin. ChatGPT sends you to TalkToBolo: sign in with this account and choose <Kbd>Allow</Kbd>. No API
                key to copy.
              </p>
            </Step>

            <Step n={6} title="Start studying">
              <p>
                Open a new chat and type <Kbd>@TalkToBolo</Kbd>, or open the ChatGPT app on your phone, start Voice and ask for your
                course. Try saying:
              </p>
              <ul class="mt-3 flex flex-col gap-2" aria-label="Things to say">
                <For each={VOICE_PROMPTS}>
                  {(prompt) => (
                    <li class="rounded-2xl bg-[var(--glass-fill)] px-3.5 py-2.5 text-[14px] text-fg [overflow-wrap:anywhere]">
                      "{prompt}"
                    </li>
                  )}
                </For>
              </ul>
            </Step>
          </ol>

          <p class="mt-6 text-[13px] leading-relaxed text-muted">
            Custom MCP servers depend on your ChatGPT plan, and a workspace admin may need to allow them. You can reopen this guide
            any time from the ChatGPT button at the top.
          </p>
        </>
      )}
    </Sheet>
  );
}

function Step(props: { n: number; title: string; done?: boolean; children: JSX.Element }) {
  return (
    <li class="flex gap-3.5">
      <span
        class={`flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-[14px] font-semibold ${
          props.done ? 'bg-soft text-green' : 'bg-solid text-solid-fg'
        }`}
        aria-hidden="true"
      >
        {props.done ? <Check size={16} stroke-width={2.25} /> : props.n}
      </span>
      <div class="min-w-0 flex-1 pt-1">
        <h3 class="text-[16px] font-semibold">{props.title}</h3>
        <div class="mt-1 text-[14px] leading-relaxed text-muted">{props.children}</div>
      </div>
    </li>
  );
}

/** A label from ChatGPT's interface. */
function Kbd(props: { children: JSX.Element }) {
  return <span class="whitespace-nowrap rounded-md bg-soft px-1.5 py-0.5 font-medium text-fg">{props.children}</span>;
}

/** A labelled value with a copy button. */
export function CopyField(props: { label?: string; text: string; mono?: boolean; glass?: boolean }) {
  const [copied, setCopied] = createSignal(false);
  return (
    <div
      class={`flex w-full min-w-0 items-center gap-2 rounded-2xl py-2 pl-3.5 pr-1.5 ${
        props.glass ? 'border border-[var(--glass-line)] bg-[var(--glass-fill)]' : 'bg-soft'
      }`}
    >
      <div class="min-w-0 flex-1">
        <Show when={props.label}>
          <span class="block text-[12px] text-muted">{props.label}</span>
        </Show>
        <span class={`block break-all text-fg ${props.mono ? 'font-mono text-[12px] leading-relaxed' : 'text-[14px]'}`}>{props.text}</span>
      </div>
      <button
        type="button"
        class={`inline-flex h-11 w-11 shrink-0 items-center justify-center rounded-full text-muted hover:text-fg sm:h-9 sm:w-9 ${
          props.glass ? 'hover:bg-[var(--glass-fill-strong)]' : 'hover:bg-surface'
        }`}
        aria-label={copied() ? 'Copied' : `Copy ${props.label ?? 'to clipboard'}`}
        title={copied() ? 'Copied' : 'Copy'}
        onClick={async () => {
          await navigator.clipboard.writeText(props.text);
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        }}
      >
        <Show when={copied()} fallback={<Copy size={16} stroke-width={1.75} />}>
          <Check size={17} stroke-width={1.75} class="text-green" />
        </Show>
      </button>
    </div>
  );
}
