import assert from 'node:assert/strict';
import { test, type TestContext } from 'node:test';
import type { Env } from '../src/env.ts';
import {
  courseMemoryScope,
  getMemoryDocument,
  indexMemorySection,
  lectureStamp,
  memoryConfigured,
  queryCourseMemory,
  recallStudentMemory,
  sectionGraph,
  type MemoryMaterial,
} from '../src/memory.ts';

const env = {
  AGENT_MEMORY_URL: 'https://memory.example/api/v1/talktobolo/',
  AGENT_MEMORY_API_KEY: 'test-secret',
} as Env;
const material: MemoryMaterial = {
  id: 'material:m1', owner: 'user:u1', course: 'course:c1', title: 'Physics', index_rev: 3,
};
const courseScope = courseMemoryScope(material.owner, material.course, material.id, material.index_rev);
const chunk = { heading: 'Forces', text: 'Force equals mass times acceleration.' };

interface CapturedRequest { url: string; init: RequestInit; headers: Headers }
type Reply = { body: unknown; status?: number };

function mockFetch(t: TestContext, replies: Reply[]) {
  const requests: CapturedRequest[] = [];
  t.mock.method(globalThis, 'fetch', async (url: string | URL | Request, init: RequestInit = {}) => {
    const headers = new Headers(init.headers);
    requests.push({ url: String(url), init, headers });
    assert.equal(headers.get('Authorization'), 'Bearer test-secret');
    assert.equal(init.redirect, 'manual');
    assert.ok(init.signal instanceof AbortSignal, 'Every request has an abort timeout.');
    const reply = replies.shift();
    assert.ok(reply, `Unexpected HTTP request: ${String(url)}`);
    return new Response(JSON.stringify(reply.body), { status: reply.status ?? 200 });
  });
  return requests;
}

const body = (request: CapturedRequest) => JSON.parse(request.init.body as string);
const scopeReply = (path = courseScope): Reply => ({ body: { path: `${path}/`, createdAt: '2026-10-20T00:00:00Z' } });
const fact = (id: string, source = 'attribute', occurredAt: string | null = lectureStamp(0)) => ({
  id, text: `Evidence ${id}`, source, score: 0.9, occurredAt,
});

test('unconfigured memory and unreached boundaries perform no requests', async (t) => {
  const requests = mockFetch(t, []);
  assert.equal(memoryConfigured({} as Env), false);
  assert.equal(memoryConfigured(env), true);
  assert.equal(await indexMemorySection({} as Env, material, 0, chunk), null);
  assert.equal(await getMemoryDocument({} as Env, 'document-1'), null);
  assert.deepEqual(await queryCourseMemory(env, material, -1, 'force'), []);
  assert.deepEqual(await queryCourseMemory({} as Env, material, 2, 'force'), []);
  assert.deepEqual(await recallStudentMemory(env, material, 'force'), []);
  assert.equal(requests.length, 0);
});

test('scope ids remain distinct across owners, courses, materials, revisions and unsafe characters', () => {
  const variants = [
    courseScope,
    courseMemoryScope('user:u2', material.course, material.id, 3),
    courseMemoryScope(material.owner, 'course:c2', material.id, 3),
    courseMemoryScope(material.owner, material.course, 'material:m2', 3),
    courseMemoryScope(material.owner, material.course, material.id, 4),
    courseMemoryScope(material.owner, material.course, 'material/m1', 3),
    courseMemoryScope(material.owner, material.course, 'material_m1', 3),
  ];
  assert.equal(new Set(variants).size, variants.length);
  assert.equal(lectureStamp(-1), '2000-01-01T00:00:00.000Z');
  assert.equal(lectureStamp(0), '2000-01-01T00:00:01.000Z');
  assert.equal(lectureStamp(1), '2000-01-01T00:00:02.000Z');
  assert.throws(() => lectureStamp(NaN));
  assert.throws(() => lectureStamp(-2));
});

test('document upload uses scoped multipart metadata and unique bytes per position', async (t) => {
  const requests = mockFetch(t, [
    scopeReply(), { status: 202, body: { id: 'doc-0', status: 'queued', observedAt: lectureStamp(0).replace('.000Z', 'Z') } },
    scopeReply(), { status: 202, body: { id: 'doc-1', status: 'queued' } },
  ]);
  assert.equal(await indexMemorySection(env, material, 0, chunk), 'doc-0');
  // observedAt is optional on the response; the request supplies it explicitly.
  assert.equal(await indexMemorySection(env, material, 1, chunk), 'doc-1');
  assert.deepEqual(body(requests[0]), { path: courseScope });
  assert.deepEqual(body(requests[2]), { path: courseScope }, 'Named scope registration repeats deterministically.');
  const text: string[] = [];
  for (const [index, position] of [[1, 0], [3, 1]]) {
    const request = requests[index];
    assert.equal(request.url, 'https://memory.example/api/v1/talktobolo/documents');
    assert.equal(request.headers.has('Content-Type'), false, 'fetch must set the multipart boundary.');
    const form = request.init.body as FormData;
    const metadata = JSON.parse(form.get('metadata') as string);
    assert.equal(metadata.observedAt, lectureStamp(position));
    assert.deepEqual(metadata.scopes, [[courseScope]]);
    assert.deepEqual(metadata.labels, [`section=${position}`]);
    assert.equal(metadata.mimeType, 'text/markdown');
    text.push(await (form.get('file') as File).text());
  }
  assert.notEqual(text[0], text[1], 'Content deduplication must not combine separate timeline positions.');
  assert.match(text[0], /material:m1/);
  assert.match(text[0], /"revision":3/);
  assert.match(text[0], /"position":0/);
});

test('upload refuses an explicitly mismatched known-time response', async (t) => {
  mockFetch(t, [scopeReply(), { status: 202, body: { id: 'doc', observedAt: lectureStamp(9) } }]);
  await assert.rejects(indexMemorySection(env, material, 1, chunk), /different lecture timestamp/);
});

test('course retrieval excludes future, undated, literal, entity-card and expanded context', async (t) => {
  const accepted = fact('known');
  const action = fact('action', 'action', lectureStamp(1));
  const requests = mockFetch(t, [{ body: {
    hits: [
      accepted, action, fact('future', 'attribute', lectureStamp(2)), fact('undated', 'attribute', null),
      fact('entity-card', 'entity'), fact('passage', 'chunk'), fact('conversation', 'memory_chunk'),
      fact('invalid-time', 'attribute', 'invalid'), fact('before-course', 'attribute', lectureStamp(-1)),
    ],
    contextHits: [fact('expanded-future', 'attribute', lectureStamp(4)), fact('expanded-past')],
  } }]);
  assert.deepEqual(await queryCourseMemory(env, material, 1, 'force'), [accepted, action]);
  assert.deepEqual(body(requests[0]), {
    query: 'force', k: 10, lens: [[courseScope]], scopeView: 'strict',
    asOf: lectureStamp(1), include: ['facts'], mode: 'graph', source: 'talktobolo',
  });
});

test('student recall uses real known-time and exact reached-section scopes when rewound', async (t) => {
  const requests = mockFetch(t, [{ body: { hits: [fact('future', 'attribute', '2026-10-21T00:00:00Z')] } }]);
  assert.deepEqual(await recallStudentMemory(env, material, 'Student understanding?', '2026-10-20T12:00:00Z', 1), []);
  const request = body(requests[0]);
  assert.equal(request.asOf, '2026-10-20T12:00:00Z');
  assert.deepEqual(request.lens, [0, 1].map((position) => [courseScope.replace(/\/course$/, `/student/section/p${position}`)]));
  assert.ok(request.lens.every(([path]: string[]) => !path.endsWith('/student') && !path.includes('*')));
});

test('document polling and errors do not silently accept missing scopes or failed requests', async (t) => {
  const requests = mockFetch(t, [
    { body: { status: 'ready' } }, { body: { status: 'failed', error: 'Extraction failed.' } },
    { status: 503, body: { message: 'busy' } }, { status: 409, body: { message: 'scope conflict' } },
  ]);
  assert.deepEqual(await getMemoryDocument(env, 'document:1'), { status: 'ready' });
  assert.ok(requests[0].url.endsWith('/documents/document%3A1'));
  assert.deepEqual(await getMemoryDocument(env, 'doc-2'), { status: 'failed', error: 'Extraction failed.' });
  await assert.rejects(getMemoryDocument(env, 'doc-3'), /failed \(503\)/);
  await assert.rejects(indexMemorySection(env, material, 0, chunk), /failed \(409\)/);
});

test('section graph keeps only concepts the section names, with their links', async (t) => {
  const entity = (type: string, name: string, text = name) => ({ id: `e-${name}`, text, source: 'entity', score: 0.5, resource: { kind: 'entity', entityType: type, name } });
  const relation = (from: string, label: string, to: string) => ({
    id: `r-${from}`, text: `${from} ${label} ${to}`, source: 'entity', score: 0.5,
    resource: { kind: 'relation', subjectType: 'concept', subjectName: from, label, objectType: 'concept', objectName: to },
  });
  const requests = mockFetch(t, [{
    body: {
      tier: 'hybrid',
      hits: [
        entity('concept', 'force', 'Force'),
        entity('concept', 'acceleration'),
        entity('concept', 'mass'),
        entity('person', 'newton'),
        entity('concept', 'photosynthesis'),
        entity('concept', "newton's_second_law", "Newton's second law"),
        { id: 'a1', text: "acceleration's definition is the rate of change of velocity", source: 'attribute', score: 0.5,
          resource: { kind: 'attribute', entityType: 'concept', name: 'acceleration', key: 'definition' } },
        relation('force', 'requires', 'acceleration'),
        relation('force', 'depends_on', 'mass'),
        relation('force', 'requires', 'photosynthesis'),
        { id: 'chunk:x', text: 'Force equals', source: 'chunk', score: 0.4, resource: { kind: 'document', documentId: 'd', position: 0 } },
      ],
      contextHits: [relation('acceleration', 'part_of', 'force')],
    },
  }]);
  const g = await sectionGraph(env, material, 0, { heading: 'Forces', text: "Newton's second law: force equals mass times acceleration, said Newton." });
  assert.deepEqual(g.concepts, ['Force', 'acceleration', 'mass', "Newton's second law"]);
  assert.deepEqual(g.summaries, { acceleration: 'the rate of change of velocity' });
  assert.deepEqual(g.requires, [['Force', 'acceleration'], ['Force', 'mass']]);
  assert.deepEqual(g.partOf, [['acceleration', 'Force']]);
  const sent = body(requests[0]);
  assert.deepEqual(sent.lens, [[courseScope]]);
  assert.equal(sent.mode, 'graph');
  assert.deepEqual(await sectionGraph({} as Env, material, 0, chunk), { concepts: [], requires: [], partOf: [] });
});

test('a URL without a context id leaves Agent Memory off', () => {
  assert.equal(memoryConfigured({ AGENT_MEMORY_URL: 'https://memory.example', AGENT_MEMORY_API_KEY: 'k' } as Env), false);
});
