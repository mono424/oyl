import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import type { Env } from '../src/env.ts';
import { ensureServerSchema } from '../src/service.ts';
import { fetchDocument, prepareStudySession, saveStudySession } from '../src/study.ts';
import { Surreal } from '../src/surreal.ts';
import {
  applyObservation, conceptKey, emptyState, recordUnderstanding, reviewDue, studentModel, type ConceptState, type Observation,
} from '../src/understanding.ts';

const day = (n: number, hour = 9) => new Date(Date.UTC(2026, 9, 20 + n, hour)).toISOString();
const run = (steps: [Observation, string][], from: ConceptState = emptyState()) =>
  steps.reduce((s, [o, at]) => applyObservation(s, o, at), from);

test('concept names match across case, accents and punctuation', () => {
  assert.equal(conceptKey('  Kettenregel (Ableitung) '), 'kettenregel ableitung');
  assert.equal(conceptKey('Größe-Ordnung'), conceptKey('grosse ordnung'.replace('ss', 'ß')));
  assert.equal(conceptKey('Bayes’ theorem'), conceptKey('bayes theorem'));
  assert.equal(conceptKey('!!!'), '');
});

test('being told or asking is exposure, never understanding', () => {
  const told = run([[{ outcome: 'explained' }, day(0)], [{ outcome: 'asked' }, day(0)]]);
  assert.equal(told.level, 'introduced');
  assert.equal(told.checks, 0);
  assert.equal(told.score, null);
});

test('levels follow graded answers: one recall is not solid, depth and retention make mastery', () => {
  const once = run([[{ outcome: 'recalled' }, day(0)]]);
  assert.equal(once.level, 'shaky');
  const deep = run([[{ outcome: 'recalled' }, day(0)], [{ outcome: 'explained_back' }, day(0)], [{ outcome: 'applied' }, day(0)]]);
  assert.equal(deep.level, 'solid');
  assert.equal(run([[{ outcome: 'applied' }, day(0)], [{ outcome: 'applied' }, day(0, 10)], [{ outcome: 'explained_back' }, day(0, 11)]]).level, 'solid',
    'Mastery waits for the student to know it again later.');
  const later = run([[{ outcome: 'explained_back' }, day(1)], [{ outcome: 'applied' }, day(2)]], deep);
  assert.equal(later.level, 'mastered');
  const slip = run([[{ outcome: 'wrong' }, day(3)]], later);
  assert.ok(slip.score! < later.score!);
  assert.notEqual(slip.level, 'mastered');
  assert.equal(run([[{ outcome: 'wrong' }, day(0)]]).level, 'struggling');
  assert.equal(run([[{ outcome: 'partial' }, day(0)]]).level, 'shaky');
});

test('a misconception caps the level until it is resolved, and gaps collect without duplicates', () => {
  const strong = run([[{ outcome: 'applied' }, day(0)], [{ outcome: 'applied' }, day(0)], [{ outcome: 'explained_back' }, day(0)]]);
  const wrongBelief = applyObservation(strong, { outcome: 'partial', misconception: 'The chain rule adds the derivatives.' }, day(1));
  assert.equal(wrongBelief.level, 'shaky');
  const again = applyObservation(wrongBelief, { outcome: 'explained_back', misconception: 'the chain rule ADDS the derivatives', gap: 'Why the inner derivative appears' }, day(1));
  assert.deepEqual(again.misconceptions, ['the chain rule ADDS the derivatives']);
  assert.equal(again.level, 'shaky');
  const fixed = applyObservation(again, { outcome: 'applied', resolved: ['The chain rule adds the derivatives', 'why the inner derivative appears'] }, day(2));
  assert.deepEqual([fixed.misconceptions, fixed.gaps], [[], []]);
  assert.ok(fixed.level === 'solid' || fixed.level === 'mastered');
});

test('review comes due by level', () => {
  const now = new Date(day(3));
  assert.equal(reviewDue({ level: 'shaky', last_checked_at: day(1) }, now), true);
  assert.equal(reviewDue({ level: 'solid', last_checked_at: day(1) }, now), false);
  assert.equal(reviewDue({ level: 'struggling', last_checked_at: day(3) }, now), true);
  assert.equal(reviewDue({ level: 'introduced', last_checked_at: null }, now), false);
});

test('ChatGPT builds the concept map and the student model tells what to explain, focus on and complete', {
  skip: !process.env.BOLO_TEST_SURREAL,
}, async (t) => {
  const namespace = `bolo_understanding_${crypto.randomUUID().replaceAll('-', '')}`;
  const vector = Array.from({ length: 1024 }, (_, i) => i === 0 ? 1 : 0);
  const origin = 'https://bolo.example';
  const links = { app: origin };
  const env = {
    SPKY_DB_URL: 'http://127.0.0.1:8666', SPKY_DB_NS: namespace, SPKY_DB_NAME: 'main', SPKY_DB_USER: 'root', SPKY_DB_PASS: 'root',
    API_ORIGIN: origin, APP_ORIGIN: origin, EMBEDDING_MODEL: 'test', AI: { run: async () => ({ data: [vector] }) },
  } as unknown as Env;
  const db = Surreal.system(env);
  try {
    await db.query(`DEFINE NAMESPACE ${namespace}; DEFINE DATABASE main;`);
    await db.queryAll(await readFile(new URL('../../../schema/src/schema.surql', import.meta.url), 'utf8'));
    await ensureServerSchema(env);
    await db.query(`
      CREATE user:u1 CONTENT {email:'student@example.test',name:'Student',password:'test'};
      CREATE user:u2 CONTENT {email:'other@example.test',name:'Other',password:'test'};
      CREATE course:c1 CONTENT {owner:user:u1,title:'Math'};
      CREATE course:c2 CONTENT {owner:user:u2,title:'Other course'};
      CREATE material:m1 CONTENT {owner:user:u1,course:course:c1,title:'Lecture 1',kind:'markdown',content:'x',status:'ready',index_rev:2,chunk_count:4,learned_through:1};
      FOR $p IN [0, 1, 2, 3] {
        CREATE type::record('chunk', 'a' + <string>$p) CONTENT {
          owner:user:u1,course:course:c1,material:material:m1,rev:2,position:$p,heading:'H',text:'Section ' + <string>$p,embedding:$vector
        };
      };`, { vector });

    await t.test('writes are owner-checked and validated', async () => {
      await assert.rejects(recordUnderstanding(env, 'user:u1', { courseId: 'course:c2', concepts: [{ name: 'X' }] }), /Course not found/);
      await assert.rejects(recordUnderstanding(env, 'user:u1', { courseId: 'course:c1' }), /Pass concepts/);
      await assert.rejects(recordUnderstanding(env, 'user:u1', { courseId: 'course:c1', concepts: [{ name: 'X', material_id: 'material:m1', position: 9 }] }), /from 0 to 3/);
      await assert.rejects(recordUnderstanding(env, 'user:u1', { courseId: 'course:c1', observations: [{ concept: 'X', outcome: 'recalled', note: ' ' }] }), /note/);
    });

    await t.test('mapping concepts and their foundations', async () => {
      const mapped = await recordUnderstanding(env, 'user:u1', {
        courseId: 'course:c1',
        concepts: [
          { name: 'Derivative', summary: 'Rate of change.', material_id: 'material:m1', position: 0, requires: ['Limits'] },
          { name: 'Limits', prior: true },
          { name: 'Power rule', material_id: 'material:m1', position: 1, requires: ['Derivative'], part_of: 'Differentiation rules' },
          { name: 'Chain rule', material_id: 'material:m1', position: 2, requires: ['derivative', 'Function composition'] },
          { name: 'Function composition', prior: true },
          // A direct cycle is dropped.
          { name: 'Derivative', requires: ['Chain rule'] },
        ],
      });
      assert.equal(mapped.concepts.filter((c) => c.new).length, 6);
      assert.equal(mapped.links_added, 4);
      const again = await recordUnderstanding(env, 'user:u1', { courseId: 'course:c1', concepts: [{ name: 'DERIVATIVE', requires: ['Limits'] }] });
      assert.equal(again.concepts.length, 2, 'Names match existing concepts; nothing is duplicated.');
      assert.ok(!again.concepts.some((c) => c.new));
      assert.equal(await db.query<number>('RETURN count(SELECT id FROM requires)'), 4);
    });

    await t.test('observations move levels and record evidence', async () => {
      const out = await recordUnderstanding(env, 'user:u1', {
        courseId: 'course:c1',
        observations: [
          { concept: 'Derivative', outcome: 'explained', note: 'Explained slope as rate of change.' },
          { concept: 'Derivative', outcome: 'explained_back', note: 'Said it is how fast f changes at a point.' },
          { concept: 'Limits', outcome: 'wrong', note: 'Thought a limit is the value at the point.', misconception: 'A limit is just f(a).' },
          { concept: 'Power rule', outcome: 'partial', note: 'Got 2x for x squared, unsure for x cubed.', gap: 'Applying it to other exponents' },
        ],
      });
      const level = (n: string) => out.concepts.find((c) => c.name === n)?.level;
      assert.equal(out.observations_recorded, 4);
      assert.equal(level('Derivative'), 'shaky');
      assert.equal(level('Limits'), 'struggling');
      assert.equal(level('Power rule'), 'shaky');
      assert.equal(await db.query<number>('RETURN count(SELECT id FROM concept_evidence WHERE owner = user:u1)'), 4);
    });

    await t.test('concurrent writes to one concept all count, also when it is new', async () => {
      const before = await db.query<number>('RETURN (SELECT VALUE checks FROM concept WHERE name = "Power rule")[0]');
      await Promise.all([0, 1, 2].map((i) => recordUnderstanding(env, 'user:u1', {
        courseId: 'course:c1',
        observations: [
          { concept: 'Power rule', outcome: 'recalled', note: `Answer ${i}.` },
          { concept: 'Sum rule', outcome: 'explained', note: `Told ${i}.` },
        ],
      })));
      assert.equal(await db.query<number>('RETURN (SELECT VALUE checks FROM concept WHERE name = "Power rule")[0]'), before + 3);
      assert.equal(await db.query<number>('RETURN count(SELECT id FROM concept WHERE name = "Sum rule")'), 1);
      assert.equal(await db.query<number>('RETURN count(SELECT id FROM concept_evidence WHERE concept.name = "Sum rule")'), 3);
    });

    await t.test('the model: foundations before the next section, focus, and what is missing', async () => {
      // Learned through section 1: section 2 (Chain rule) comes next.
      const model = await studentModel(env, 'user:u1', { courseId: 'course:c1', materialId: 'material:m1', full: true });
      assert.deepEqual(model.up_next, ['Chain rule']);
      const first = model.explain_first.map((e) => e.concept);
      assert.equal(first[0], 'Limits', 'The deepest weak foundation comes first.');
      assert.ok(first.includes('Function composition') && first.includes('Derivative'));
      assert.equal(model.explain_first.find((e) => e.concept === 'Limits')?.background, true);
      assert.equal(model.focus[0].concept, 'Limits');
      assert.match(model.focus[0].why, /misconception/);
      assert.deepEqual(model.missing.gaps.map((g) => g.gap), ['Applying it to other exponents']);
      assert.equal(model.missing.unmapped_sections, undefined, 'Both reached sections have concepts.');
      assert.ok(model.concepts!.find((c) => c.name === 'Chain rule')?.requires?.includes('Derivative'));
      assert.equal(model.concepts!.find((c) => c.name === 'Power rule')?.part_of, 'Differentiation rules');

      const topic = await studentModel(env, 'user:u1', { courseId: 'course:c1', topics: ['power rule', 'Taylor series'] });
      assert.deepEqual(topic.explain_first.map((e) => e.concept), ['Limits', 'Derivative']);
      assert.ok(topic.notes?.some((n) => n.includes('Taylor series')));
      await assert.rejects(studentModel(env, 'user:u2', { courseId: 'course:c1' }), /Course not found/);
    });

    await t.test('study sessions carry the model and claim their evidence', async () => {
      const prepared = await prepareStudySession(env, 'user:u1', links, { courseId: 'course:c1', mode: 'explain' }) as {
        guide: string; student_model: { explain_first: { concept: string }[] };
      };
      assert.match(prepared.guide, /record_understanding/);
      assert.ok(prepared.student_model.explain_first.some((e) => e.concept === 'Limits'));
      const saved = await saveStudySession(env, 'user:u1', links, {
        courseId: 'course:c1', materialId: 'material:m1', mode: 'explain', recap: '## What we covered\nDerivatives and limits.', assistant: 'ChatGPT', minutes: 15,
      });
      assert.equal(await db.query<number>('RETURN count(SELECT id FROM concept_evidence WHERE call = type::record($id))', { id: saved.id }), 10);
      const session = await fetchDocument(env, 'user:u1', links, saved.id, (md) => md);
      assert.match(session.text, /Understanding recorded:[\s\S]*Limits: wrong/);
      const conceptId = (await studentModel(env, 'user:u1', { courseId: 'course:c1', full: true })).concepts!.find((c) => c.name === 'Limits')!.id;
      const concept = await fetchDocument(env, 'user:u1', links, conceptId, (md) => md);
      assert.match(concept.title, /Limits \(struggling\)/);
      assert.match(concept.text, /A limit is just f\(a\)/);
      await assert.rejects(fetchDocument(env, 'user:u2', links, conceptId, (md) => md), /Concept not found/);
    });

    await t.test('resolving a misconception, then deleting the lecture keeps understanding but drops where it is taught', async () => {
      const out = await recordUnderstanding(env, 'user:u1', {
        courseId: 'course:c1',
        observations: [{ concept: 'Limits', outcome: 'explained_back', note: 'Limit is where f approaches.', resolved: ['A limit is just f(a).'] }],
      });
      assert.equal(out.concepts[0].misconceptions, undefined);
      await db.query('DELETE material:m1');
      const left = await db.query<{ name: string; material?: string; level: string }[]>('SELECT name, material, level FROM concept WHERE name = "Derivative"');
      assert.equal(left[0].material ?? null, null);
      assert.equal(left[0].level, 'shaky');
      await db.query('DELETE course:c1');
      assert.equal(await db.query<number>('RETURN count(SELECT id FROM concept) + count(SELECT id FROM concept_evidence) + count(SELECT id FROM requires)'), 0);
    });
  } finally {
    await db.query(`REMOVE NAMESPACE IF EXISTS ${namespace};`).catch(() => {});
  }
});
