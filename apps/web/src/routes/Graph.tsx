import { A, useParams, useSearchParams } from '@solidjs/router';
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from 'solid-js';
import { useDb } from '@spooky-sync/client-solid';
import { ArrowLeft, Box, Square, X } from 'lucide-solid';
import ForceGraph3D, { type ForceGraph3DInstance } from '3d-force-graph';
import SpriteText from 'three-spritetext';
import * as THREE from 'three';
import type { schema } from '../schema.gen';
import { useAuth } from '../lib/auth';
import { fetchCourseGraph, materialPath, type ConceptLevel, type CourseGraph, type GraphConcept } from '../lib/api';
import { idKey } from '../lib/ids';
import { useCourse } from '../lib/query';
import { Loader } from '../components/ui';
import { Focus, HeaderEndActions, HeaderTitle } from '../components/HeaderSlot';

/**
 * The course knowledge graph in 3D: the concepts of each lecture (from Agent
 * Memory when it is processed, and what ChatGPT mapped while tutoring), what
 * each builds on, and the student's level on each one.
 * Learned concepts glow, concepts in progress take the color of their state,
 * and concepts not learned yet stay hollow and dim. `?lecture=` opens one
 * lecture: its concepts and the foundations they build on.
 *
 * The concept tables are server-only (not synced), so the graph comes from
 * the API and is fetched again when the window regains focus.
 */

const LEVELS: { level: ConceptLevel; label: string; color: string }[] = [
  { level: 'mastered', label: 'Mastered', color: '#7dffc6' },
  { level: 'solid', label: 'Solid', color: '#34d399' },
  { level: 'shaky', label: 'Shaky', color: '#fbbf24' },
  { level: 'struggling', label: 'Struggling', color: '#fb7185' },
  { level: 'introduced', label: 'Seen', color: '#60a5fa' },
  { level: 'unseen', label: 'Not learned', color: '#64748b' },
];
const LEVEL = Object.fromEntries(LEVELS.map((l) => [l.level, l])) as Record<ConceptLevel, (typeof LEVELS)[number]>;
const LEARNED = new Set<ConceptLevel>(['solid', 'mastered']);
const LECTURE_COLOR = '#a78bfa';
const BACKGROUND_COLOR = '#94a3b8';
const SCENE = '#05070d';

type Node = {
  id: string;
  kind: 'concept' | 'lecture' | 'background';
  name: string;
  concept?: GraphConcept;
  /** A foundation from outside the open lecture. */
  outside?: boolean;
  dependents: number;
  /** What the node's look depends on: a node is rebuilt when it changes. */
  look: string;
  x?: number;
  y?: number;
  z?: number;
};
type Link = { source: string | Node; target: string | Node; kind: 'requires' | 'part_of' | 'lecture' };

export default function GraphPage() {
  const params = useParams();
  const [search, setSearch] = useSearchParams();
  const db = useDb<typeof schema>();
  const auth = useAuth();
  const courseId = () => `course:${params.courseKey}`;
  const course = useCourse(db, auth.userId, courseId);

  const [graph, setGraph] = createSignal<CourseGraph | null>(null);
  const [loadError, setLoadError] = createSignal<string | null>(null);
  const refetch = async () => {
    try {
      setGraph(await fetchCourseGraph(db, courseId()));
      setLoadError(null);
    } catch (err) {
      // A graph already shown stays; a later refresh may still work.
      if (!graph()) setLoadError(err instanceof Error ? err.message : 'Something went wrong.');
    }
  };
  void refetch();
  const onFocus = () => document.visibilityState === 'visible' && void refetch();
  window.addEventListener('focus', onFocus);
  document.addEventListener('visibilitychange', onFocus);
  onCleanup(() => {
    window.removeEventListener('focus', onFocus);
    document.removeEventListener('visibilitychange', onFocus);
  });

  const lectureParam = () => {
    const v = Array.isArray(search.lecture) ? search.lecture[0] : search.lecture;
    return v ? `material:${v}` : null;
  };
  const lecture = createMemo(() => {
    const id = lectureParam();
    return id ? graph()?.lectures.find((l) => l.id === id) ?? null : null;
  });
  const [dims, setDims] = createSignal<2 | 3>(3);
  const [selected, setSelected] = createSignal<string | null>(null);

  // The concepts in view: the open lecture's and every foundation they build on.
  const scope = createMemo(() => {
    const g = graph();
    if (!g) return null;
    const open = lecture()?.id ?? null;
    const byId = new Map(g.concepts.map((c) => [c.id, c]));
    const builds = new Map<string, string[]>();
    for (const r of g.requires) builds.set(r.from, [...(builds.get(r.from) ?? []), r.to]);
    if (!open) return { graph: g, byId, ids: new Set(byId.keys()), own: new Set(byId.keys()) };
    const own = new Set(g.concepts.filter((c) => c.lecture === open).map((c) => c.id));
    const ids = new Set(own);
    const queue = [...own];
    while (queue.length) {
      for (const base of builds.get(queue.shift()!) ?? []) {
        if (!ids.has(base)) {
          ids.add(base);
          queue.push(base);
        }
      }
    }
    return { graph: g, byId, ids, own };
  });

  const counts = createMemo(() => {
    const s = scope();
    const out = Object.fromEntries(LEVELS.map((l) => [l.level, 0])) as Record<ConceptLevel, number>;
    if (!s) return { out, total: 0, learned: 0 };
    for (const id of s.own) out[s.byId.get(id)!.level]++;
    return { out, total: s.own.size, learned: out.solid + out.mastered };
  });

  const concept = () => {
    const id = selected();
    return id ? scope()?.byId.get(id) ?? null : null;
  };
  const lectureTitle = (id: string | null) => (id ? graph()?.lectures.find((l) => l.id === id)?.title ?? null : null);

  // ── The scene ─────────────────────────────────────────────────────────

  let host!: HTMLDivElement;
  let fg: ForceGraph3DInstance<Node, Link> | undefined;
  const nodes = new Map<string, Node>();
  const rings = new Map<string, THREE.Object3D>();
  const glow = glowTexture();

  onMount(() => {
    fg = new ForceGraph3D(host, { controlType: 'orbit' }) as unknown as ForceGraph3DInstance<Node, Link>;
    fg.backgroundColor(SCENE)
      .showNavInfo(false)
      .nodeLabel(() => '')
      .nodeThreeObject((n: Node) => nodeObject(n, glow, rings, selected()))
      .linkOpacity(0.5)
      .linkDirectionalArrowLength((l: Link) => (l.kind === 'requires' ? 3 : 0))
      .linkDirectionalArrowRelPos(1)
      .linkDirectionalParticles((l: Link) => (l.kind === 'requires' ? 2 : 0))
      .linkDirectionalParticleWidth(1.4)
      .linkDirectionalParticleSpeed(0.006)
      .linkDirectionalParticleColor((l: Link) => {
        const from = end(l.target);
        return from?.concept ? LEVEL[from.concept.level].color : '#ffffff';
      })
      .onNodeClick((n: Node) => {
        if (n.kind === 'concept') select(n.id);
        else if (n.kind === 'lecture') openLecture(n.id);
      })
      .onBackgroundClick(() => setSelected(null))
      .cooldownTicks(200);
    styleLinks(null);
    const link = fg.d3Force('link') as unknown as { distance: (fn: (l: Link) => number) => void } | undefined;
    link?.distance((l) => (l.kind === 'lecture' ? 46 : l.kind === 'part_of' ? 16 : 30));
    const charge = fg.d3Force('charge') as unknown as { strength: (n: number) => void } | undefined;
    charge?.strength(-60);

    const resize = new ResizeObserver(() => fg?.width(host.clientWidth).height(host.clientHeight));
    resize.observe(host);
    onCleanup(() => {
      resize.disconnect();
      fg?._destructor();
      fg = undefined;
      glow.dispose();
    });
  });

  // The view last fitted to the screen: the lecture and dimensions.
  let fitted = '';
  createEffect(() => {
    const s = scope();
    if (!fg || !s) return;
    const open = lecture()?.id ?? null;
    const list: Node[] = [];
    const links: Link[] = [];
    const keep = (draft: Node) => {
      // Reuse the node while it looks the same, so it keeps its place.
      const prev = nodes.get(draft.id);
      const node = prev && prev.look === draft.look ? Object.assign(prev, draft, { x: prev.x, y: prev.y, z: prev.z }) : { ...draft, x: prev?.x, y: prev?.y, z: prev?.z };
      nodes.set(node.id, node);
      list.push(node);
      return node;
    };

    const dependents = new Map<string, number>();
    for (const r of s.graph.requires) dependents.set(r.to, (dependents.get(r.to) ?? 0) + 1);

    for (const id of s.ids) {
      const c = s.byId.get(id)!;
      const outside = !s.own.has(id);
      keep({
        id,
        kind: 'concept',
        name: c.name,
        concept: c,
        outside,
        dependents: dependents.get(id) ?? 0,
        look: `${c.level}|${c.misconceptions.length > 0}|${outside}|${dependents.get(id) ?? 0}|${c.name}`,
      });
      if (c.parent && s.ids.has(c.parent)) links.push({ source: id, target: c.parent, kind: 'part_of' });
      else if (c.prior) links.push({ source: id, target: 'hub:background', kind: 'lecture' });
      else if (c.lecture && (!open || c.lecture === open)) links.push({ source: id, target: `hub:${c.lecture}`, kind: 'lecture' });
    }
    for (const r of s.graph.requires) {
      if (s.ids.has(r.from) && s.ids.has(r.to)) links.push({ source: r.from, target: r.to, kind: 'requires' });
    }
    // Hubs: the lectures with concepts in view, and background knowledge.
    const hubs = new Set(links.filter((l) => l.kind === 'lecture').map((l) => l.target as string));
    for (const hub of hubs) {
      const lectureId = hub.slice(4);
      const background = hub === 'hub:background';
      const name = background ? 'Background knowledge' : lectureTitle(lectureId) ?? 'Lecture';
      keep({ id: hub, kind: background ? 'background' : 'lecture', name, dependents: 0, look: `hub|${name}` });
    }
    for (const id of [...nodes.keys()]) if (!list.some((n) => n.id === id)) nodes.delete(id);

    fg.graphData({ nodes: list, links });
    const view = `${open}|${dims()}`;
    if (fitted !== view && list.length) {
      fitted = view;
      fg.onEngineStop(() => {
        fg?.onEngineStop(() => {});
        if (!fg) return;
        // 2D is seen from straight above.
        if (dims() === 2) {
          const { x, y, z } = fg.cameraPosition();
          fg.cameraPosition({ x: 0, y: 0, z: Math.hypot(x, y, z) }, { x: 0, y: 0, z: 0 });
        }
        // Room for the lectures and legend over the scene on phones.
        fg.zoomToFit(600, host.clientWidth < 640 ? 90 : 40);
      });
    }
  });

  createEffect(on(() => lecture()?.id, () => setSelected(null), { defer: true }));
  createEffect(on(dims, (d) => fg?.numDimensions(d), { defer: true }));

  // The selection: a ring on the node, brighter links around it.
  createEffect(on(selected, (id, prev) => {
    if (prev) rings.get(prev)?.scale.setScalar(0.0001);
    if (id) rings.get(id)?.scale.setScalar(1);
    styleLinks(id);
  }));

  /** Fresh accessors, so the graph redraws its links for the selection. */
  function styleLinks(id: string | null) {
    fg?.linkColor((l: Link) => linkColor(l, id))
      .linkWidth((l: Link) => (l.kind === 'requires' ? (touches(l, id) ? 0.8 : 0.35) : 0))
      .linkDirectionalArrowColor((l: Link) => linkColor(l, id));
  }

  function select(id: string) {
    setSelected(id);
    const n = nodes.get(id);
    if (!fg || n?.x == null) return;
    const { x = 0, y = 0, z = 0 } = n;
    const distance = 150;
    const ratio = 1 + distance / Math.max(1, Math.hypot(x, y, z));
    fg.cameraPosition(dims() === 2 ? { x, y, z: distance * 1.6 } : { x: x * ratio, y: y * ratio, z: z * ratio }, { x, y, z }, 900);
  }

  function openLecture(id: string | null) {
    setSearch({ lecture: id ? idKey(id) : undefined });
  }

  const related = (c: GraphConcept, dir: 'requires' | 'requiredBy') => {
    const s = scope();
    if (!s) return [];
    return s.graph.requires
      .filter((r) => (dir === 'requires' ? r.from === c.id : r.to === c.id))
      .map((r) => s.byId.get(dir === 'requires' ? r.to : r.from))
      .filter((x): x is GraphConcept => !!x);
  };

  // ── Overlay ───────────────────────────────────────────────────────────

  const glass = 'border border-white/10 bg-[#0b0f1a]/75 backdrop-blur-md';
  const chip = (active: boolean) =>
    `shrink-0 rounded-full border px-3 py-1.5 text-[13px] backdrop-blur-md transition-colors ${active ? 'border-white bg-white font-medium text-[#05070d]' : 'border-white/10 bg-[#0b0f1a]/60 text-white/75 hover:bg-white/15 hover:text-white'}`;

  return (
    <div class="relative h-full min-h-[24rem] overflow-hidden text-white" style={{ background: SCENE }}>
      <div ref={host} class="absolute inset-0" />

      {/* The whole window: back, the graph's name and the view in the top bar. */}
      <Focus />
      <HeaderTitle>
        <div class="focus-in flex min-w-0 items-center gap-1.5 sm:-ml-2 sm:gap-2">
          <A
            href={`/courses/${params.courseKey}`}
            class="inline-flex h-11 w-11 shrink-0 items-center justify-center rounded-full text-muted transition-colors hover:bg-soft hover:text-fg md:h-10 md:w-10"
            aria-label="Back to the course"
            title="Back to the course"
          >
            <ArrowLeft size={20} stroke-width={1.75} />
          </A>
          <div class="min-w-0">
            <div class="truncate text-[15px] font-semibold leading-tight tracking-tight sm:text-[16px]">{lecture()?.title ?? course.data()?.title ?? 'Course'}</div>
            <div class="truncate text-[12px] leading-tight text-muted sm:text-[13px]">Knowledge graph{lecture() ? ` · ${course.data()?.title ?? ''}` : ''}</div>
          </div>
        </div>
      </HeaderTitle>
      <HeaderEndActions>
        <ViewToggle dims={dims()} onChange={setDims} class="focus-in max-sm:hidden" />
      </HeaderEndActions>

      {/* The lectures over the scene, and on phones the view, which the top bar has no room for. */}
      <div class="pointer-events-none absolute inset-x-0 top-0 flex items-center gap-2 bg-linear-to-b from-[#05070d]/90 to-transparent pb-8 pr-3 pt-3 sm:pb-10 sm:pr-4 sm:pt-4">
        <div class="scroll-quiet pointer-events-auto flex min-w-0 flex-1 gap-1.5 overflow-x-auto px-3 sm:px-4">
          <Show when={(graph()?.lectures.length ?? 0) > 0}>
            <button type="button" class={chip(!lecture())} onClick={() => openLecture(null)}>
              Whole course
            </button>
            <For each={graph()?.lectures}>
              {(l) => (
                <button type="button" class={`${chip(lecture()?.id === l.id)} max-w-56 truncate`} onClick={() => openLecture(l.id)}>
                  {l.title}
                </button>
              )}
            </For>
          </Show>
        </div>
        <ViewToggle dims={dims()} onChange={setDims} dark class="pointer-events-auto sm:hidden" />
      </div>

      {/* Empty, loading and error states. */}
      <Show when={!graph() && loadError()}>
        {(err) => (
        <Centered>
          <p class="text-[15px] font-medium">The knowledge graph could not be loaded.</p>
          <p class="mt-1 text-[14px] text-white/55">{err()}</p>
          <button type="button" class={`mt-4 ${chip(false)}`} onClick={() => void refetch()}>
            Try again
          </button>
        </Centered>
        )}
      </Show>
      <Show when={!graph() && !loadError()}>
        <Centered>
          <Loader size={8} pattern="ripple" label="Loading the knowledge graph" class="text-white/55" />
        </Centered>
      </Show>
      <Show when={scope() && scope()!.own.size === 0}>
        <Centered>
          <p class="text-[15px] font-medium">{lecture() ? 'No concepts mapped for this lecture yet' : 'No concepts mapped yet'}</p>
          <p class="mt-1 max-w-sm text-[14px] leading-snug text-white/55">
            The graph fills in from your lectures once they are processed. Study with ChatGPT to see what you understood about each idea.
          </p>
        </Centered>
      </Show>

      {/* What is learned, and what each color means. */}
      <Show when={counts().total > 0}>
        <div class={`absolute bottom-3 left-3 w-[min(17rem,calc(100%-1.5rem))] rounded-2xl p-3 sm:bottom-4 sm:left-4 ${glass} ${concept() ? 'max-sm:hidden' : ''}`}>
          <div class="flex items-baseline justify-between gap-2">
            <span class="text-[13px] text-white/60">Learned</span>
            <span class="text-[13px] tabular-nums">
              <span class="text-[17px] font-semibold">{counts().learned}</span>
              <span class="text-white/55"> of {counts().total}</span>
            </span>
          </div>
          <div class="mt-2 flex h-1.5 overflow-hidden rounded-full bg-white/8">
            <For each={LEVELS}>
              {(l) => <div style={{ width: `${(counts().out[l.level] / counts().total) * 100}%`, background: l.color, opacity: l.level === 'unseen' ? 0.35 : 1 }} />}
            </For>
          </div>
          <ul class="mt-2.5 grid grid-cols-2 gap-x-3 gap-y-1">
            <For each={LEVELS}>
              {(l) => (
                <li class={`flex items-center gap-1.5 text-[12px] ${counts().out[l.level] ? 'text-white/80' : 'text-white/35'}`}>
                  <LevelDot level={l.level} />
                  <span class="min-w-0 flex-1 truncate">{l.label}</span>
                  <span class="tabular-nums text-white/50">{counts().out[l.level]}</span>
                </li>
              )}
            </For>
          </ul>
        </div>
      </Show>

      {/* The selected concept. */}
      <Show when={concept()}>
        {(c) => (
          <aside
            class={`scroll-quiet absolute inset-x-3 bottom-3 max-h-[55%] overflow-y-auto rounded-2xl p-4 sm:inset-x-auto sm:right-4 sm:bottom-auto sm:top-16 sm:max-h-[calc(100%-5rem)] sm:w-80 ${glass}`}
            aria-label={c().name}
          >
            <div class="flex items-start gap-2">
              <div class="min-w-0 flex-1">
                <h2 class="text-[16px] font-semibold leading-snug">{c().name}</h2>
                <div class="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-[12px]">
                  <span class="flex items-center gap-1.5" style={{ color: LEVEL[c().level].color }}>
                    <LevelDot level={c().level} />
                    {LEVEL[c().level].label}
                  </span>
                  <Show when={c().score != null}>
                    <span class="text-white/50">{Math.round(c().score! * 100)}%</span>
                  </Show>
                  <Show when={c().review_due}>
                    <span class="rounded-full bg-amber-400/15 px-2 py-0.5 text-amber-300">Due for review</span>
                  </Show>
                </div>
              </div>
              <button type="button" class="-mr-1 -mt-1 flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-white/55 hover:bg-white/10 hover:text-white" aria-label="Close" onClick={() => setSelected(null)}>
                <X size={16} stroke-width={1.8} />
              </button>
            </div>

            <Show when={c().summary}>
              <p class="mt-3 text-[14px] leading-snug text-white/80">{c().summary}</p>
            </Show>

            <p class="mt-3 text-[13px] text-white/55">
              <Show
                when={!c().prior}
                fallback="Background knowledge the lectures build on."
              >
                <Show when={c().lecture} fallback="Not tied to a lecture section.">
                  <A href={materialPath(courseId(), c().lecture!)} class="text-white/80 underline decoration-white/25 underline-offset-2 hover:decoration-white">
                    {lectureTitle(c().lecture)}
                  </A>
                  <Show when={c().sections.length}>{`, section ${c().sections.join(', ')}`}</Show>
                </Show>
              </Show>
              <Show when={c().checks > 0}>{` · checked ${c().checks}×`}</Show>
            </p>

            <Issues title="Misconceptions" items={c().misconceptions} tone="text-rose-300" />
            <Issues title="Missing pieces" items={c().gaps} tone="text-amber-200" />
            <Related title="Builds on" items={related(c(), 'requires')} onSelect={select} />
            <Related title="Needed for" items={related(c(), 'requiredBy')} onSelect={select} />
          </aside>
        )}
      </Show>
    </div>
  );
}

/** 3D or 2D: in the top bar, or over the scene (`dark`). */
function ViewToggle(props: { dims: 2 | 3; onChange: (d: 2 | 3) => void; dark?: boolean; class?: string }) {
  return (
    <div
      class={`flex shrink-0 rounded-full p-1 ${props.dark ? 'border border-white/10 bg-[#0b0f1a]/75 backdrop-blur-md' : 'bg-soft'} ${props.class ?? ''}`}
      role="group"
      aria-label="View"
    >
      <For each={[3, 2] as const}>
        {(d) => (
          <button
            type="button"
            aria-pressed={props.dims === d}
            onClick={() => props.onChange(d)}
            class={`flex h-8 items-center gap-1.5 rounded-full px-2.5 text-[12px] font-medium transition-colors ${
              props.dims === d
                ? props.dark ? 'bg-white/15 text-white' : 'bg-surface text-fg shadow-card'
                : props.dark ? 'text-white/55 hover:text-white' : 'text-muted hover:text-fg'
            }`}
          >
            {d === 3 ? <Box size={14} stroke-width={1.8} /> : <Square size={13} stroke-width={1.8} />}
            {d}D
          </button>
        )}
      </For>
    </div>
  );
}

function Centered(props: { children: import('solid-js').JSX.Element }) {
  return (
    <div class="pointer-events-none absolute inset-0 flex items-center justify-center p-6">
      <div class="pointer-events-auto text-center">{props.children}</div>
    </div>
  );
}

function LevelDot(props: { level: ConceptLevel }) {
  const l = () => LEVEL[props.level];
  return (
    <span
      class="inline-block h-2.5 w-2.5 shrink-0 rounded-full"
      style={
        LEARNED.has(props.level)
          ? { background: l().color, 'box-shadow': `0 0 8px ${l().color}` }
          : props.level === 'unseen'
            ? { border: `1.5px solid ${l().color}` }
            : { background: l().color }
      }
    />
  );
}

function Issues(props: { title: string; items: string[]; tone: string }) {
  return (
    <Show when={props.items.length}>
      <div class="mt-4">
        <h3 class="text-[12px] font-medium uppercase tracking-wide text-white/45">{props.title}</h3>
        <ul class="mt-1.5 space-y-1">
          <For each={props.items}>{(i) => <li class={`text-[13px] leading-snug ${props.tone}`}>{i}</li>}</For>
        </ul>
      </div>
    </Show>
  );
}

function Related(props: { title: string; items: GraphConcept[]; onSelect: (id: string) => void }) {
  return (
    <Show when={props.items.length}>
      <div class="mt-4">
        <h3 class="text-[12px] font-medium uppercase tracking-wide text-white/45">{props.title}</h3>
        <div class="mt-1.5 flex flex-wrap gap-1.5">
          <For each={props.items}>
            {(c) => (
              <button
                type="button"
                class="flex max-w-full items-center gap-1.5 rounded-full bg-white/8 px-2.5 py-1 text-[12px] text-white/85 hover:bg-white/15"
                onClick={() => props.onSelect(c.id)}
              >
                <LevelDot level={c.level} />
                <span class="truncate">{c.name}</span>
              </button>
            )}
          </For>
        </div>
      </div>
    </Show>
  );
}

// ── Three.js objects ────────────────────────────────────────────────────

const end = (v: string | Node): Node | null => (typeof v === 'object' ? v : null);
const endId = (v: string | Node) => (typeof v === 'object' ? v.id : v);
const touches = (l: Link, id: string | null) => !!id && (endId(l.source) === id || endId(l.target) === id);

function linkColor(l: Link, selected: string | null): string {
  if (l.kind !== 'requires') return 'rgba(148,163,184,0.25)';
  if (touches(l, selected)) return '#ffffff';
  const base = end(l.target)?.concept;
  return base && LEARNED.has(base.level) ? 'rgba(52,211,153,0.55)' : 'rgba(148,163,184,0.4)';
}

/** A soft round glow for halos. */
function glowTexture(): THREE.Texture {
  const canvas = document.createElement('canvas');
  canvas.width = canvas.height = 64;
  const ctx = canvas.getContext('2d')!;
  const g = ctx.createRadialGradient(32, 32, 0, 32, 32, 32);
  g.addColorStop(0, 'rgba(255,255,255,1)');
  g.addColorStop(0.25, 'rgba(255,255,255,0.45)');
  g.addColorStop(1, 'rgba(255,255,255,0)');
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, 64, 64);
  return new THREE.CanvasTexture(canvas);
}

const SPHERE = new THREE.SphereGeometry(1, 20, 14);
const RING = new THREE.RingGeometry(1.35, 1.6, 40);

/**
 * Learned: a solid, lit sphere with a halo. In progress: solid in its
 * state's color. Seen but unchecked: translucent. Not learned yet: a hollow
 * wire sphere. A red ring marks a misconception.
 */
function nodeObject(n: Node, glow: THREE.Texture, rings: Map<string, THREE.Object3D>, selected: string | null): THREE.Object3D {
  const group = new THREE.Group();
  const hub = n.kind !== 'concept';
  const level = n.concept?.level ?? 'solid';
  const color = new THREE.Color(n.kind === 'lecture' ? LECTURE_COLOR : n.kind === 'background' ? BACKGROUND_COLOR : LEVEL[level].color);
  const r = hub ? 7 : 2.6 + Math.min(3, Math.sqrt(n.dependents) * 1.1);
  const dim = n.outside ? 0.45 : 1;

  if (hub || LEARNED.has(level) || level === 'shaky' || level === 'struggling') {
    const mesh = new THREE.Mesh(
      SPHERE,
      new THREE.MeshLambertMaterial({ color, emissive: color, emissiveIntensity: LEARNED.has(level) && !hub ? 0.55 : 0.25, transparent: dim < 1, opacity: dim }),
    );
    mesh.scale.setScalar(r);
    group.add(mesh);
  } else if (level === 'introduced') {
    const mesh = new THREE.Mesh(SPHERE, new THREE.MeshLambertMaterial({ color, emissive: color, emissiveIntensity: 0.2, transparent: true, opacity: 0.45 * dim, depthWrite: false }));
    mesh.scale.setScalar(r);
    group.add(mesh);
  } else {
    const wire = new THREE.Mesh(SPHERE, new THREE.MeshBasicMaterial({ color, wireframe: true, transparent: true, opacity: 0.4 * dim }));
    wire.scale.setScalar(r);
    group.add(wire);
    const core = new THREE.Mesh(SPHERE, new THREE.MeshBasicMaterial({ color, transparent: true, opacity: 0.12 * dim, depthWrite: false }));
    core.scale.setScalar(r * 0.95);
    group.add(core);
  }

  // Halo: strong for learned concepts, faint for lectures.
  if (hub || LEARNED.has(level)) {
    const halo = new THREE.Sprite(
      new THREE.SpriteMaterial({ map: glow, color, transparent: true, opacity: (hub ? 0.35 : level === 'mastered' ? 0.9 : 0.6) * dim, blending: THREE.AdditiveBlending, depthWrite: false }),
    );
    halo.scale.setScalar(r * (level === 'mastered' && !hub ? 6 : 4.5));
    group.add(halo);
  }

  if (n.concept?.misconceptions.length) {
    const ring = new THREE.Mesh(RING, new THREE.MeshBasicMaterial({ color: '#fb7185', side: THREE.DoubleSide, transparent: true, opacity: 0.9 * dim }));
    ring.scale.setScalar(r);
    ring.onBeforeRender = (_renderer, _scene, camera) => ring.quaternion.copy(camera.quaternion);
    group.add(ring);
  }

  if (!hub) {
    const sel = new THREE.Mesh(RING, new THREE.MeshBasicMaterial({ color: '#ffffff', side: THREE.DoubleSide, transparent: true, opacity: 0.95, depthWrite: false }));
    sel.onBeforeRender = (_renderer, _scene, camera) => sel.quaternion.copy(camera.quaternion);
    const holder = new THREE.Group();
    holder.add(sel);
    sel.scale.setScalar(r * 1.35);
    holder.scale.setScalar(selected === n.id ? 1 : 0.0001);
    rings.set(n.id, holder);
    group.add(holder);
  }

  const label = new SpriteText(n.name.length > 34 ? `${n.name.slice(0, 33)}…` : n.name, hub ? 4.6 : 3.2, hub ? '#e9e5ff' : 'rgba(255,255,255,0.82)');
  label.fontFace = '-apple-system, BlinkMacSystemFont, Inter, system-ui, sans-serif';
  label.fontWeight = hub ? '600' : '500';
  label.material.depthWrite = false;
  label.material.opacity = (n.kind === 'concept' && (level === 'unseen' || n.outside) ? 0.55 : 1);
  label.position.y = -(r + (hub ? 5 : 3.2));
  group.add(label);
  return group;
}
