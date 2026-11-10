import { useParams } from '@solidjs/router';
import { createMemo, createSignal, For, Show } from 'solid-js';
import { useDb, useQuery } from '@spooky-sync/client-solid';
import { Check, ExternalLink, Link2 } from 'lucide-solid';
import type { schema } from '../schema.gen';
import { siteUrl } from '../lib/api';
import { firstRow, qSite } from '../lib/query';
import { buttonClass, IconButton, inputClass, PageLoader } from '../components/ui';
import { CourseCrumb } from '../components/CourseCrumb';
import { Focus, HeaderEndActions, HeaderTitle } from '../components/HeaderSlot';

/**
 * An uploaded site, in the whole window. The frame's sandbox matches the one
 * the API sends with every file (apps/api/src/sites.ts): the page runs without
 * the app's origin, so it cannot reach the student's session.
 */
export default function SitePage() {
  const params = useParams();
  const db = useDb<typeof schema>();
  const siteId = `site:${params.siteKey}`;
  const site = firstRow(useQuery(() => qSite(db, siteId)));
  const pages = createMemo<string[]>(() => {
    try {
      return JSON.parse(site.data()?.pages ?? '[]') as string[];
    } catch {
      return [];
    }
  });
  const url = () => siteUrl(params.siteKey ?? '', site.data()?.entry ?? '');

  const [copied, setCopied] = createSignal(false);
  const copy = async () => {
    await navigator.clipboard.writeText(url());
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };

  return (
    <Show when={site.data()} fallback={site.isSettled() ? <div class="p-10 text-muted">Site not found.</div> : <PageLoader />}>
      {(s) => (
        <div class="flex h-full min-h-0 flex-col">
          <Focus />
          <HeaderTitle>
            <div class="flex min-w-0 items-center gap-2.5">
              <CourseCrumb courseKey={params.courseKey ?? ''} tab="sites" />
              <span class="text-faint">/</span>
              <EditableTitle value={s().title} onSave={(title) => void db.update('site', siteId, { title })} />
            </div>
          </HeaderTitle>
          <HeaderEndActions>
            <Show when={pages().length > 1}>
              <select
                class={`${inputClass} h-9 w-auto max-w-40 py-0 text-[14px] max-sm:hidden`}
                aria-label="Page the site opens on"
                title="Page the site opens on"
                onChange={(e) => void db.update('site', siteId, { entry: e.currentTarget.value })}
              >
                <For each={pages()}>
                  {(page) => (
                    <option value={page} selected={page === s().entry}>
                      {page}
                    </option>
                  )}
                </For>
              </select>
            </Show>
            <Show when={s().status === 'ready'}>
              <IconButton label={copied() ? 'Link copied' : 'Copy link. Anyone with it can open the site.'} size="sm" onClick={() => void copy()}>
                {copied() ? <Check size={16} stroke-width={1.75} /> : <Link2 size={16} stroke-width={1.75} />}
              </IconButton>
              <a href={url()} target="_blank" rel="noopener" class={`${buttonClass('ghost', 'sm')} w-11 px-0 md:w-8`} aria-label="Open in a new tab" title="Open in a new tab">
                <ExternalLink size={16} stroke-width={1.75} />
              </a>
            </Show>
          </HeaderEndActions>
          <Show when={s().status === 'ready'} fallback={<div class="p-10 text-muted">This site's upload did not finish. Delete it and upload it again.</div>}>
            {/* Keyed by address, so choosing another first page loads it. */}
            <Show when={url()} keyed>
              {(src) => <iframe title={s().title} src={src} class="block min-h-0 w-full flex-1 border-0 bg-white" sandbox="allow-scripts allow-forms allow-modals allow-downloads allow-popups allow-popups-to-escape-sandbox" allow="autoplay; fullscreen" />}
            </Show>
          </Show>
        </div>
      )}
    </Show>
  );
}

function EditableTitle(props: { value: string; onSave: (v: string) => void }) {
  const [editing, setEditing] = createSignal(false);
  return (
    <Show
      when={editing()}
      fallback={
        <h1 class="min-w-0 cursor-text truncate text-[15px] font-semibold tracking-tight sm:text-[16px]" onClick={() => setEditing(true)} title="Click to rename">
          {props.value}
        </h1>
      }
    >
      <input
        class="-mx-2 h-9 w-full min-w-0 rounded-lg bg-soft px-2 text-[15px] font-semibold tracking-tight outline-none sm:text-[16px]"
        value={props.value}
        aria-label="Site name"
        ref={(el) => queueMicrotask(() => el.select())}
        onKeyDown={(e) => {
          if (e.key === 'Enter') e.currentTarget.blur();
          if (e.key === 'Escape') setEditing(false);
        }}
        onBlur={(e) => {
          const v = e.currentTarget.value.trim();
          if (v && v !== props.value) props.onSave(v);
          setEditing(false);
        }}
      />
    </Show>
  );
}
