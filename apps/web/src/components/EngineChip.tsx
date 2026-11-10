import { engineLabel } from '../lib/cloud-papers';

/** What a synced paper is written in: Typst or LaTeX. */
export function EngineChip(props: { engine: string | null | undefined; class?: string }) {
  return (
    <span class={`inline-flex h-5 shrink-0 items-center rounded-md bg-soft px-1.5 text-[11px] font-medium tracking-wide text-muted ${props.class ?? ''}`}>
      {engineLabel(props.engine)}
    </span>
  );
}
