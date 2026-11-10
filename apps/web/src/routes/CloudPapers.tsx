import { A } from '@solidjs/router';
import { createMemo, For, Show } from 'solid-js';
import { useDb, useQuery } from '@spooky-sync/client-solid';
import { ScrollText } from 'lucide-solid';
import type { schema } from '../schema.gen';
import { useAuth } from '../lib/auth';
import { authorLabel } from '../lib/cloud-papers';
import { idKey } from '../lib/ids';
import { qCloudPapers } from '../lib/query';
import { EngineChip } from '../components/EngineChip';
import { Card, formatDate, Loader } from '../components/ui';

/** The papers Oyl on the user's Macs synced to the account, latest change first. */
export default function CloudPapersPage() {
  const db = useDb<typeof schema>();
  const auth = useAuth();
  const papers = useQuery(() => (auth.userId() ? qCloudPapers(db, auth.userId()!) : null));
  // Removed from the cloud on a Mac: kept for a restore, not listed.
  const shown = createMemo(() => (papers.data() ?? []).filter((p) => !p.deleted_at));

  return (
    <div class="mx-auto w-full min-w-0 max-w-3xl px-4 pb-6 pt-6 sm:px-8 sm:pb-14 sm:pt-5">
      <h1 class="font-display text-[28px] font-normal leading-tight tracking-[-0.01em] sm:text-[32px]">Papers</h1>
      <p class="mt-1.5 text-[15px] leading-relaxed text-muted sm:text-[16px]">
        The papers you write in Oyl on your Mac, synced to your account. Open one to read its source and its history.
      </p>

      <Card class="mt-6 p-1.5 sm:mt-8 sm:p-2">
        <ul>
          <For
            each={shown()}
            fallback={
              <li class="px-4 py-12 text-center text-[15px] text-muted">
                <Show when={!papers.isLoading()} fallback={<Loader size={6} label="Loading papers" class="mx-auto text-faint" />}>
                  No papers yet. Turn on sync for a paper in Oyl on your Mac and it shows up here.
                </Show>
              </li>
            }
          >
            {(p) => (
              <li>
                <A href={`/papers/${idKey(p.id)}`} class="flex items-center gap-3 rounded-lg px-3 py-3 hover:bg-soft">
                  <ScrollText size={19} stroke-width={1.6} class="shrink-0 text-muted" />
                  <span class="min-w-0 flex-1">
                    <span class="flex min-w-0 items-center gap-2">
                      <span class="truncate text-[15px] font-medium">{p.name}</span>
                      <EngineChip engine={p.engine} />
                    </span>
                    <span class="mt-0.5 block truncate text-[13px] text-muted">
                      <span class="font-mono text-[12px]">{p.main}</span>
                      <Show when={p.head} fallback={<span>, not synced yet</span>}>
                        <Show when={authorLabel(p.updated_by)}>{(who) => <span>, from {who()}</span>}</Show>
                      </Show>
                    </span>
                  </span>
                  <span class="shrink-0 text-[12px] text-muted sm:text-[13px]">{formatDate(p.updated_at, true)}</span>
                </A>
              </li>
            )}
          </For>
        </ul>
      </Card>
    </div>
  );
}
