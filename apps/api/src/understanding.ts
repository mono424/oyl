import type { Env } from './env';
import { sha256Hex } from './auth';
import { ensureServerSchema, getOwnedCourse } from './service';
import { BadRequest, NotFound, Surreal, recordId } from './surreal';
import type { SectionGraph } from './memory';

/**
 * What the student understood, concept by concept: the student's side of the
 * course knowledge graph (`concept`, `requires` and `concept_evidence` in
 * server-schema.surql).
 *
 * The API calls no language model, so ChatGPT does the reading: while it
 * tutors it maps the small ideas of each section it reads (`concepts`, with
 * the ideas each one builds on) and reports what the student actually said
 * about them (`observations`). The level of a concept is never set directly:
 * it follows from the evidence here (applyObservation), so "I get it" or an
 * explanation alone never counts as understanding.
 *
 * From that graph, studentModel tells the tutor three things: what to explain
 * first because the next material builds on it (explain_first), what to work
 * on (focus), and what is still missing for the full picture (missing).
 *
 * Every query runs as the API's database user and filters by owner itself.
 */

export const OUTCOMES = ['explained', 'asked', 'unsure', 'wrong', 'partial', 'recalled', 'explained_back', 'applied'] as const;
export type Outcome = (typeof OUTCOMES)[number];

export const LEVELS = ['unseen', 'introduced', 'struggling', 'shaky', 'solid', 'mastered'] as const;
export type Level = (typeof LEVELS)[number];

/**
 * Graded outcomes: how much understanding the answer shows (target) and how
 * much one answer says about it (weight). `explained` and `asked` are not
 * graded: being told something or asking about it shows no understanding.
 */
const GRADES: Partial<Record<Outcome, { target: number; weight: number }>> = {
  unsure: { target: 0.15, weight: 0.6 },
  wrong: { target: 0.05, weight: 1 },
  partial: { target: 0.5, weight: 1 },
  recalled: { target: 0.75, weight: 0.8 },
  explained_back: { target: 0.9, weight: 1 },
  applied: { target: 1, weight: 1.2 },
};
const SUCCESS = new Set<Outcome>(['recalled', 'explained_back', 'applied']);
const DEEP = new Set<Outcome>(['explained_back', 'applied']);
/** Where a concept starts before its first graded answer. */
const PRIOR_SCORE = 0.25;
/** Mastery needs successes this far apart: knowing it again later, not only right after hearing it. */
const RETAINED_MS = 20 * 60 * 60 * 1000;
/** Days after the last check before a concept at this level is due for review. */
const REVIEW_DAYS: Partial<Record<Level, number>> = { struggling: 0, shaky: 1, solid: 4, mastered: 14 };
const WEAK = new Set<Level>(['unseen', 'introduced', 'struggling', 'shaky']);
const MAX_ISSUES = 8;

export interface ConceptState {
  level: Level;
  score: number | null;
  checks: number;
  successes: number;
  deep_successes: number;
  last_outcome: Outcome | null;
  explained_at: string | null;
  last_checked_at: string | null;
  first_success_at: string | null;
  last_success_at: string | null;
  misconceptions: string[];
  gaps: string[];
}

export interface Observation {
  outcome: Outcome;
  misconception?: string | null;
  gap?: string | null;
  /** Misconceptions or gaps the student no longer shows, as listed on the concept. */
  resolved?: string[] | null;
}

export const emptyState = (): ConceptState => ({
  level: 'unseen', score: null, checks: 0, successes: 0, deep_successes: 0, last_outcome: null,
  explained_at: null, last_checked_at: null, first_success_at: null, last_success_at: null, misconceptions: [], gaps: [],
});

/** Folds a concept name for matching: case, accents, punctuation and spacing do not matter. */
export function conceptKey(name: string): string {
  return name.normalize('NFKD').replace(/\p{M}/gu, '').toLowerCase().replace(/[^\p{L}\p{N}]+/gu, ' ').trim();
}

const rank = (level: Level) => LEVELS.indexOf(level);

export function levelOf(s: ConceptState): Level {
  if (!s.checks) return s.explained_at ? 'introduced' : 'unseen';
  const score = s.score ?? PRIOR_SCORE;
  let level: Level = score < 0.35 ? 'struggling' : score < 0.65 ? 'shaky' : 'solid';
  const retained = !!s.first_success_at && !!s.last_success_at &&
    Date.parse(s.last_success_at) - Date.parse(s.first_success_at) >= RETAINED_MS;
  if (level === 'solid' && score >= 0.85 && s.deep_successes >= 2 && retained) level = 'mastered';
  // A wrong belief the student still holds caps what they can be said to understand.
  if (s.misconceptions.length && rank(level) > rank('shaky')) level = 'shaky';
  return level;
}

function withIssue(list: string[], issue: string | null | undefined): string[] {
  const text = issue?.trim();
  if (!text) return list;
  const key = conceptKey(text);
  const rest = list.filter((i) => conceptKey(i) !== key);
  return [...rest, text].slice(-MAX_ISSUES);
}

/** The concept after one observation. Pure, so the rules are easy to test. */
export function applyObservation(s: ConceptState, o: Observation, at: string): ConceptState {
  const resolved = new Set((o.resolved ?? []).map(conceptKey).filter(Boolean));
  const next: ConceptState = {
    ...s,
    misconceptions: withIssue(s.misconceptions.filter((i) => !resolved.has(conceptKey(i))), o.misconception),
    gaps: withIssue(s.gaps.filter((i) => !resolved.has(conceptKey(i))), o.gap),
    last_outcome: o.outcome,
  };
  if (o.outcome === 'explained') next.explained_at = at;
  const grade = GRADES[o.outcome];
  if (grade) {
    const score = s.score ?? PRIOR_SCORE;
    // The first answer moves the estimate further than later ones.
    const rate = Math.min(1, (s.checks ? 0.4 : 0.6) * grade.weight);
    next.score = Math.round((score + rate * (grade.target - score)) * 1000) / 1000;
    next.checks = s.checks + 1;
    next.last_checked_at = at;
    if (SUCCESS.has(o.outcome)) {
      next.successes = s.successes + 1;
      next.first_success_at = s.first_success_at ?? at;
      next.last_success_at = at;
      if (DEEP.has(o.outcome)) next.deep_successes = s.deep_successes + 1;
    }
  }
  next.level = levelOf(next);
  return next;
}

export function reviewDue(s: Pick<ConceptState, 'level' | 'last_checked_at'>, now: Date): boolean {
  const days = REVIEW_DAYS[s.level];
  if (days === undefined || !s.last_checked_at) return false;
  return Date.parse(s.last_checked_at) + days * 86_400_000 <= now.getTime();
}

// ── Rows ─────────────────────────────────────────────────────────────────

interface ConceptRow extends ConceptState {
  id: string;
  course: string;
  key: string;
  name: string;
  summary?: string | null;
  prior: boolean;
  parent?: string | null;
  material?: string | null;
  rev?: number | null;
  positions: number[];
  version: number;
}

interface LectureRow {
  id: string;
  title: string;
  status: string;
  index_rev: number;
  chunk_count: number;
  learned_through?: number | null;
}

interface Graph {
  lectures: Map<string, LectureRow>;
  concepts: ConceptRow[];
  byId: Map<string, ConceptRow>;
  /** concept -> the concepts it builds on, and the reverse. */
  requires: Map<string, Set<string>>;
  requiredBy: Map<string, Set<string>>;
}

const CONCEPT_FIELDS = `id, course, key, name, summary, prior, parent, material, rev, positions, level, score, checks, successes, deep_successes,
  last_outcome, explained_at, last_checked_at, first_success_at, last_success_at, misconceptions, gaps, version`;

function normalizeRow(row: ConceptRow): ConceptRow {
  return {
    ...row,
    summary: row.summary ?? null,
    parent: row.parent ?? null,
    material: row.material ?? null,
    rev: row.rev ?? null,
    positions: row.positions ?? [],
    score: row.score ?? null,
    last_outcome: row.last_outcome ?? null,
    explained_at: row.explained_at ?? null,
    last_checked_at: row.last_checked_at ?? null,
    first_success_at: row.first_success_at ?? null,
    last_success_at: row.last_success_at ?? null,
    misconceptions: row.misconceptions ?? [],
    gaps: row.gaps ?? [],
  };
}

async function loadGraph(env: Env, ownerId: string, courseId: string): Promise<Graph> {
  const [lectures, concepts, edges] = await Surreal.system(env).queryAll(
    `SELECT id, title, status, index_rev, chunk_count, learned_through, created_at FROM material
       WHERE owner = type::record($owner) AND course = type::record($course) ORDER BY created_at;
     SELECT ${CONCEPT_FIELDS} FROM concept WHERE owner = type::record($owner) AND course = type::record($course);
     SELECT in, out FROM requires WHERE owner = type::record($owner) AND course = type::record($course);`,
    { owner: ownerId, course: courseId },
  ) as [LectureRow[], ConceptRow[], { in: string; out: string }[]];
  const rows = concepts.map(normalizeRow);
  const graph: Graph = {
    lectures: new Map(lectures.map((l) => [l.id, l])),
    concepts: rows,
    byId: new Map(rows.map((c) => [c.id, c])),
    requires: new Map(),
    requiredBy: new Map(),
  };
  for (const e of edges) link(graph, e.in, e.out);
  return graph;
}

function link(graph: Graph, from: string, to: string) {
  if (!graph.requires.has(from)) graph.requires.set(from, new Set());
  if (!graph.requiredBy.has(to)) graph.requiredBy.set(to, new Set());
  graph.requires.get(from)!.add(to);
  graph.requiredBy.get(to)!.add(from);
}

/** The concept's sections, when they belong to the lecture's current text. */
function anchor(graph: Graph, c: ConceptRow): { lecture: LectureRow; positions: number[] } | null {
  const lecture = c.material ? graph.lectures.get(c.material) : undefined;
  if (!lecture || lecture.status !== 'ready' || c.rev !== lecture.index_rev) return null;
  return { lecture, positions: c.positions.filter((p) => p < lecture.chunk_count) };
}

function reached(graph: Graph, c: ConceptRow): boolean {
  const a = anchor(graph, c);
  return !!a && a.positions.some((p) => p <= (a.lecture.learned_through ?? -1));
}

/** Concept ids, mint-free: the same name in the same course is always the same record. */
async function conceptIdFor(courseId: string, key: string): Promise<string> {
  return `concept:k${(await sha256Hex(`${courseId}\n${key}`)).slice(0, 32)}`;
}

// ── Writing ──────────────────────────────────────────────────────────────

export interface ConceptInput {
  name: string;
  summary?: string | null;
  material_id?: string | null;
  position?: number | null;
  prior?: boolean | null;
  requires?: string[] | null;
  drop_requires?: string[] | null;
  part_of?: string | null;
}

export interface ObservationInput {
  concept: string;
  outcome: Outcome;
  note: string;
  material_id?: string | null;
  position?: number | null;
  misconception?: string | null;
  gap?: string | null;
  resolved?: string[] | null;
}

const MAX_CONCEPTS = 40;
const MAX_OBSERVATIONS = 30;
const MAX_NAME = 120;
const MAX_TEXT = 400;

const clip = (s: string | null | undefined, n = MAX_TEXT) => {
  const t = s?.trim();
  return t ? t.slice(0, n) : null;
};

/**
 * Adds to the concept map of a course and records what the student showed.
 * Concepts are matched by id or by name (conceptKey); a name not seen before
 * becomes a new concept. Concurrent writes to the same concept retry.
 */
export async function recordUnderstanding(env: Env, ownerId: string, input: {
  courseId: string;
  concepts?: ConceptInput[] | null;
  observations?: ObservationInput[] | null;
  now?: Date;
  /** Leave a concept where it is when it already belongs to another lecture. */
  keepAnchors?: boolean;
}) {
  const concepts = input.concepts ?? [];
  const observations = input.observations ?? [];
  if (!concepts.length && !observations.length) throw new BadRequest('Pass concepts to map, observations of the student, or both.');
  if (concepts.length > MAX_CONCEPTS) throw new BadRequest(`Map at most ${MAX_CONCEPTS} concepts per call.`);
  if (observations.length > MAX_OBSERVATIONS) throw new BadRequest(`Record at most ${MAX_OBSERVATIONS} observations per call.`);
  for (const o of observations) {
    if (!OUTCOMES.includes(o.outcome)) throw new BadRequest(`outcome must be one of ${OUTCOMES.join(', ')}.`);
    if (!o.note?.trim()) throw new BadRequest('Each observation needs a note: what the student actually said or did.');
  }
  await ensureServerSchema(env);
  const course = await getOwnedCourse(env, ownerId, input.courseId);

  for (let attempt = 0; ; attempt++) {
    try {
      return await writeOnce(env, ownerId, course.id, concepts, observations, input.now ?? new Date(), input.keepAnchors ?? false);
    } catch (err) {
      const retry = err instanceof Error && /concept_conflict|already exists|Transaction conflict|can be retried/i.test(err.message);
      if (!retry || attempt >= 3) throw err;
    }
  }
}

async function writeOnce(
  env: Env, ownerId: string, courseId: string, concepts: ConceptInput[], observations: ObservationInput[], now: Date, keepAnchors: boolean,
) {
  const graph = await loadGraph(env, ownerId, courseId);
  const at = now.toISOString();
  const touched = new Map<string, ConceptRow>();
  const created = new Set<string>();
  const byKey = new Map(graph.concepts.map((c) => [c.key, c]));

  const lectureAt = (materialId: string | null | undefined, position: number | null | undefined) => {
    if (!materialId) {
      if (position != null) throw new BadRequest('position needs the lecture (material_id) it belongs to.');
      return null;
    }
    const lecture = graph.lectures.get(recordId('material', materialId));
    if (!lecture) throw new NotFound('That lecture is not in this course.');
    if (lecture.status !== 'ready') throw new BadRequest(`That lecture is not ready yet (${lecture.status}).`);
    if (position != null && (!Number.isInteger(position) || position < 0 || position >= lecture.chunk_count)) {
      throw new BadRequest(`Choose a section from 0 to ${lecture.chunk_count - 1} of "${lecture.title}".`);
    }
    return { lecture, position: position ?? null };
  };

  /** The concept a reference names: an id from this course, or a name (new names make new concepts). */
  const resolve = async (ref: string): Promise<ConceptRow> => {
    const raw = ref.trim();
    if (/^concept:/.test(raw)) {
      const hit = touched.get(raw) ?? graph.byId.get(raw);
      if (!hit) throw new NotFound(`Concept ${raw} is not in this course. Use its name instead.`);
      touched.set(hit.id, hit);
      return hit;
    }
    const name = clip(raw, MAX_NAME);
    const key = name ? conceptKey(name) : '';
    if (!name || !key) throw new BadRequest('A concept needs a name.');
    const known = byKey.get(key);
    if (known) {
      touched.set(known.id, touched.get(known.id) ?? known);
      return touched.get(known.id)!;
    }
    const id = await conceptIdFor(courseId, key);
    const row: ConceptRow = {
      ...emptyState(), id, course: courseId, key, name, summary: null, prior: false, parent: null,
      material: null, rev: null, positions: [], version: -1,
    };
    byKey.set(key, row);
    touched.set(id, row);
    created.add(id);
    return row;
  };

  const anchorTo = (row: ConceptRow, place: ReturnType<typeof lectureAt>) => {
    if (!place) return row;
    if (keepAnchors && row.material && row.material !== place.lecture.id && graph.lectures.has(row.material)) return row;
    const same = row.material === place.lecture.id && row.rev === place.lecture.index_rev;
    const positions = same ? row.positions : [];
    return {
      ...row,
      material: place.lecture.id,
      rev: place.lecture.index_rev,
      positions: place.position == null || positions.includes(place.position)
        ? positions
        : [...positions, place.position].sort((a, b) => a - b).slice(-12),
    };
  };

  const put = (row: ConceptRow) => {
    touched.set(row.id, row);
    byKey.set(row.key, row);
  };

  const addEdges: [string, string][] = [];
  const dropEdges: [string, string][] = [];

  for (const c of concepts) {
    let row = await resolve(c.name);
    const place = lectureAt(c.material_id, c.position);
    row = anchorTo(row, place);
    if (clip(c.summary) && !(keepAnchors && row.summary)) row = { ...row, summary: clip(c.summary) };
    if (c.prior != null) row = { ...row, prior: c.prior };
    if (c.part_of) {
      const parent = await resolve(c.part_of);
      if (parent.id !== row.id) row = { ...row, parent: parent.id };
    }
    put(row);
    for (const ref of c.requires ?? []) {
      const base = await resolve(ref);
      if (base.id === row.id) continue;
      // A direct cycle is a mistake in the map: keep the edge that came first.
      if (graph.requires.get(base.id)?.has(row.id)) continue;
      addEdges.push([row.id, base.id]);
      link(graph, row.id, base.id);
    }
    for (const ref of c.drop_requires ?? []) {
      const base = await resolve(ref);
      dropEdges.push([row.id, base.id]);
      graph.requires.get(row.id)?.delete(base.id);
      graph.requiredBy.get(base.id)?.delete(row.id);
    }
  }

  const evidence: Record<string, unknown>[] = [];
  for (const o of observations) {
    let row = await resolve(o.concept);
    const place = lectureAt(o.material_id, o.position);
    row = anchorTo(row, place);
    const state = applyObservation(row, o, at);
    row = { ...row, ...state };
    put(row);
    evidence.push({
      id: `concept_evidence:k${crypto.randomUUID().replaceAll('-', '')}`,
      concept: row.id,
      material: place?.lecture.id ?? null,
      position: place?.position ?? null,
      outcome: o.outcome,
      note: clip(o.note)!,
      misconception: clip(o.misconception),
      gap: clip(o.gap),
      resolved: (o.resolved ?? []).map((r) => clip(r)).filter(Boolean),
      score_after: state.score,
      level_after: state.level,
      at,
    });
  }

  // Concepts only referenced (a prerequisite, a parent) still get a level.
  const writes = [...touched.values()].map((row) => ({
    ...row,
    level: levelOf(row),
    create: created.has(row.id),
  }));
  const edgeRows = await Promise.all(addEdges.map(async ([from, to]) => ({
    id: `requires:k${(await sha256Hex(`${from}\n${to}`)).slice(0, 32)}`, in: from, out: to,
  })));

  await Surreal.system(env).query(
    `BEGIN TRANSACTION;
     FOR $w IN $writes {
       IF $w.create {
         CREATE type::record($w.id) SET ${SET_CONCEPT};
       } ELSE {
         LET $done = UPDATE type::record($w.id) SET ${SET_CONCEPT}
           WHERE owner = type::record($owner) AND course = type::record($course) AND version = $w.version RETURN id;
         IF array::len($done) = 0 { THROW 'concept_conflict' };
       };
     };
     FOR $e IN $edges {
       IF array::len(SELECT id FROM requires WHERE in = type::record($e.in) AND out = type::record($e.out)) = 0 {
         RELATE (type::record($e.in))->(type::record($e.id))->(type::record($e.out))
           SET owner = type::record($owner), course = type::record($course);
       };
     };
     FOR $d IN $drops {
       DELETE requires WHERE in = type::record($d[0]) AND out = type::record($d[1]) AND owner = type::record($owner);
     };
     FOR $e IN $evidence {
       CREATE type::record($e.id) SET owner = type::record($owner), course = type::record($course),
         concept = type::record($e.concept), material = IF $e.material THEN type::record($e.material) END,
         position = $e.position, outcome = $e.outcome, note = $e.note, misconception = $e.misconception, gap = $e.gap,
         resolved = $e.resolved, score_after = IF $e.score_after != NONE THEN <float>$e.score_after END,
         level_after = $e.level_after, at = <datetime>$e.at;
     };
     COMMIT TRANSACTION;`,
    { owner: ownerId, course: courseId, writes, edges: edgeRows, drops: dropEdges, evidence },
  );

  return {
    concepts: writes.map((w) => ({
      id: w.id,
      name: w.name,
      level: w.level,
      ...(w.score != null ? { score: w.score } : {}),
      ...(w.misconceptions.length ? { misconceptions: w.misconceptions } : {}),
      ...(w.gaps.length ? { gaps: w.gaps } : {}),
      ...(w.create ? { new: true } : {}),
    })),
    observations_recorded: evidence.length,
    ...(addEdges.length ? { links_added: addEdges.length } : {}),
  };
}

const SET_CONCEPT = `owner = type::record($owner), course = type::record($course), key = $w.key, name = $w.name,
  summary = $w.summary, prior = $w.prior ?? false, parent = IF $w.parent THEN type::record($w.parent) END,
  material = IF $w.material THEN type::record($w.material) END, rev = $w.rev, positions = $w.positions ?? [],
  level = $w.level, score = IF $w.score != NONE THEN <float>$w.score END,
  checks = $w.checks, successes = $w.successes, deep_successes = $w.deep_successes, last_outcome = $w.last_outcome,
  explained_at = IF $w.explained_at THEN <datetime>$w.explained_at END,
  last_checked_at = IF $w.last_checked_at THEN <datetime>$w.last_checked_at END,
  first_success_at = IF $w.first_success_at THEN <datetime>$w.first_success_at END,
  last_success_at = IF $w.last_success_at THEN <datetime>$w.last_success_at END,
  misconceptions = $w.misconceptions ?? [], gaps = $w.gaps ?? [], version = $w.version + 1`;

/**
 * Adds the concepts Agent Memory extracted from a lecture, section by
 * section (`graphs[position]`), to the course's concept map, with what they
 * build on and are part of. New concepts start unseen; what the student
 * showed about known ones stays, and so does a concept another lecture
 * already teaches.
 */
export async function mapConcepts(env: Env, ownerId: string, courseId: string, materialId: string, graphs: SectionGraph[]): Promise<number> {
  const entries: ConceptInput[] = [];
  graphs.forEach((g, position) => {
    const byKey = new Map<string, ConceptInput>();
    for (const name of g.concepts) {
      const key = conceptKey(name);
      if (key && !byKey.has(key)) byKey.set(key, { name, summary: g.summaries?.[name] ?? null, material_id: materialId, position, requires: [] });
    }
    for (const [from, to] of g.requires) byKey.get(conceptKey(from))?.requires!.push(to);
    for (const [from, to] of g.partOf) {
      const c = byKey.get(conceptKey(from));
      if (c && !c.part_of && conceptKey(to) !== conceptKey(from)) c.part_of = to;
    }
    entries.push(...byKey.values());
  });
  for (let i = 0; i < entries.length; i += MAX_CONCEPTS) {
    await recordUnderstanding(env, ownerId, { courseId, concepts: entries.slice(i, i + MAX_CONCEPTS), keepAnchors: true });
  }
  return entries.length;
}

// ── Reading ──────────────────────────────────────────────────────────────

const round = (n: number | null) => (n == null ? null : Math.round(n * 100) / 100);

function where(graph: Graph, c: ConceptRow): string | null {
  if (c.prior) return 'background knowledge';
  const a = anchor(graph, c);
  if (!a || !a.positions.length) return a ? a.lecture.title : null;
  return `${a.lecture.title}, section ${a.positions.map((p) => p + 1).join(', ')}`;
}

function daysAgo(iso: string | null, now: Date): string | null {
  if (!iso) return null;
  const days = Math.floor((now.getTime() - Date.parse(iso)) / 86_400_000);
  return days <= 0 ? 'today' : days === 1 ? 'yesterday' : `${days} days ago`;
}

/**
 * The student model of a course or one lecture: what to explain first, what
 * to focus on and what is missing, and with `full` every concept in scope.
 * `topics` are concept names or ids to plan for instead of the next sections.
 */
export async function studentModel(env: Env, ownerId: string, input: {
  courseId: string;
  materialId?: string | null;
  topics?: string[] | null;
  full?: boolean;
  now?: Date;
}) {
  await ensureServerSchema(env);
  const course = await getOwnedCourse(env, ownerId, input.courseId);
  const graph = await loadGraph(env, ownerId, course.id);
  const now = input.now ?? new Date();
  const materialId = input.materialId ? recordId('material', input.materialId) : null;
  if (materialId && !graph.lectures.has(materialId)) throw new NotFound('That lecture is not in this course.');

  const lectures = [...graph.lectures.values()].filter((l) => l.status === 'ready' && l.chunk_count > 0 && (!materialId || l.id === materialId));
  const name = (id: string) => graph.byId.get(id)?.name ?? id;

  // What comes next: the given topics, or the concepts of each lecture's next sections.
  const unknownTopics: string[] = [];
  let targets: ConceptRow[] = [];
  if (input.topics?.length) {
    for (const t of input.topics) {
      const hit = graph.byId.get(t.trim()) ?? graph.concepts.find((c) => c.key === conceptKey(t));
      if (hit) targets.push(hit);
      else unknownTopics.push(t);
    }
  } else {
    for (const l of lectures) {
      const from = (l.learned_through ?? -1) + 1;
      targets.push(...graph.concepts.filter((c) => {
        const a = anchor(graph, c);
        return a?.lecture.id === l.id && a.positions.some((p) => p >= from && p <= from + 1);
      }));
    }
  }
  targets = [...new Map(targets.map((t) => [t.id, t])).values()];

  // Scope: a lecture's concepts and everything they build on, or the whole course.
  let scope: ConceptRow[];
  if (materialId) {
    const ids = new Set(graph.concepts.filter((c) => anchor(graph, c)?.lecture.id === materialId).map((c) => c.id));
    for (const t of targets) ids.add(t.id);
    const queue = [...ids];
    while (queue.length) {
      for (const base of graph.requires.get(queue.shift()!) ?? []) {
        if (!ids.has(base)) { ids.add(base); queue.push(base); }
      }
    }
    scope = [...ids].map((id) => graph.byId.get(id)!).filter(Boolean);
  } else {
    scope = graph.concepts;
  }

  // 1. Explain first: weak foundations of what comes next, deepest first.
  const needed = new Map<string, { depth: number; for: Set<string> }>();
  for (const t of targets) {
    const seen = new Set([t.id]);
    let frontier = [t.id];
    for (let depth = 1; depth <= 4 && frontier.length; depth++) {
      const next: string[] = [];
      for (const id of frontier) {
        for (const base of graph.requires.get(id) ?? []) {
          if (seen.has(base)) continue;
          seen.add(base);
          next.push(base);
          const entry = needed.get(base) ?? { depth, for: new Set<string>() };
          entry.depth = Math.max(entry.depth, depth);
          entry.for.add(t.name);
          needed.set(base, entry);
        }
      }
      frontier = next;
    }
  }
  const explainFirst = [...needed.entries()]
    .map(([id, n]) => ({ c: graph.byId.get(id)!, ...n }))
    .filter(({ c }) => c && (WEAK.has(c.level) || c.misconceptions.length > 0 || reviewDue(c, now)))
    .sort((a, b) => b.depth - a.depth || rank(a.c.level) - rank(b.c.level))
    .slice(0, 8)
    .map(({ c, for: f }) => ({
      concept: c.name,
      id: c.id,
      level: c.level,
      needed_for: [...f].slice(0, 3),
      ...(c.prior ? { background: true } : {}),
      ...(c.misconceptions.length ? { misconceptions: c.misconceptions } : {}),
      ...(where(graph, c) ? { where: where(graph, c) } : {}),
    }));

  // 2. Focus: what the student got wrong, is unsure about or is due to review.
  const dependents = (c: ConceptRow) => graph.requiredBy.get(c.id)?.size ?? 0;
  const focus = scope
    .filter((c) => c.checks > 0 || c.misconceptions.length > 0)
    .map((c) => {
      const due = reviewDue(c, now);
      const reasons: string[] = [];
      if (c.misconceptions.length) reasons.push('holds a misconception');
      if (c.level === 'struggling') reasons.push('struggling');
      else if (c.level === 'shaky') reasons.push('shaky');
      if (c.last_outcome === 'wrong' || c.last_outcome === 'unsure') reasons.push(`last answer ${c.last_outcome === 'wrong' ? 'wrong' : 'unsure'}`);
      if (due) reasons.push(`due for review (${c.level}, last checked ${daysAgo(c.last_checked_at, now)})`);
      if (dependents(c) >= 2) reasons.push(`${dependents(c)} other concepts build on it`);
      const priority = (1 - (c.score ?? PRIOR_SCORE)) + 0.4 * Math.min(c.misconceptions.length, 2) +
        0.1 * Math.min(dependents(c), 5) + (due ? 0.25 : 0);
      return { c, due, reasons, priority };
    })
    .filter(({ c, due }) => due || c.misconceptions.length > 0 || c.level === 'struggling' || c.level === 'shaky')
    .sort((a, b) => b.priority - a.priority)
    .slice(0, 6)
    .map(({ c, reasons }) => ({
      concept: c.name,
      id: c.id,
      level: c.level,
      score: round(c.score),
      why: reasons.join(', '),
      ...(c.misconceptions.length ? { misconceptions: c.misconceptions } : {}),
      ...(c.gaps.length ? { gaps: c.gaps } : {}),
      ...(where(graph, c) ? { where: where(graph, c) } : {}),
    }));

  // 3. Missing for the full picture: open gaps, ideas seen but never
  // checked, and reached sections with no concepts mapped yet.
  const gaps = scope.flatMap((c) => c.gaps.map((gap) => ({ concept: c.name, id: c.id, gap }))).slice(0, 10);
  const neverChecked = scope
    .filter((c) => c.checks === 0 && (c.explained_at || reached(graph, c)))
    .slice(0, 10)
    .map((c) => ({ concept: c.name, id: c.id, level: c.level, ...(where(graph, c) ? { where: where(graph, c) } : {}) }));
  const unmapped = lectures.flatMap((l) => {
    const through = l.learned_through ?? -1;
    if (through < 0) return [];
    const mapped = new Set(graph.concepts.flatMap((c) => {
      const a = anchor(graph, c);
      return a?.lecture.id === l.id ? a.positions : [];
    }));
    const positions = Array.from({ length: through + 1 }, (_, p) => p).filter((p) => !mapped.has(p));
    return positions.length ? [{ lecture: l.title, material_id: l.id, positions: positions.slice(0, 12), more: Math.max(0, positions.length - 12) || undefined }] : [];
  });
  const nextUnmapped = !input.topics?.length && lectures.some((l) => {
    const from = (l.learned_through ?? -1) + 1;
    return from < l.chunk_count && !targets.some((t) => anchor(graph, t)?.lecture.id === l.id);
  });

  const byLevel = Object.fromEntries(LEVELS.map((lv) => [lv, scope.filter((c) => c.level === lv).length]).filter(([, n]) => n));
  const notes: string[] = [];
  if (!graph.concepts.length) {
    notes.push('No concepts are mapped for this course yet. As you read each section, add its concepts with record_understanding (concepts), then record what the student shows (observations).');
  } else if (nextUnmapped) {
    notes.push('The next section has no concepts mapped yet: after reading it, map its concepts and what they build on, then call this again to see which foundations to explain first.');
  }
  if (unmapped.length) notes.push('missing.unmapped_sections are reached sections without concepts: map them when you revisit them.');
  if (unknownTopics.length) notes.push(`Not in the concept map yet: ${unknownTopics.join(', ')}.`);

  return {
    summary: {
      concepts: scope.length,
      ...(Object.keys(byLevel).length ? { by_level: byLevel } : {}),
      review_due: scope.filter((c) => reviewDue(c, now)).length,
    },
    ...(targets.length ? { up_next: targets.slice(0, 8).map((t) => t.name) } : {}),
    explain_first: explainFirst,
    focus,
    missing: {
      gaps,
      never_checked: neverChecked,
      ...(unmapped.length ? { unmapped_sections: unmapped } : {}),
    },
    ...(input.full
      ? {
          concepts: scope.slice(0, 120).map((c) => ({
            id: c.id,
            name: c.name,
            level: c.level,
            ...(c.score != null ? { score: round(c.score) } : {}),
            ...(c.checks ? { checks: c.checks, last_checked: daysAgo(c.last_checked_at, now) } : {}),
            ...(c.summary ? { summary: c.summary } : {}),
            ...(where(graph, c) ? { where: where(graph, c) } : {}),
            ...(graph.requires.get(c.id)?.size ? { requires: [...graph.requires.get(c.id)!].map(name) } : {}),
            ...(c.parent ? { part_of: name(c.parent) } : {}),
            ...(c.misconceptions.length ? { misconceptions: c.misconceptions } : {}),
            ...(c.gaps.length ? { gaps: c.gaps } : {}),
          })),
        }
      : { known_concepts: scope.slice(0, 60).map((c) => c.name) }),
    ...(notes.length ? { notes } : {}),
  };
}

/** One concept with its evidence, newest first, for `fetch`. */
export async function conceptDetail(env: Env, ownerId: string, conceptId: string, now = new Date()) {
  await ensureServerSchema(env);
  const id = recordId('concept', conceptId);
  const [rows, evidence, requires, requiredBy] = await Surreal.system(env).queryAll(
    `SELECT ${CONCEPT_FIELDS}, material.title AS lecture, course.title AS course_title FROM type::record($id) WHERE owner = type::record($owner);
     SELECT outcome, note, misconception, gap, resolved, level_after, at, call FROM concept_evidence
       WHERE concept = type::record($id) AND owner = type::record($owner) ORDER BY at DESC LIMIT 30;
     SELECT VALUE out.name FROM requires WHERE in = type::record($id) AND owner = type::record($owner);
     SELECT VALUE in.name FROM requires WHERE out = type::record($id) AND owner = type::record($owner);`,
    { id, owner: ownerId },
  ) as [(ConceptRow & { lecture?: string | null; course_title?: string | null })[], {
    outcome: string; note: string; misconception?: string; gap?: string; resolved?: string[]; level_after: string; at: string; call?: string;
  }[], string[], string[]];
  const c = rows[0];
  if (!c) throw new NotFound('Concept not found.');
  const row = normalizeRow(c);
  return {
    id: row.id,
    course_id: row.course,
    course: c.course_title ?? null,
    name: row.name,
    ...(row.summary ? { summary: row.summary } : {}),
    level: row.level,
    score: round(row.score),
    checks: row.checks,
    successes: row.successes,
    review_due: reviewDue(row, now),
    ...(row.prior ? { background: true } : c.lecture ? { lecture: c.lecture, sections: row.positions.map((p) => p + 1) } : {}),
    requires,
    required_by: requiredBy,
    misconceptions: row.misconceptions,
    gaps: row.gaps,
    evidence: evidence.map((e) => ({
      when: e.at,
      outcome: e.outcome,
      note: e.note,
      ...(e.misconception ? { misconception: e.misconception } : {}),
      ...(e.gap ? { gap: e.gap } : {}),
      ...(e.resolved?.length ? { resolved: e.resolved } : {}),
      level_after: e.level_after,
      ...(e.call ? { session: e.call } : {}),
    })),
  };
}

/** Evidence recorded during a session that has just been saved joins that session. */
export async function linkEvidenceToSession(env: Env, ownerId: string, courseId: string, callId: string, since: string) {
  await Surreal.system(env).query(
    `UPDATE concept_evidence SET call = type::record($call)
     WHERE owner = type::record($owner) AND course = type::record($course) AND call = NONE AND at >= <datetime>$since`,
    { owner: ownerId, course: courseId, call: callId, since },
  );
}

/** The evidence of one session, for its recap in `fetch`. */
export async function sessionEvidence(env: Env, ownerId: string, callId: string) {
  return Surreal.system(env).query<{ concept: string; outcome: string; note: string; misconception?: string; gap?: string }[]>(
    `SELECT concept.name AS concept, outcome, note, misconception, gap, at FROM concept_evidence
     WHERE call = type::record($call) AND owner = type::record($owner) ORDER BY at`,
    { owner: ownerId, call: callId },
  );
}

/**
 * The course's concept map for the web app's graph view: every concept with
 * its level, what it builds on and what it is part of, and the lectures that
 * teach them. A concept's lecture is only given while its sections belong to
 * the lecture's current text.
 */
export async function courseGraph(env: Env, ownerId: string, courseId: string, now = new Date()) {
  await ensureServerSchema(env);
  const course = await getOwnedCourse(env, ownerId, courseId);
  const graph = await loadGraph(env, ownerId, course.id);
  return {
    course: course.id,
    lectures: [...graph.lectures.values()].map((l) => ({ id: l.id, title: l.title })),
    concepts: graph.concepts.map((c) => {
      const a = anchor(graph, c);
      return {
        id: c.id,
        name: c.name,
        summary: c.summary ?? null,
        level: c.level,
        score: round(c.score),
        checks: c.checks,
        prior: c.prior,
        parent: c.parent && graph.byId.has(c.parent) ? c.parent : null,
        lecture: a?.lecture.id ?? null,
        sections: a ? a.positions.map((p) => p + 1) : [],
        reached: reached(graph, c),
        review_due: reviewDue(c, now),
        last_checked_at: c.last_checked_at,
        misconceptions: c.misconceptions,
        gaps: c.gaps,
      };
    }),
    requires: [...graph.requires.entries()].flatMap(([from, to]) =>
      [...to].filter((id) => graph.byId.has(from) && graph.byId.has(id)).map((id) => ({ from, to: id }))),
  };
}
