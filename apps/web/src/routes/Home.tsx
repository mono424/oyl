import { A, useNavigate } from '@solidjs/router';
import { createSignal, For, Show } from 'solid-js';
import { useDb, useQuery } from '@spooky-sync/client-solid';
import { AudioLines, Folder, Plus, TriangleAlert } from 'lucide-solid';
import type { schema } from '../schema.gen';
import { useAuth } from '../lib/auth';
import { idKey, newId } from '../lib/ids';
import { courseColor } from '../lib/palette';
import { qAttention, qCourses } from '../lib/query';
import { useChatGPTGuide } from '../components/ChatGPTGuide';
import { Button, buttonClass, Card, greeting, NoticeCard } from '../components/ui';

export default function Home() {
  const db = useDb<typeof schema>();
  const auth = useAuth();
  const uid = () => auth.userId();
  const courses = useQuery(() => (uid() ? qCourses(db, uid()!) : null));
  const attention = useQuery(() => (uid() ? qAttention(db, uid()!) : null));
  const guide = useChatGPTGuide();
  const [dismissed, setDismissed] = createSignal<string[]>([]);

  const failed = () => (attention.data() ?? []).filter((m) => m.status === 'failed' && !dismissed().includes(m.id));
  const firstName = () => auth.user()?.name?.split(' ')[0];

  return (
    <div class="mx-auto w-full max-w-3xl px-4 pb-7 pt-6 sm:px-8 sm:pb-14 sm:pt-5">
      <h1 class="break-words text-[28px] font-semibold leading-tight tracking-tight sm:text-[30px]">
        {greeting()}
        <Show when={firstName()}>, {firstName()}</Show>
      </h1>
      <p class="mt-1.5 text-[16px] text-muted">Add your lecture material, then study it with ChatGPT.</p>

      <Show when={(courses.data() ?? []).length > 0 && !guide.connected()}>
        <NoticeCard
          class="mt-8"
          icon={<AudioLines size={20} stroke-width={1.75} />}
          title="Study by voice in ChatGPT"
          description="Connect once, then ask ChatGPT voice for your course. Your progress stays in sync."
        >
          <Button onClick={guide.open}>Connect ChatGPT</Button>
        </NoticeCard>
      </Show>

      <Show when={failed().length > 0}>
        <div class="mt-6 space-y-3">
          <For each={failed()}>
            {(m) => (
              <NoticeCard
                icon={<TriangleAlert size={20} stroke-width={1.75} />}
                title={`${m.title} could not be processed`}
                description={m.error ?? 'Something went wrong while reading this material.'}
                tone="attention"
              >
                <A href={`/courses/${idKey(m.course)}/materials/${idKey(m.id)}`} class={buttonClass('solid')}>
                  Review
                </A>
                <A href={`/courses/${idKey(m.course)}`} class={buttonClass('soft')}>
                  Upload again
                </A>
                <Button variant="ghost" class="ml-auto" onClick={() => setDismissed([...dismissed(), m.id])}>
                  Dismiss
                </Button>
              </NoticeCard>
            )}
          </For>
        </div>
      </Show>

      <section class="mt-10" aria-label="Courses">
        <h2 class="mb-3 text-[13px] text-muted">Courses</h2>
        <div class="grid gap-3 sm:grid-cols-2">
          <For each={courses.data() ?? []}>
            {(c) => (
              <A href={`/courses/${idKey(c.id)}`} class="block min-w-0">
                <Card class="h-full min-w-0 p-5 transition-colors hover:border-line-strong">
                  <Folder size={22} stroke-width={1.6} style={{ color: courseColor(c.color) }} fill={courseColor(c.color)} fill-opacity="0.18" />
                  <h3 class="mt-4 truncate text-[16px] font-semibold">{c.title}</h3>
                  <p class="mt-1 line-clamp-2 text-[14px] text-muted">{c.description || 'Lecture material'}</p>
                </Card>
              </A>
            )}
          </For>
          <CreateCourseCard />
        </div>
      </section>
    </div>
  );
}

function CreateCourseCard() {
  const db = useDb<typeof schema>();
  const auth = useAuth();
  const navigate = useNavigate();
  const [editing, setEditing] = createSignal(false);
  const [title, setTitle] = createSignal('');

  const create = async (e: SubmitEvent) => {
    e.preventDefault();
    if (!auth.userId() || !title().trim()) return;
    const id = newId('course');
    await db.create(id, { owner: auth.userId()!, title: title().trim(), color: 'blue', language: 'en' });
    navigate(`/courses/${idKey(id)}`);
  };

  return (
    <Card class="p-5">
      <Show
        when={editing()}
        fallback={
          <button type="button" class="block w-full text-left" onClick={() => setEditing(true)}>
            <Plus size={24} stroke-width={1.5} class="text-muted" />
            <h3 class="mt-4 text-[16px] font-semibold">Create a course</h3>
            <p class="mt-1 text-[14px] text-muted">Slides, notes and scripts for one class</p>
          </button>
        }
      >
        <form onSubmit={create}>
          <Plus size={24} stroke-width={1.5} class="text-muted" />
          <input
            class="mt-4 w-full bg-transparent text-[16px] font-semibold outline-none placeholder:font-normal placeholder:text-faint"
            placeholder="Course name, e.g. Linear Algebra"
            value={title()}
            onInput={(e) => setTitle(e.currentTarget.value)}
            onKeyDown={(e) => e.key === 'Escape' && setEditing(false)}
            aria-label="Course name"
            ref={(el) => queueMicrotask(() => el.focus())}
          />
          <div class="mt-3 flex gap-2">
            <Button type="submit" size="sm" disabled={!title().trim()}>
              Create
            </Button>
            <Button variant="ghost" size="sm" onClick={() => setEditing(false)}>
              Cancel
            </Button>
          </div>
        </form>
      </Show>
    </Card>
  );
}
