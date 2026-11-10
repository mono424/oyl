import { A, useParams } from '@solidjs/router';
import { createMemo, For, Show } from 'solid-js';
import { useDb, useQuery } from '@spooky-sync/client-solid';
import { ArrowLeft, FileText } from 'lucide-solid';
import type { schema } from '../schema.gen';
import { materialPath, pageLabel } from '../lib/api';
import { useAuth } from '../lib/auth';
import { idKey } from '../lib/ids';
import { renderMarkdown } from '../lib/markdown';
import { modeLabel } from '../lib/modes';
import { firstRow, qCall, qMaterial, qTurnSources, qTurns, useCourse } from '../lib/query';
import { buttonClass, Dot, formatDate, Loader, PageLoader } from '../components/ui';

/**
 * A past study session (/calls/:callKey): one ChatGPT saved through the MCP
 * server, with its recap, or an earlier voice call with its transcript.
 * Mounted afresh per session (perParam in App.tsx).
 */
export default function SessionPage() {
  const params = useParams();
  const db = useDb<typeof schema>();
  const auth = useAuth();
  const callId = `call:${params.callKey}`;
  const record = firstRow(useQuery(() => qCall(db, callId)));
  const turns = useQuery(() => qTurns(db, callId));
  const cited = useQuery(() => qTurnSources(db, callId));

  const courseKey = () => (record.data()?.course ? idKey(record.data()!.course) : '');
  const materialKey = () => (record.data()?.material ? idKey(record.data()!.material!) : '');
  const course = useCourse(db, auth.userId, () => (courseKey() ? `course:${courseKey()}` : null));
  const material = firstRow(useQuery(() => (materialKey() ? qMaterial(db, `material:${materialKey()}`) : null)));
  const topic = () => material.data()?.title ?? course.data()?.title ?? '';

  // Each tutor turn's citations, one link per material and page.
  const sourcesBySeq = createMemo(() => {
    const out = new Map<number, { key: string; href: string; title: string; where: string | null }[]>();
    for (const s of cited.data() ?? []) {
      const where = pageLabel(s.page, s.page_end) ?? s.heading ?? null;
      const key = `${s.material}:${where ?? ''}`;
      const list = out.get(s.seq) ?? [];
      if (list.some((l) => l.key === key)) continue;
      list.push({ key, href: materialPath(`course:${courseKey()}`, s.material, s.page), title: s.title, where });
      out.set(s.seq, list);
    }
    return out;
  });

  const recapHtml = createMemo(() => (record.data()?.recap ? renderMarkdown(record.data()!.recap!) : ''));

  return (
    <Show when={record.data()} fallback={record.isLoading() ? <PageLoader /> : <div class="p-10 text-muted">Session not found.</div>}>
      {(call) => (
        <div class="mx-auto w-full min-w-0 max-w-3xl px-4 pb-10 pt-4 sm:px-8 sm:pb-14 sm:pt-5">
          {/* On larger screens the sidebar leads back. */}
          <A href="/calls" class={`${buttonClass('ghost', 'sm')} -ml-3 mb-2 text-muted md:hidden`}>
            <ArrowLeft size={16} stroke-width={1.75} class="shrink-0" /> Sessions
          </A>
          <h1 class="break-words text-[26px] font-semibold leading-tight tracking-tight sm:text-[30px]">
            {modeLabel(call().mode)}
            <Show when={topic()}>: {topic()}</Show>
          </h1>
          <p class="mt-1.5 break-words text-[15px] text-muted">
            {call().assistant ? `Studied in ${call().assistant}` : 'Voice call with Bolo'}, {formatDate(call().started_at, true)}
          </p>

          <Show when={recapHtml()}>
            <div class="mt-6 min-w-0 rounded-[20px] bg-soft p-4 sm:mt-8 sm:p-6">
              <div class="mb-3 flex items-center gap-2 text-[13px] text-muted">
                <Dot tone="ready" /> Recap
              </div>
              <div class="lecture" innerHTML={recapHtml()} />
            </div>
          </Show>

          <Show
            when={(turns.data() ?? []).length > 0}
            fallback={
              <p class="flex justify-center py-8 text-center text-[15px] text-muted">
                {turns.isLoading()
                  ? <Loader label="Loading the transcript" />
                  : call().assistant
                    ? `This session took place in ${call().assistant}. Only its recap is kept.`
                    : 'Nothing was said in this call.'}
              </p>
            }
          >
            <h2 class="mb-3 mt-8 text-[13px] text-muted">Transcript</h2>
            <ol class="space-y-3" aria-label="Transcript">
              <For each={turns.data() ?? []}>
                {(t) => (
                  <li class={`flex flex-col ${t.role === 'student' ? 'items-end' : 'items-start'}`}>
                    <p
                      class={`min-w-0 max-w-[88%] rounded-[20px] px-4 py-2.5 text-[15px] leading-relaxed [overflow-wrap:anywhere] sm:max-w-[80%] ${
                        t.role === 'student' ? 'rounded-br-md bg-solid text-solid-fg' : 'rounded-bl-md bg-soft text-fg'
                      }`}
                    >
                      <span class="sr-only">{t.role === 'tutor' ? 'Tutor: ' : 'You: '}</span>
                      {t.text}
                    </p>
                    <Show when={t.role === 'tutor' && sourcesBySeq().get(t.seq)}>
                      {(sources) => (
                        <div class="mt-1.5 flex min-w-0 max-w-[88%] flex-wrap gap-1.5 sm:max-w-[80%]" aria-label="Sources">
                          <For each={sources()}>
                            {(s) => (
                              <A
                                href={s.href}
                                class="inline-flex min-w-0 max-w-full items-center gap-1.5 rounded-full border border-line px-2.5 py-1 text-[12px] text-muted transition-colors hover:bg-soft hover:text-fg"
                              >
                                <FileText size={12} stroke-width={1.9} class="shrink-0" />
                                <span class="truncate">{s.title}</span>
                                <Show when={s.where}>
                                  <span class="max-w-[60%] shrink-0 truncate tabular-nums">· {s.where}</span>
                                </Show>
                              </A>
                            )}
                          </For>
                        </div>
                      )}
                    </Show>
                  </li>
                )}
              </For>
            </ol>
          </Show>
        </div>
      )}
    </Show>
  );
}
