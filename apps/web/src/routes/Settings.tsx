import { createSignal, For, Show } from 'solid-js';
import { useDb, useQuery } from '@spooky-sync/client-solid';
import { AudioLines, MessageCircle } from 'lucide-solid';
import type { schema } from '../schema.gen';
import { api, MCP_URL } from '../lib/api';
import { useAuth } from '../lib/auth';
import { qApiKeys } from '../lib/query';
import { CopyField, useChatGPTGuide, VOICE_PROMPTS } from '../components/ChatGPTGuide';
import { Button, Card, Dot, formatDate, inputClass } from '../components/ui';

/** Makes a key for the user's own scripts. The API only hands it out this once. */
function NewApiKey() {
  const db = useDb<typeof schema>();
  const [name, setName] = createSignal('');
  const [key, setKey] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const create = async (e: Event) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const made = await api<{ key: string }>(db, '/api/keys', { method: 'POST', body: JSON.stringify({ name: name().trim() }) });
      setKey(made.key);
      setName('');
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Could not create the key.');
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card class="mt-4 p-4 sm:p-6">
      <h2 class="text-[18px] font-semibold tracking-tight">API keys</h2>
      <p class="mt-2 max-w-xl text-[14px] leading-relaxed text-muted">
        For your own scripts and tools. A key reads and writes your courses and documents through the server URL above, until you revoke it under Connections.
      </p>
      <form class="mt-4 flex flex-col gap-3 sm:flex-row" onSubmit={create}>
        <input
          class={`${inputClass} min-w-0 flex-1`}
          placeholder="What is it for?"
          aria-label="Key name"
          maxLength={80}
          value={name()}
          onInput={(e) => setName(e.currentTarget.value)}
        />
        <Button type="submit" disabled={busy() || !name().trim()}>Create key</Button>
      </form>
      <Show when={error()}>
        <p role="alert" class="mt-3 text-[14px] text-muted">{error()}</p>
      </Show>
      <Show when={key()}>
        {(k) => (
          <div class="mt-4">
            <CopyField label="Copy it now, it is shown only once" text={k()} mono />
          </div>
        )}
      </Show>
    </Card>
  );
}

/** Connecting ChatGPT to the MCP server, and the connections made so far. */
export default function SettingsPage() {
  const db = useDb<typeof schema>();
  const auth = useAuth();
  const keys = useQuery(() => (auth.userId() ? qApiKeys(db, auth.userId()!) : null));
  const guide = useChatGPTGuide();

  return (
    <div class="mx-auto w-full min-w-0 max-w-3xl px-4 py-7 sm:px-8 sm:py-14">
      <h1 class="text-[28px] font-semibold leading-tight tracking-tight sm:text-[30px]">ChatGPT</h1>
      <p class="mt-1.5 max-w-2xl text-[16px] leading-relaxed text-muted">
        Study with ChatGPT from your own courses: it reads your lectures, progress and past sessions, quizzes you and saves what
        you covered. It can also create courses, add material and upload PDFs, through TalkToBolo's MCP server.
      </p>

      <Card class="mt-6 p-4 sm:mt-8 sm:p-6">
        <h2 class="text-[18px] font-semibold tracking-tight">Bring your courses into the conversation</h2>
        <p class="mt-2 max-w-xl text-[14px] leading-relaxed text-muted">
          Connect ChatGPT, then sign in to TalkToBolo and approve access. No API key to copy.
        </p>
        <div class="mt-5 flex flex-col gap-3 sm:flex-row sm:flex-wrap">
          <Button onClick={guide.open}>
            <MessageCircle size={17} stroke-width={1.8} /> {guide.connected() ? 'Show the setup guide' : 'Connect ChatGPT'}
          </Button>
        </div>
        <p class="mt-3 text-[13px] leading-relaxed text-muted">
          A short setup in ChatGPT's Plugins page, once, in a browser. The guide walks you through it.
        </p>
        <div class="mt-5">
          <CopyField label="Server URL" text={MCP_URL} mono />
        </div>
      </Card>

      <Card class="mt-4 p-4 sm:p-6">
        <div class="flex items-center gap-3">
          <span class="flex h-10 w-10 shrink-0 items-center justify-center rounded-2xl bg-soft text-muted">
            <AudioLines size={20} stroke-width={1.7} />
          </span>
          <h2 class="text-[18px] font-semibold tracking-tight">Learn on the go with ChatGPT voice</h2>
        </div>
        <p class="mt-3 max-w-xl text-[14px] leading-relaxed text-muted">
          Once connected, open ChatGPT on your phone, start Voice and ask for your course. It picks up where you left off in each
          lecture, sticks to the sections you have reached, and cites the lecture page.
        </p>
        <ul class="mt-4 flex flex-col gap-2" aria-label="Things to say">
          <For each={VOICE_PROMPTS}>
            {(prompt) => <li class="rounded-2xl bg-soft px-3.5 py-2.5 text-[14px] [overflow-wrap:anywhere]">"{prompt}"</li>}
          </For>
        </ul>
        <p class="mt-4 max-w-xl text-[13px] leading-relaxed text-muted">
          Reading your notes runs without asking when ChatGPT's plugin permissions allow read actions. Saving a session and your
          progress asks you on screen first; saved sessions appear under Sessions with their recap.
        </p>
      </Card>

      <NewApiKey />

      <Card class="mt-6 p-2 sm:mt-8">
        <h2 class="px-3 pb-1 pt-3 text-[16px] font-semibold">Connections</h2>
        <ul>
          <For each={keys.data() ?? []} fallback={<li class="px-3 py-4 text-[14px] text-muted">No connections yet.</li>}>
            {(k) => (
              <li class="flex items-center gap-2 rounded-2xl px-2 py-2.5 hover:bg-soft sm:gap-3 sm:px-3">
                <Dot tone={k.revoked_at ? 'idle' : 'ready'} class="mx-1" />
                <div class="min-w-0 flex-1">
                  <span class="text-[15px] font-medium [overflow-wrap:anywhere]">{k.name}</span>
                  <span class="block font-mono text-[12px] text-faint sm:ml-2 sm:inline">{k.prefix}…</span>
                  <span class="block text-[13px] text-muted">
                    {k.revoked_at ? `Revoked ${formatDate(k.revoked_at)}` : `Created ${formatDate(k.created_at)}`}
                  </span>
                </div>
                <Show when={!k.revoked_at}>
                  <Button variant="soft" size="sm" class="shrink-0" onClick={() => db.update('api_key', k.id, { revoked_at: new Date().toISOString() })}>
                    Revoke
                  </Button>
                </Show>
              </li>
            )}
          </For>
        </ul>
      </Card>
    </div>
  );
}
