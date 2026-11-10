import { A, useSearchParams } from '@solidjs/router';
import { createResource, createSignal, Show } from 'solid-js';
import { useDb } from '@spooky-sync/client-solid';
import { ArrowUpRight, Link2 } from 'lucide-solid';
import type { schema } from '../schema.gen';
import { api } from '../lib/api';
import { useAuth } from '../lib/auth';
import { Button, buttonClass, Card, Loader } from '../components/ui';

interface Consent {
  requestId: string;
  clientName: string;
  /** Set when the client identified itself with a metadata document on this host. */
  clientHost: string | null;
  redirectHost: string;
  scopes: string[];
  expiresAt: string;
}

/** Account approval stays in Bolo; credentials never travel in install links. */
export default function ConnectPage() {
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
    <div class="mx-auto w-full min-w-0 max-w-xl px-4 py-7 sm:px-5 sm:py-20">
      <Card class="p-5 sm:p-8">
        <span class="mb-5 flex h-12 w-12 items-center justify-center rounded-2xl bg-soft text-muted"><Link2 size={23} stroke-width={1.6} /></span>
        <h1 class="text-[26px] font-semibold leading-tight tracking-tight">Connect your assistant</h1>
        <Show when={consent.loading}><Loader size={6} label="Checking your connection request" class="mt-5 text-faint" /></Show>
        <Show when={consent.error}>
          <p class="mt-4 break-words text-[15px] text-red" role="alert">{consent.error?.message ?? 'This connection request has expired.'}</p>
          <A href="/settings" class={`${buttonClass('soft')} mt-5 w-full sm:w-auto`}>Back to ChatGPT settings</A>
        </Show>
        <Show when={consent()}>
          {(request) => <>
            <p class="mt-3 text-[15px] leading-relaxed text-muted [overflow-wrap:anywhere]">
              <strong class="font-medium text-fg">{request().clientName}</strong>
              <Show when={request().clientHost}>{(host) => <span> ({host()})</span>}</Show> wants to use TalkToBolo as{' '}
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
              Return to {request().redirectHost} after approval. You can revoke this connection from the ChatGPT page in TalkToBolo at any time.
            </p>
            <Show when={error()}><p class="mt-4 break-words text-[14px] text-red" role="alert">{error()}</p></Show>
            <div class="mt-6 flex flex-col gap-3 sm:flex-row sm:flex-wrap">
              <Button disabled={busy()} onClick={() => decide(true)}><Show when={busy()} fallback={<>Allow access<ArrowUpRight size={16} /></>}><Loader size={4} label="Connecting" class="text-solid-fg" /></Show></Button>
              <Button variant="ghost" disabled={busy()} onClick={() => decide(false)}>Cancel</Button>
            </div>
          </>}
        </Show>
      </Card>
    </div>
  );
}
