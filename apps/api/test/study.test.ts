import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import type { Env } from '../src/env.ts';
import { loadAnswerImage } from '../src/answers.ts';
import { sha256Hex } from '../src/auth.ts';
import { handleMcp } from '../src/mcp.ts';
import { ensureServerSchema } from '../src/service.ts';
import {
  fetchDocument, lectureOutline, prepareStudySession, readSection, recordProgress, reviewNext, saveStudySession, searchEverything,
  sectionId, studyOverview,
} from '../src/study.ts';
import { Surreal } from '../src/surreal.ts';

const origin = 'https://bolo.example';
const links = { app: origin };
const same = (md: string) => md;

test('reviewNext takes the last recap section in any language', () => {
  assert.equal(reviewNext('## Was wir gemacht haben\nA\n\n## Wie es lief\nB\n\n## Als Nächstes\nKettenregel üben'), 'Kettenregel üben');
  assert.equal(reviewNext('No headings at all'), null);
  assert.equal(reviewNext(null), null);
});

test('studying over MCP keeps lecture boundaries, writes progress forward and logs sessions', {
  skip: !process.env.BOLO_TEST_SURREAL,
}, async (t) => {
  const namespace = `bolo_study_${crypto.randomUUID().replaceAll('-', '')}`;
  const vector = Array.from({ length: 1024 }, (_, i) => i === 0 ? 1 : 0);
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
      CREATE user:u1 CONTENT {email:'student@example.test',name:'Student Name',password:'test'};
      CREATE user:u2 CONTENT {email:'other@example.test',name:'Other',password:'test'};
      CREATE course:c1 CONTENT {owner:user:u1,title:'Math',language:'de',tutor_notes:'Exam is oral.'};
      CREATE course:c3 CONTENT {owner:user:u1,title:'Physics'};
      CREATE course:c2 CONTENT {owner:user:u2,title:'Other course'};
      CREATE material:m1 CONTENT {owner:user:u1,course:course:c1,title:'Lecture 1',kind:'pdf',content:'Source',status:'ready',index_rev:2,chunk_count:4,page_count:3};
      CREATE material:m3 CONTENT {owner:user:u1,course:course:c3,title:'Mechanics',kind:'markdown',content:'Forces',status:'ready',index_rev:1,chunk_count:1};
      CREATE material:mp CONTENT {owner:user:u1,course:course:c1,title:'Processing',kind:'markdown',content:'',status:'processing',index_rev:1};
      CREATE material:m2 CONTENT {owner:user:u2,course:course:c2,title:'Other lecture',kind:'markdown',status:'ready',index_rev:1,chunk_count:1,learned_through:0};
      FOR $c IN $chunks {
        CREATE type::record($c.id) CONTENT {
          owner: type::record($c.owner),course:type::record($c.course),material:type::record($c.material),
          rev:$c.rev,position:$c.position,heading:$c.heading,text:$c.text,page:$c.page,page_end:$c.page,embedding:$vector
        };
      };`, { vector, chunks: [
        { id: 'chunk:a0', owner: 'user:u1', course: 'course:c1', material: 'material:m1', rev: 2, position: 0, heading: 'Basics', page: 1, text: 'FIRST_MARKER derivatives describe the rate of change.' },
        { id: 'chunk:a1', owner: 'user:u1', course: 'course:c1', material: 'material:m1', rev: 2, position: 1, heading: 'Basics', page: 1, text: 'SECOND_MARKER the derivative of x squared is two x.' },
        { id: 'chunk:a2', owner: 'user:u1', course: 'course:c1', material: 'material:m1', rev: 2, position: 2, heading: 'Chain rule', page: 2, text: 'THIRD_MARKER the chain rule composes derivatives.' },
        { id: 'chunk:a3', owner: 'user:u1', course: 'course:c1', material: 'material:m1', rev: 2, position: 3, heading: 'Gradients', page: 3, text: 'FOURTH_MARKER gradients collect partial derivatives.' },
        { id: 'chunk:old', owner: 'user:u1', course: 'course:c1', material: 'material:m1', rev: 1, position: 0, heading: 'Old', page: 1, text: 'OBSOLETE_MARKER derivatives.' },
        { id: 'chunk:p0', owner: 'user:u1', course: 'course:c3', material: 'material:m3', rev: 1, position: 0, heading: 'Forces', page: null, text: 'PHYSICS_MARKER forces and derivatives of momentum.' },
        { id: 'chunk:f0', owner: 'user:u2', course: 'course:c2', material: 'material:m2', rev: 1, position: 0, heading: 'Foreign', page: null, text: 'FOREIGN_MARKER derivatives.' },
      ] });

    await t.test('the overview lists every course, then one course with each lecture and a place to start', async () => {
      const all = await studyOverview(env, 'user:u1', links) as { courses: { id: string; lectures: number; sections_total: number }[] };
      assert.deepEqual(all.courses.map((c) => c.id), ['course:c1', 'course:c3']);
      assert.equal(all.courses[0].lectures, 1, 'Material still processing is not counted.');
      assert.equal(all.courses[0].sections_total, 4);
      const one = await studyOverview(env, 'user:u1', links, 'course:c1') as {
        course: { language: string; tutor_notes: string };
        lectures: { id: string; status: string; sections_reached: number; next_section: { position: number; heading: string } | null; url: string }[];
        suggestions: string[];
      };
      assert.equal(one.course.language, 'German');
      assert.equal(one.course.tutor_notes, 'Exam is oral.');
      assert.equal(one.lectures.length, 2);
      assert.deepEqual(one.lectures[0].next_section, { position: 0, heading: 'Basics', page: 1 });
      assert.equal(one.lectures[0].url, `${origin}/courses/c1/materials/m1`);
      assert.equal(one.lectures[1].status, 'processing');
      assert.match(one.suggestions[0], /start "Lecture 1" at section 1 of 4/);
      await assert.rejects(studyOverview(env, 'user:u1', links, 'course:c2'), /Course not found/);
    });

    await t.test('explain teaches only the next section; quiz waits until something is reached', async () => {
      const explain = await prepareStudySession(env, 'user:u1', links, { courseId: 'course:c1', mode: 'explain' });
      const text = JSON.stringify(explain);
      assert.ok(text.includes('FIRST_MARKER'));
      for (const secret of ['SECOND_MARKER', 'THIRD_MARKER', 'FOURTH_MARKER', 'OBSOLETE_MARKER', 'FOREIGN_MARKER', 'PHYSICS_MARKER']) {
        assert.ok(!text.includes(secret), secret);
      }
      assert.ok(explain.guide.includes('Speak German'));
      assert.equal((explain as { next_position: number }).next_position, 1);
      const quiz = await prepareStudySession(env, 'user:u1', links, { courseId: 'course:c1', mode: 'quiz' });
      assert.deepEqual((quiz as { reached_sections: unknown[] }).reached_sections, []);
      assert.match(String((quiz as { note: string }).note), /not reached any section/);
      await assert.rejects(prepareStudySession(env, 'user:u1', links, { courseId: 'course:c1', materialId: 'material:mp', mode: 'quiz' }), /not ready/);
      await assert.rejects(prepareStudySession(env, 'user:u1', links, { courseId: 'course:c1', materialId: 'material:m3', mode: 'quiz' }), /another course/);
    });

    await t.test('sections read one at a time, and the outline groups headings with pages', async () => {
      const first = await readSection(env, 'user:u1', links, 'material:m1');
      assert.equal(first.section.position, 0);
      assert.equal(first.section.id, 'section:m1:2:0');
      assert.equal(first.section.reached, false);
      assert.equal(first.section.url, `${origin}/courses/c1/materials/m1?page=1`);
      assert.equal(first.next_position, 1);
      assert.ok((await readSection(env, 'user:u1', links, 'material:m1', 3)).section.text.includes('FOURTH_MARKER'));
      await assert.rejects(readSection(env, 'user:u1', links, 'material:m1', 4), /from 0 to 3/);
      await assert.rejects(readSection(env, 'user:u1', links, 'material:m2'), /Material not found/);
      const outline = await lectureOutline(env, 'user:u1', links, 'material:m1');
      assert.deepEqual(outline.outline.map((o) => [o.positions, o.heading]), [['0-1', 'Basics'], ['2', 'Chain rule'], ['3', 'Gradients']]);
    });

    await t.test('progress only moves forward, on the current revision and the owner\'s own lecture', async () => {
      await assert.rejects(recordProgress(env, 'user:u1', { materialId: 'material:m1', through: 1, rev: 1 }), /changed/);
      await assert.rejects(recordProgress(env, 'user:u1', { materialId: 'material:m1', through: 4, rev: 2 }), /from 0 to 3/);
      await assert.rejects(recordProgress(env, 'user:u1', { materialId: 'material:m2', through: 0, rev: 1 }), /Material not found/);
      assert.equal((await recordProgress(env, 'user:u1', { materialId: 'material:m1', through: 1, rev: 2 })).changed, true);
      const back = await recordProgress(env, 'user:u1', { materialId: 'material:m1', through: 0, rev: 2 });
      assert.deepEqual([back.changed, back.sections_reached], [false, 2]);
      const quiz = JSON.stringify(await prepareStudySession(env, 'user:u1', links, { courseId: 'course:c1', mode: 'quiz' }));
      assert.ok(quiz.includes('FIRST_MARKER') && quiz.includes('SECOND_MARKER'));
      assert.ok(!quiz.includes('THIRD_MARKER') && !quiz.includes('FOURTH_MARKER'));
    });

    let sessionId = '';
    await t.test('a logged session joins the history with its recap and records how far it got', async () => {
      await assert.rejects(saveStudySession(env, 'user:u1', links, { courseId: 'course:c2', mode: 'quiz', recap: 'x'.repeat(40), assistant: 'ChatGPT' }), /Course not found/);
      await assert.rejects(saveStudySession(env, 'user:u1', links, { courseId: 'course:c1', mode: 'quiz', recap: 'short', assistant: 'ChatGPT' }), /short recap/);
      await assert.rejects(saveStudySession(env, 'user:u1', links, {
        courseId: 'course:c1', materialId: 'material:m1', mode: 'explain', recap: 'x'.repeat(40), assistant: 'ChatGPT', coveredThrough: 2, rev: 1,
      }), /changed/);
      const recap = `## What we covered\nThe [chain rule](${origin}/courses/c1/materials/m1?page=2).\n\n## How it went\nGood.\n\n## Review next\n2026-10-21T09:15:00+02:00 practice`;
      const saved = await saveStudySession(env, 'user:u1', links, {
        courseId: 'course:c1', materialId: 'material:m1', mode: 'explain', recap, assistant: 'ChatGPT', minutes: 20, coveredThrough: 2, rev: 2,
      });
      sessionId = saved.id;
      assert.equal(saved.progress?.sections_reached, 3);
      const row = await db.query<{ status: string; assistant: string; recap: string; started_at: string; ended_at: string }>(
        'SELECT status, assistant, recap, started_at, ended_at FROM ONLY type::record($id)', { id: saved.id });
      assert.equal(row.status, 'ended');
      assert.equal(row.assistant, 'ChatGPT');
      assert.ok(row.recap.includes('](/courses/c1/materials/m1?page=2)'), 'Links into the app are stored as paths, like call recaps.');
      assert.ok(row.recap.endsWith('2026-10-21T09:15:00+02:00 practice'));
      assert.ok(Date.parse(row.ended_at) - Date.parse(row.started_at) >= 19 * 60_000);
      const overview = await studyOverview(env, 'user:u1', links, 'course:c1') as { recent_sessions: { id: string; where: string; recap: string }[] };
      assert.equal(overview.recent_sessions[0].id, saved.id);
      assert.equal(overview.recent_sessions[0].where, 'ChatGPT (logged)');
      assert.ok(overview.recent_sessions[0].recap.includes(`](${origin}/courses/c1/materials/m1?page=2)`));
      const next = await prepareStudySession(env, 'user:u1', links, { courseId: 'course:c1', mode: 'explain' }) as { last_session?: { review_next: string } };
      assert.match(String(next.last_session?.review_next), /practice/);
    });

    await t.test('search spans the student\'s courses only, and fetch opens every id the tools hand out', async () => {
      const found = await searchEverything(env, 'user:u1', links, 'derivatives');
      const text = JSON.stringify(found);
      assert.ok(text.includes('FIRST_MARKER') && text.includes('PHYSICS_MARKER'));
      assert.ok(!text.includes('FOREIGN_MARKER') && !text.includes('OBSOLETE_MARKER'));
      assert.ok(found.results.every((r) => r.url.startsWith(`${origin}/courses/`) && r.id.startsWith('section:')));
      const section = await fetchDocument(env, 'user:u1', links, sectionId('material:m1', 2, 2), same);
      assert.ok(section.text.includes('THIRD_MARKER'));
      assert.equal(section.url, `${origin}/courses/c1/materials/m1?page=2`);
      assert.equal(section.metadata?.reached, 'true');
      await assert.rejects(fetchDocument(env, 'user:u1', links, 'section:m1:1:0', same), /re-indexed/);
      await assert.rejects(fetchDocument(env, 'user:u2', links, 'section:m1:2:0', same), /Material not found/);
      assert.equal((await fetchDocument(env, 'user:u1', links, 'material:m1', same)).text, 'Source');
      const session = await fetchDocument(env, 'user:u1', links, sessionId, same);
      assert.match(session.text, /only its recap is kept/);
      await assert.rejects(fetchDocument(env, 'user:u2', links, sessionId, same), /Session not found/);
      assert.equal((await fetchDocument(env, 'user:u1', links, 'course:c1', same)).title, 'Math');
      await assert.rejects(fetchDocument(env, 'user:u1', links, 'nonsense', same), /Unknown id/);
    });

    await t.test('the MCP server lists annotated tools and serves search and fetch to a key holder', async () => {
      const key = `sc_${'k'.repeat(40)}`;
      await db.query('CREATE api_key:k1 CONTENT { owner: user:u1, name: $name, prefix: $prefix, key_hash: $hash }', {
        name: 'ChatGPT on my phone', prefix: key.slice(0, 7), hash: await sha256Hex(key),
      });
      let id = 0;
      const rpc = async (method: string, params: Record<string, unknown> = {}) => {
        const res = await handleMcp(new Request(`${origin}/mcp`, {
          method: 'POST',
          headers: {
            authorization: `Bearer ${key}`, 'content-type': 'application/json', accept: 'application/json, text/event-stream',
            'mcp-protocol-version': '2025-06-18',
          },
          body: JSON.stringify({ jsonrpc: '2.0', id: ++id, method, params }),
        }), env);
        assert.equal(res.status, 200, await res.clone().text());
        const raw = await res.text();
        const json = raw.trim().startsWith('{') ? raw : raw.split('\n').find((l) => l.startsWith('data:'))!.slice(5);
        return JSON.parse(json) as { result: Record<string, unknown>; error?: unknown };
      };
      const init = await rpc('initialize', { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'test', version: '1' } });
      assert.match(String((init.result as { instructions?: string }).instructions), /get_study_overview/);
      const listed = (await rpc('tools/list')).result.tools as {
        name: string; annotations: Record<string, boolean>; _meta: Record<string, unknown>; outputSchema?: unknown;
      }[];
      const byName = new Map(listed.map((tool) => [tool.name, tool]));
      for (const name of ['search', 'fetch', 'get_study_overview', 'prepare_study_session', 'read_section', 'get_lecture_outline', 'get_profile', 'get_understanding']) {
        assert.equal(byName.get(name)?.annotations.readOnlyHint, true, name);
      }
      for (const tool of listed) {
        assert.equal(typeof tool.annotations.readOnlyHint, 'boolean', tool.name);
        assert.equal(typeof tool.annotations.destructiveHint, 'boolean', tool.name);
        assert.equal(typeof tool.annotations.openWorldHint, 'boolean', tool.name);
        assert.deepEqual(tool._meta.securitySchemes, [{ type: 'oauth2', scopes: ['courses'] }], tool.name);
      }
      assert.equal(byName.get('delete_material')?.annotations.destructiveHint, true);
      assert.equal(byName.get('save_study_session')?.annotations.readOnlyHint, false);
      assert.ok(byName.get('search')?.outputSchema && byName.get('fetch')?.outputSchema);

      const search = (await rpc('tools/call', { name: 'search', arguments: { query: 'derivatives' } })).result as {
        structuredContent: { results: { id: string }[] }; content: { text: string }[];
      };
      assert.ok(search.structuredContent.results.length > 0);
      assert.deepEqual(JSON.parse(search.content[0].text), search.structuredContent);
      const fetched = (await rpc('tools/call', { name: 'fetch', arguments: { id: search.structuredContent.results[0].id } })).result as {
        structuredContent: { text: string; url: string };
      };
      assert.ok(fetched.structuredContent.url.startsWith(`${origin}/courses/`));
      const failed = (await rpc('tools/call', { name: 'fetch', arguments: { id: 'material:m2' } })).result as { isError: boolean };
      assert.equal(failed.isError, true, "Another student's lecture is not found.");

      const saved = (await rpc('tools/call', {
        name: 'save_study_session',
        arguments: { course_id: 'course:c3', mode: 'qa', recap: '## What we covered\nForces.\n\n## How it went\nFine.\n\n## Review next\nMomentum.' },
      })).result as { content: { text: string }[] };
      const savedId = JSON.parse(saved.content[0].text).id as string;
      assert.equal(await db.query('RETURN (SELECT VALUE assistant FROM ONLY type::record($id))', { id: savedId }), 'ChatGPT on my phone');
      const profile = (await rpc('tools/call', { name: 'get_profile', arguments: {} })).result as { content: { text: string }[] };
      assert.equal(JSON.parse(profile.content[0].text).email, 'student@example.test');

      // An image in a LaTeX document: stored, written where it goes, read back as its address.
      await db.query(`DEFINE BUCKET IF NOT EXISTS materials BACKEND 'memory' PERMISSIONS NONE;
        CREATE document:foreign CONTENT {owner:user:u2,course:course:c2,title:'Theirs',format:'tex',content:'x'};`);
      const call = async (name: string, args: Record<string, unknown>) => {
        const result = (await rpc('tools/call', { name, arguments: args })).result as { content: { text: string }[]; isError?: boolean };
        assert.ok(!result.isError, result.content[0].text);
        return JSON.parse(result.content[0].text);
      };
      const png = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]).toString('base64');
      const created = await call('create_document', { course_id: 'course:c1', title: 'With a figure', format: 'tex', content: '\\documentclass{article}\n\\begin{document}\nText.\n\\end{document}\n' });
      assert.match(byName.get('add_document_image')?.description ?? '', /LaTeX/);
      const added = await call('add_document_image', { document_id: created.id, base64: png });
      const imageKey = added.image.slice(added.image.indexOf(':') + 1);
      assert.equal(added.write_as, `\\includegraphics[width=\\linewidth]{${origin}/answer-images/${imageKey}}`);
      await call('edit_document', { document_id: created.id, edits: [{ old_text: 'Text.', new_text: `Text.\n${added.write_as}` }] });
      const stored = await db.query<string>('RETURN (SELECT VALUE content FROM ONLY type::record($id))', { id: created.id });
      assert.ok(stored.includes(`\\includegraphics[width=\\linewidth]{answer-image:${imageKey}}`), stored);
      assert.ok((await call('get_document', { document_id: created.id })).content.includes(added.write_as));
      assert.equal((await loadAnswerImage(env, added.image))?.mime, 'image/png');
      const theirs = (await rpc('tools/call', { name: 'add_document_image', arguments: { document_id: 'document:foreign', base64: png } })).result as { isError: boolean };
      assert.equal(theirs.isError, true, "Another student's document takes no image.");
      const notImage = (await rpc('tools/call', { name: 'add_document_image', arguments: { document_id: created.id, base64: Buffer.from('not an image').toString('base64') } })).result as { isError: boolean };
      assert.equal(notImage.isError, true, 'Only image files are kept.');
    });
  } finally {
    await db.query(`REMOVE NAMESPACE ${namespace};`);
  }
});
