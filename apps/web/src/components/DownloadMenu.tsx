import { createSignal, For, Show } from 'solid-js';
import { Download } from 'lucide-solid';
import { IconButton } from './ui';

/** Saves a text or bytes as a file. */
export function download(name: string, body: string | Uint8Array<ArrayBuffer>, type: string) {
  const url = URL.createObjectURL(new Blob([body], { type }));
  const a = document.createElement('a');
  a.href = url;
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** A page's downloads (a document's, a paper's), behind one button. */
export function DownloadMenu(props: { items: { label: string; detail: string; icon: typeof Download; run: () => void }[] }) {
  const [open, setOpen] = createSignal(false);
  return (
    <div class="relative">
      <IconButton label="Download" size="sm" title="Download" aria-haspopup="menu" aria-expanded={open()} onClick={() => setOpen(!open())}>
        <Download size={16} stroke-width={1.75} />
      </IconButton>
      <Show when={open()}>
        <div class="fixed inset-0 z-40" onClick={() => setOpen(false)} />
        <div
          role="menu"
          class="absolute right-0 z-50 mt-2 w-64 oyl-glass rounded-2xl border-line-strong p-1.5 shadow-raised"
          onKeyDown={(e) => e.key === 'Escape' && setOpen(false)}
        >
          <For each={props.items}>
            {(it) => (
              <button
                type="button"
                role="menuitem"
                class="flex w-full items-center gap-3 rounded-xl px-3 py-2 text-left hover:bg-soft"
                onClick={() => {
                  setOpen(false);
                  it.run();
                }}
              >
                <it.icon size={16} stroke-width={1.75} class="shrink-0 text-muted" />
                <span class="min-w-0">
                  <span class="block text-[14px]">{it.label}</span>
                  <span class="block truncate text-[12px] text-muted">{it.detail}</span>
                </span>
              </button>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
