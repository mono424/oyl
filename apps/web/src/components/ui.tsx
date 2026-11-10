import type { JSX } from 'solid-js';
import { Show, splitProps } from 'solid-js';
import { GridLoader, type GridLoaderProps } from 'punktraster/solid';

// ── Buttons ───────────────────────────────────────────────────────────────

type ButtonProps = JSX.ButtonHTMLAttributes<HTMLButtonElement> & {
  /** solid: the one main action. soft: secondary. ghost: dismiss-level. */
  variant?: 'solid' | 'soft' | 'ghost' | 'danger';
  size?: 'sm' | 'md' | 'lg';
};

const VARIANTS = {
  solid: 'bg-solid text-solid-fg hover:bg-solid-hover',
  soft: 'bg-soft text-fg hover:bg-hover',
  ghost: 'text-fg hover:bg-soft',
  danger: 'bg-red text-white hover:brightness-95',
} as const;

const SIZES = {
  sm: 'h-11 px-3.5 text-[13px] gap-1.5 md:h-8',
  md: 'h-11 px-4.5 text-[15px] gap-2 md:h-10',
  lg: 'h-12 px-6 text-base gap-2.5',
} as const;

/** Pill styling, for buttons and for links that act as buttons. */
export const buttonClass = (variant: keyof typeof VARIANTS = 'solid', size: keyof typeof SIZES = 'md') =>
  `inline-flex items-center justify-center whitespace-nowrap rounded-full font-medium transition-colors disabled:opacity-40 disabled:pointer-events-none ${VARIANTS[variant]} ${SIZES[size]}`;

export function Button(props: ButtonProps) {
  const [local, rest] = splitProps(props, ['variant', 'size', 'class', 'type']);
  return <button type={local.type ?? 'button'} class={`${buttonClass(local.variant, local.size)} ${local.class ?? ''}`} {...rest} />;
}

/** Round icon-only button. */
export function IconButton(props: JSX.ButtonHTMLAttributes<HTMLButtonElement> & { label: string; size?: 'sm' | 'md' | 'lg' }) {
  const [local, rest] = splitProps(props, ['label', 'size', 'class', 'type']);
  const size = { sm: 'h-11 w-11 md:h-8 md:w-8', md: 'h-11 w-11 md:h-10 md:w-10', lg: 'h-14 w-14' }[local.size ?? 'md'];
  return (
    <button
      type={local.type ?? 'button'}
      aria-label={local.label}
      title={local.label}
      class={`inline-flex shrink-0 items-center justify-center rounded-full text-muted hover:text-fg hover:bg-soft transition-colors ${size} ${local.class ?? ''}`}
      {...rest}
    />
  );
}

// ── Surfaces ──────────────────────────────────────────────────────────────

export function Card(props: { children: JSX.Element; class?: string; as?: 'section' | 'div' } & JSX.HTMLAttributes<HTMLElement>) {
  const [local, rest] = splitProps(props, ['children', 'class', 'as']);
  return (
    <section class={`rounded-[20px] border border-line bg-surface shadow-card ${local.class ?? ''}`} {...rest}>
      {local.children}
    </section>
  );
}

/**
 * A card that asks for a decision: icon, title, state dot, one or two lines of
 * description, then actions. The sidebar's "Needs attention" items open these.
 */
export function NoticeCard(props: {
  icon: JSX.Element;
  title: string;
  description?: string | null;
  tone?: Tone;
  children?: JSX.Element;
  class?: string;
}) {
  return (
    <Card class={`p-4 sm:p-5 ${props.class ?? ''}`}>
      <div class="flex items-start gap-3">
        <span class="mt-0.5 shrink-0 text-muted">{props.icon}</span>
        <div class="min-w-0 flex-1">
          <div class="flex items-center gap-3">
            <h3 class="line-clamp-2 flex-1 text-[16px] font-semibold">{props.title}</h3>
            <Show when={props.tone}>
              <Dot tone={props.tone!} />
            </Show>
          </div>
          <Show when={props.description}>
            <p class="mt-1.5 text-[15px] leading-relaxed text-muted line-clamp-2">{props.description}</p>
          </Show>
          <Show when={props.children}>
            <div class="mt-4 flex flex-wrap items-center gap-2 [&>*]:max-w-full">{props.children}</div>
          </Show>
        </div>
      </div>
    </Card>
  );
}

// ── Loading ───────────────────────────────────────────────────────────────

/** The punktraster dot raster, shown instead of loading text. Takes the text color. */
export function Loader(props: { label?: string; size?: number; pattern?: GridLoaderProps['pattern']; class?: string }) {
  return <GridLoader size={props.size ?? 5} pattern={props.pattern ?? 'helix'} label={props.label ?? 'Loading'} class={`shrink-0 ${props.class ?? 'text-faint'}`} />;
}

/** A loader centered in the page, for pages waiting on their record. */
export function PageLoader(props: { label?: string }) {
  return (
    <div class="flex justify-center p-10">
      <Loader size={7} label={props.label} />
    </div>
  );
}

// ── State dots ────────────────────────────────────────────────────────────

/** attention: red. running: blue, breathing. ready: green. idle: hollow ring. */
export type Tone = 'attention' | 'running' | 'ready' | 'idle';

export function Dot(props: { tone: Tone; class?: string }) {
  return (
    <span
      aria-hidden="true"
      class={`inline-block h-2 w-2 shrink-0 rounded-full ${
        {
          attention: 'bg-red',
          running: 'bg-blue dot-breathe',
          ready: 'bg-green',
          idle: 'border-[1.5px] border-faint',
        }[props.tone]
      } ${props.class ?? ''}`}
    />
  );
}

export const materialTone = (status: string): Tone =>
  status === 'failed' ? 'attention' : status === 'ready' ? 'ready' : status === 'processing' ? 'running' : 'idle';

export function StatusText(props: { status: string; detail?: string | null }) {
  const label = () =>
    ({
      ready: 'Ready',
      failed: 'Processing failed',
      processing: props.detail || 'Processing',
      pending: 'Waiting to process',
    })[props.status] ?? props.status;
  return (
    <span class="inline-flex items-center gap-2 text-[13px] text-muted">
      <Dot tone={materialTone(props.status)} />
      {label()}
    </span>
  );
}

// ── Lists and labels ──────────────────────────────────────────────────────

export function SectionLabel(props: { children: JSX.Element; class?: string }) {
  return <h2 class={`px-3 pb-1.5 text-[13px] font-normal text-muted ${props.class ?? ''}`}>{props.children}</h2>;
}

/** A row in a list card: hover tint, optional leading icon or dot. */
export const rowClass = 'flex items-center gap-3 rounded-2xl px-3 h-11 text-[15px] hover:bg-soft transition-colors';

// ── Forms ─────────────────────────────────────────────────────────────────

export function Field(props: { label: string; hint?: string; children: JSX.Element; class?: string }) {
  return (
    <label class={`block ${props.class ?? ''}`}>
      <span class="mb-1.5 block text-[13px] text-muted">{props.label}</span>
      {props.children}
      {props.hint && <span class="mt-1.5 block text-[12px] text-faint">{props.hint}</span>}
    </label>
  );
}

export const inputClass =
  'form-input w-full min-w-0 rounded-xl border border-line bg-surface px-3.5 py-2.5 placeholder:text-faint transition-colors focus:border-line-strong focus:outline-none focus:ring-4 focus:ring-soft';

/** Segmented control: light gray track, white selected pill. */
export function Segmented<T extends string>(props: {
  value: T;
  options: { value: T; label: string }[];
  onChange: (v: T) => void;
  label: string;
}) {
  return (
    <div role="radiogroup" aria-label={props.label} class="inline-flex min-w-0 max-w-full rounded-full bg-soft p-1">
      {props.options.map((o) => (
        <button
          type="button"
          role="radio"
          aria-checked={props.value === o.value}
          onClick={() => props.onChange(o.value)}
          class={`min-h-11 min-w-0 flex-auto whitespace-nowrap rounded-full px-3 text-[14px] transition-colors md:min-h-8 md:flex-none md:px-3.5 ${
            props.value === o.value ? 'bg-surface font-medium text-fg shadow-[0_1px_2px_rgb(0_0_0/0.08)]' : 'text-muted hover:text-fg'
          }`}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

// ── Formatting ────────────────────────────────────────────────────────────

export function formatDate(v: unknown, withTime = false): string {
  if (!v) return '';
  const d = v instanceof Date ? v : new Date(String(v));
  if (Number.isNaN(d.getTime())) return '';
  return d.toLocaleDateString(undefined, {
    month: 'short',
    day: 'numeric',
    ...(withTime ? { hour: '2-digit', minute: '2-digit' } : {}),
  });
}

/** "Good morning" style greeting for the home screen. */
export function greeting(date = new Date()): string {
  const h = date.getHours();
  return h < 5 ? 'Late night' : h < 12 ? 'Good morning' : h < 18 ? 'Good afternoon' : 'Good evening';
}
