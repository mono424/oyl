import { A, useSearchParams } from '@solidjs/router';
import { createResource, createSignal, Match, Show, Switch } from 'solid-js';
import { useDb } from '@spooky-sync/client-solid';
import { ArrowUpRight, Laptop, Link2 } from 'lucide-solid';
import type { schema } from '../schema.gen';
import { answerDeviceSignIn, api, deviceSignIn } from '../lib/api';
import { useAuth } from '../lib/auth';
import { Button, buttonClass, Card, inputClass, Loader } from '../components/ui';

interface Consent {
  requestId: string;
  clientName: string;
  /** Set when the client identified itself with a metadata document on this host. */
  clientHost: string | null;
  redirectHost: string;
  scopes: string[];
  expiresAt: string;
}

/**
 * Account approval stays in Oyl; credentials never travel in install links.
 * `?request=` is an AI client's OAuth consent, anything else Oyl on a Mac
 * signing in to sync papers (`?code=`, the code the app shows).
 */
export default function ConnectPage() {
  const [params] = useSearchParams();
  return (
    <div class="mx-auto w-full min-w-0 max-w-xl px-4 py-7 sm:px-5 sm:py-20">
      <Show when={typeof params.request === 'string' && params.request} fallback={<DeviceApproval />}>
        <AssistantConsent />
      </Show>
    </div>
  );
}

function AssistantConsent() {
  const db = useDb<typeof schema>();
  const auth = useAuth();
  const [params] = useSearchParams();
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal('');
  const [consent] = createResource(() => typeof params.request === 'string' ? params.request : '', async (requestId) => {
    if (!requestId) throw new Error('This connection link is incomplete. Start again from your assistant.');
    return api<Consent>(db, '/api/oauth/request', { method: 'POST', body: JSON.stringify({ requestId }) });
  });

  const decide = async (allow: boolean) => {
    const request = consent();
    if (!request) return;
    setBusy(true);
    setError('');
    try {
      const result = await api<{ redirectUrl: string }>(db, '/api/oauth/approve', {
        method: 'POST', body: JSON.stringify({ requestId: request.requestId, allow }),
      });
      window.location.assign(result.redirectUrl);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'The connection could not be completed. Try again.');
      setBusy(false);
    }
  };

  return (
    <Card class="p-5 sm:p-8">
      <span class="mb-5 flex h-12 w-12 items-center justify-center rounded-lg bg-soft text-muted"><Link2 size={23} stroke-width={1.6} /></span>
      <h1 class="font-display text-[28px] font-normal leading-tight tracking-[-0.01em]">Connect your assistant</h1>
      <Show when={consent.loading}><Loader size={6} label="Checking your connection request" class="mt-5 text-faint" /></Show>
      <Show when={consent.error}>
        <p class="mt-4 break-words text-[15px] text-danger" role="alert">{consent.error?.message ?? 'This connection request has expired.'}</p>
        <A href="/settings" class={`${buttonClass('soft')} mt-5 w-full sm:w-auto`}>Back to ChatGPT settings</A>
      </Show>
      <Show when={consent()}>
        {(request) => <>
          <p class="mt-3 text-[15px] leading-relaxed text-muted [overflow-wrap:anywhere]">
            <strong class="font-medium text-fg">{request().clientName}</strong>
            <Show when={request().clientHost}>{(host) => <span> ({host()})</span>}</Show> wants to use Oyl as{' '}
            {auth.user()?.email ?? 'your signed-in account'}.
          </p>
          <div class="mt-5 rounded-2xl bg-soft p-4 text-[14px] leading-relaxed">
            <p class="font-medium">This connection can:</p>
            <ul class="mt-2 list-disc space-y-1 pl-5 text-muted">
              <li>Read and search your courses, lecture material, progress and past study sessions.</li>
              <li>Move your lecture progress forward and save study sessions with their recap.</li>
              <li>Create, edit and delete your courses and material.</li>
            </ul>
          </div>
          <p class="mt-4 text-[13px] leading-relaxed text-muted [overflow-wrap:anywhere]">
            Return to {request().redirectHost} after approval. You can revoke this connection from the ChatGPT page in Oyl at any time.
          </p>
          <Show when={error()}><p class="mt-4 break-words text-[14px] text-danger" role="alert">{error()}</p></Show>
          <div class="mt-6 flex flex-col gap-3 sm:flex-row sm:flex-wrap">
            <Button disabled={busy()} onClick={() => decide(true)}><Show when={busy()} fallback={<>Allow access<ArrowUpRight size={16} /></>}><Loader size={4} label="Connecting" class="text-on-accent" /></Show></Button>
            <Button variant="ghost" disabled={busy()} onClick={() => decide(false)}>Cancel</Button>
          </div>
        </>}
      </Show>
    </Card>
  );
}

/**
 * Oyl on a Mac asks to sync papers with this account (docs/sync-protocol.md,
 * Auth). The page shows which device and the code, so the user can check it
 * matches what the app shows, and answers. The app then collects its key.
 */
function DeviceApproval() {
  const db = useDb<typeof schema>();
  const auth = useAuth();
  const [params, setParams] = useSearchParams();
  const [typed, setTyped] = createSignal('');
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal('');
  const [answer, setAnswer] = createSignal<'approved' | 'denied' | null>(null);
  const code = () => (typeof params.code === 'string' ? params.code.trim() : '');
  const [request] = createResource(code, (c) => deviceSignIn(db, c));

  const decide = async (allow: boolean) => {
    const r = request();
    if (!r) return;
    setBusy(true);
    setError('');
    try {
      setAnswer((await answerDeviceSignIn(db, r.user_code, allow)).status);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'The answer could not be saved. Try again.');
    } finally {
      setBusy(false);
    }
  };
  const status = () => answer() ?? request()?.status ?? null;

  return (
    <Card class="p-5 sm:p-8">
      <span class="mb-5 flex h-12 w-12 items-center justify-center rounded-lg bg-soft text-muted"><Laptop size={23} stroke-width={1.6} /></span>
      <h1 class="font-display text-[28px] font-normal leading-tight tracking-[-0.01em]">Connect Oyl on your Mac</h1>
      <Show
        when={code()}
        fallback={
          <form
            class="mt-4"
            onSubmit={(e) => {
              e.preventDefault();
              if (typed().trim()) setParams({ code: typed().trim().toUpperCase() });
            }}
          >
            <p class="text-[15px] leading-relaxed text-muted">Enter the code Oyl shows when you turn on sync.</p>
            <div class="mt-4 flex flex-col gap-3 sm:flex-row">
              <input
                class={`${inputClass} min-w-0 flex-1 font-mono uppercase tracking-[0.12em]`}
                placeholder="BCDF-GHJK"
                aria-label="Code from Oyl"
                autocomplete="off"
                autocapitalize="characters"
                spellcheck={false}
                maxLength={12}
                value={typed()}
                onInput={(e) => setTyped(e.currentTarget.value)}
              />
              <Button type="submit" disabled={!typed().trim()}>Continue</Button>
            </div>
          </form>
        }
      >
        <Switch>
          <Match when={request.loading}><Loader size={6} label="Checking the code" class="mt-5 text-faint" /></Match>
          <Match when={request.error}>
            <p class="mt-4 break-words text-[15px] text-danger" role="alert">{request.error?.message ?? 'This code is not valid or has expired.'}</p>
            <Button variant="soft" class="mt-5 w-full sm:w-auto" onClick={() => { setTyped(''); setParams({ code: undefined }); }}>Enter another code</Button>
          </Match>
          <Match when={request() && status() === 'approved'}>
            <p class="mt-3 text-[15px] leading-relaxed text-muted [overflow-wrap:anywhere]">
              <strong class="font-medium text-fg">Oyl on {request()!.device_name}</strong> is connected. Go back to the app: it finishes signing in by itself.
            </p>
            <p class="mt-3 text-[13px] leading-relaxed text-muted">Disconnect it any time under Connections in Settings.</p>
            <A href="/papers" class={`${buttonClass('soft')} mt-5 w-full sm:w-auto`}>Open your papers</A>
          </Match>
          <Match when={request() && status() === 'denied'}>
            <p class="mt-3 text-[15px] leading-relaxed text-muted [overflow-wrap:anywhere]">
              Declined. Oyl on {request()!.device_name} was not connected to your account.
            </p>
          </Match>
          <Match when={request()}>
            {(r) => <>
              <p class="mt-3 text-[15px] leading-relaxed text-muted [overflow-wrap:anywhere]">
                <strong class="font-medium text-fg">Oyl on {r().device_name}</strong> wants to sync your papers with{' '}
                {auth.user()?.email ?? 'your signed-in account'}.
              </p>
              <div class="mt-5 rounded-2xl bg-soft p-4">
                <p class="text-[13px] text-muted">Check that Oyl shows the same code:</p>
                <p class="mt-1 font-mono text-[24px] tracking-[0.12em] text-fg">{r().user_code}</p>
              </div>
              <div class="mt-4 text-[14px] leading-relaxed">
                <p class="font-medium">This connection can:</p>
                <ul class="mt-2 list-disc space-y-1 pl-5 text-muted">
                  <li>Upload your papers, their files and their history to your account.</li>
                  <li>Download your synced papers to that Mac.</li>
                </ul>
                <p class="mt-2 text-muted">It cannot open your courses, documents or study sessions.</p>
              </div>
              <p class="mt-4 text-[13px] leading-relaxed text-muted">
                Only allow it if you just turned on sync in Oyl on your own Mac. Someone who sent you this link could otherwise read your papers.
              </p>
              <Show when={error()}><p class="mt-4 break-words text-[14px] text-danger" role="alert">{error()}</p></Show>
              <div class="mt-6 flex flex-col gap-3 sm:flex-row sm:flex-wrap">
                <Button disabled={busy()} onClick={() => decide(true)}>
                  <Show when={busy()} fallback={<>Allow</>}><Loader size={4} label="Connecting" class="text-on-accent" /></Show>
                </Button>
                <Button variant="ghost" disabled={busy()} onClick={() => decide(false)}>Decline</Button>
              </div>
            </>}
          </Match>
        </Switch>
      </Show>
    </Card>
  );
}
