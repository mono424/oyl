import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { deleteDocument, listVersions, readDocument, readVersion, restoreVersion, writeDocument } from '../src/documents.ts';
import type { Env } from '../src/env.ts';
import { ensureServerSchema } from '../src/service.ts';
import { Surreal, signinRecord } from '../src/surreal.ts';

const plain = { external: (md: string) => md, internal: (md: string) => md };
const links = { material: () => '', document: () => '' };

// Text value::patch breaks on ("@@"), every width of character, and one run with nothing to tell its places apart.
const WORDS = ['the', 'gradient', 'über', 'Straße', '$x^2$', '\\cite{stark2019}', '[@a; @b]', '😀', '🧑‍🔬', '日本語', '%20', '+', '@@ -1,4 +1,4 @@', '\\@@foo', '\t', '\r\n', '\n\n', '# Heading', ' ', '"', "'", '\\'];

/** Deterministic random numbers, so that a failure repeats. */
function random(seed: number) {
  const r = (n: number) => {
    seed = (seed * 1664525 + 1013904223) >>> 0;
    return seed % n;
  };
  const para = (n: number) => Array.from({ length: n }, () => WORDS[r(WORDS.length)]).join(' ');
  /** The kinds of text and of change a document goes through, cut at whole code points. */
  const change = (text: string) => {
    const chars = [...text];
    const at = r(chars.length + 1);
    const len = r(Math.min(chars.length - at, [3, 40, 800][r(3)]) + 1);
    switch (r(9)) {
      case 0: return '';
      case 1: return para(5 + r(200));
      case 2: return 'x'.repeat(r(3000));
      case 3: { const p = para(30); return Array.from({ length: 2 + r(20) }, () => p).join('\n\n'); }
      case 4: { const moved = chars.splice(at, len); chars.splice(r(chars.length + 1), 0, ...moved); return chars.join(''); }
      case 5: chars.splice(at, len); return chars.join('');
      case 6: chars.splice(at, 0, para(1 + r(3))); return chars.join('');
      case 7: chars.splice(at, 0, chars[at] ?? 'x'); return chars.join(''); // doubles a character: the end of the prefix and of the suffix overlap
      default: chars.splice(at, len, para(1 + r(3))); return chars.join('');
    }
  };
  return { r, para, change };
}

test('a delete that loses a race for a row runs again, other errors do not', async () => {
  const conflict = new Error('SurrealDB: There was a problem with the key-value store: Transaction conflict: Transaction write conflict. This transaction can be retried');
  let calls = 0;
  const failing = (error: Error, times: number) => ({
    query: async () => {
      if (++calls <= times) throw error;
      return [{ id: 'document:d1' }];
    },
  }) as unknown as Surreal;
  assert.deepEqual(await deleteDocument(failing(conflict, 2), 'document:d1'), { deleted: 'document:d1' });
  assert.equal(calls, 3);
  calls = 0;
  await assert.rejects(deleteDocument(failing(conflict, 9), 'document:d1'), /can be retried/);
  assert.equal(calls, 4);
  calls = 0;
  await assert.rejects(deleteDocument(failing(new Error('SurrealDB: Not enough permissions'), 1), 'document:d1'), /permissions/);
  assert.equal(calls, 1);
});

test('document versions', { skip: !process.env.BOLO_TEST_SURREAL }, async (t) => {
  const namespace = `bolo_versions_${crypto.randomUUID().replaceAll('-', '')}`;
  const env = {
    SPKY_DB_URL: 'http://127.0.0.1:8666', SPKY_DB_NS: namespace, SPKY_DB_NAME: 'main', SPKY_DB_USER: 'root', SPKY_DB_PASS: 'root',
    API_ORIGIN: 'https://bolo.example', APP_ORIGIN: 'https://bolo.example', EMBEDDING_MODEL: 'test',
  } as unknown as Env;
  const system = Surreal.system(env);
  try {
    await system.query(`DEFINE NAMESPACE ${namespace}; DEFINE DATABASE main;`);
    await system.queryAll(await readFile(new URL('../../../schema/src/schema.surql', import.meta.url), 'utf8'));
    await ensureServerSchema(env);
    await system.query(`
      CREATE user:u1 CONTENT { email: 'student@example.test', name: 'Student', password: crypto::argon2::generate('pw') };
      CREATE user:u2 CONTENT { email: 'other@example.test', name: 'Other', password: crypto::argon2::generate('pw') };
      CREATE course:c1 CONTENT { owner: user:u1, title: 'Math' };
      CREATE api_key:k1 CONTENT { owner: user:u1, name: 'ChatGPT', prefix: 'x', key_hash: crypto::sha256('secret') };
      CREATE api_key:k2 CONTENT { owner: user:u2, name: 'ChatGPT', prefix: 'y', key_hash: crypto::sha256('other') };`);
    const session = async (access: string, params: Record<string, string>) => {
      const token = await signinRecord(env, access, params);
      assert.ok(token, `no ${access} session`);
      return Surreal.as(env, token);
    };
    const student = await session('account', { email: 'student@example.test', password: 'pw' });
    const assistant = await session('agent', { key: 'secret' });
    const other = await session('agent', { key: 'other' });

    let docs = 0;
    /** A document the student started, with what the app's save does to it. */
    const start = async (content = '') => {
      const id = `document:t${++docs}`;
      await student.query(`CREATE type::record($id) CONTENT { owner: $auth.id, course: course:c1, title: 'T', content: $content }`, { id, content });
      return { id, type: (text: string) => student.query(`UPDATE type::record($id) SET content = $text`, { id, text }) };
    };
    const versions = async (id: string) => (await listVersions(student, id)).versions.map((v) => ({ n: v.n, author: v.author, added: v.added, removed: v.removed, restored_from: v.restored_from ?? undefined }));
    const textOf = async (id: string, n: number) => (await readVersion(student, id, n)).content;
    /** Moves the document's latest version back in time, so the next save does not extend it. */
    const age = (id: string, by: string) => system.query(
      `UPDATE document_version SET created_at = created_at - ${by}, updated_at = updated_at - ${by} WHERE document = type::record($id)`,
      { id },
    );

    await t.test('saves in one sitting are one version, and it reads back as it was', async () => {
      const doc = await start('Hello.');
      await doc.type('Hello. A');
      await doc.type('Hello. A longer');
      await doc.type('Hello. A longer text.');
      assert.deepEqual(await versions(doc.id), [{ n: 1, author: 'student', added: 21, removed: 0, restored_from: undefined }]);
      await age(doc.id, '6m');
      await doc.type('Hello. A longer text. And more.');
      await doc.type('Hello there. A longer text. And more.');
      const list = await versions(doc.id);
      assert.equal(list.length, 2);
      assert.equal(list[0].added, 16);
      assert.equal(await textOf(doc.id, 1), 'Hello. A longer text.');
      assert.equal(await textOf(doc.id, 2), 'Hello there. A longer text. And more.');
      assert.equal((await readVersion(student, doc.id, 2)).previous, 'Hello. A longer text.');
      // Two places, two hunks: the changeset stays small, however far apart they are.
      const deltas = await system.query<{ hunks: unknown[] }[]>('SELECT hunks FROM document_delta WHERE document = type::record($id) AND n = 2', { id: doc.id });
      assert.equal(deltas[0].hunks.length, 2);
    });

    await t.test('a session has its limit, and a big change starts a version of its own', async () => {
      const doc = await start('Start.');
      await age(doc.id, '31m');
      await doc.type('Start. On.');
      assert.equal((await versions(doc.id)).length, 2);
      const big = 'x'.repeat(600);
      await doc.type(`Start. On.${big}`);
      assert.equal((await versions(doc.id)).length, 3);
      assert.equal(await textOf(doc.id, 2), 'Start. On.');
      await doc.type('Start.');
      assert.equal((await versions(doc.id)).length, 4);
      assert.equal(await textOf(doc.id, 3), `Start. On.${big}`);
    });

    await t.test('typing something and taking it back leaves no version', async () => {
      const doc = await start('Start.');
      await age(doc.id, '6m');
      await doc.type('Start. Oops');
      assert.equal((await versions(doc.id)).length, 2);
      await doc.type('Start.');
      assert.equal((await versions(doc.id)).length, 1);
    });

    await t.test('the writer is told apart, and each of the assistant\'s writes stands alone', async () => {
      const doc = await start('Mine.');
      await writeDocument(assistant, plain, doc.id, 'Mine. Yours.', 'replace');
      await writeDocument(assistant, plain, doc.id, 'Mine. Yours. Too.', 'replace');
      await doc.type('Mine. Yours. Too. Mine again.');
      assert.deepEqual((await versions(doc.id)).map((v) => [v.n, v.author]), [[4, 'student'], [3, 'assistant'], [2, 'assistant'], [1, 'student']]);
      assert.equal((await readDocument(assistant, links, plain, doc.id)).version, 4);
      assert.equal(await textOf(doc.id, 2), 'Mine. Yours.');
    });

    await t.test('going back is a new version, and can be undone', async () => {
      const doc = await start('First.');
      await age(doc.id, '6m');
      await doc.type('Second.');
      assert.deepEqual(await restoreVersion(student, system, 'user:u1', 'student', doc.id, 1), { document: doc.id, status: 'restored', restored_from: 1, version: 3 });
      assert.equal((await readDocument(student, links, plain, doc.id)).content, 'First.');
      assert.deepEqual((await versions(doc.id))[0], { n: 3, author: 'student', added: 5, removed: 6, restored_from: 1 });
      // The next save does not change what the restored version holds.
      await doc.type('First. Then.');
      assert.equal((await versions(doc.id)).length, 4);
      assert.equal(await textOf(doc.id, 3), 'First.');
      await restoreVersion(student, system, 'user:u1', 'student', doc.id, 2);
      assert.equal((await readDocument(student, links, plain, doc.id)).content, 'Second.');
      // Going back to what is already there changes nothing.
      assert.equal((await restoreVersion(student, system, 'user:u1', 'assistant', doc.id, 5)).status, 'unchanged');
      assert.equal((await versions(doc.id)).length, 5);
      // An AI client going back is said to be one.
      await restoreVersion(assistant, system, 'user:u1', 'assistant', doc.id, 1);
      assert.equal((await versions(doc.id))[0].author, 'assistant');
      // Back to nothing at all.
      const empty = await start('Text.');
      await age(empty.id, '6m');
      await empty.type('');
      await age(empty.id, '6m');
      await empty.type('Again.');
      assert.equal((await restoreVersion(student, system, 'user:u1', 'student', empty.id, 2)).status, 'restored');
      assert.equal((await readDocument(student, links, plain, empty.id)).content, '');
    });

    await t.test('a format change is a version, and going back brings the format back', async () => {
      const doc = await start('Markdown.');
      await student.query(`UPDATE type::record($id) SET format = 'tex', content = 'Tex.'`, { id: doc.id });
      const list = await listVersions(student, doc.id);
      assert.deepEqual(list.versions.map((v) => v.format), ['tex', 'markdown']);
      await restoreVersion(student, system, 'user:u1', 'student', doc.id, 1);
      const back = await readDocument(student, links, plain, doc.id);
      assert.equal(back.format, 'markdown');
      assert.equal(back.content, 'Markdown.');
    });

    await t.test('a save built on text that changed meanwhile is refused', async () => {
      const sha = (text: string) => createHash('sha256').update(text).digest('hex');
      const { id, type } = await start('first');
      const save = (text: string, base: string) => student.query(`UPDATE type::record($id) SET content = $text, base = $base`, { id, text, base });
      const text = async () => (await readDocument(student, links, plain, id)).content;
      await save('second', sha('first'));
      await save('third', sha('second'));
      // Saved twice, as a retry does: the text is already there.
      await save('third', sha('second'));
      await system.query(`UPDATE type::record($id) SET content = 'from elsewhere'`, { id });
      await assert.rejects(save('fourth', sha('third')), /document_changed_elsewhere/);
      assert.equal(await text(), 'from elsewhere');
      await save('fourth', sha('from elsewhere'));
      assert.equal(await text(), 'fourth');
      // Writers that name no base are not checked, a rename neither.
      await type('plain');
      assert.equal(await text(), 'plain');
      await student.query(`UPDATE type::record($id) SET title = 'Renamed'`, { id });
      await save('with umlauts äß 😀', sha('plain'));
      await save('again', sha('with umlauts äß 😀'));
      assert.equal(await text(), 'again');
    });

    await t.test('a rename does not make a version', async () => {
      const doc = await start('Text.');
      await student.query(`UPDATE type::record($id) SET title = 'New'`, { id: doc.id });
      assert.equal((await versions(doc.id)).length, 1);
    });

    await t.test('a document from before versions gets what it held as version 1', async () => {
      await system.query(`
        REMOVE EVENT document_versions ON TABLE document;
        CREATE document:old CONTENT { owner: user:u1, course: course:c1, title: 'Old', content: 'From before.' };`);
      await system.queryAll((await readFile(new URL('../../../schema/src/schema.surql', import.meta.url), 'utf8')).match(/DEFINE EVENT OVERWRITE document_versions[\s\S]*?\n {2}};\n/)![0]);
      await student.query(`UPDATE document:old SET content = 'From before. And now.'`);
      assert.deepEqual((await versions('document:old')).map((v) => [v.n, v.author]), [[2, 'student'], [1, 'system']]);
      assert.equal(await textOf('document:old', 1), 'From before.');
    });

    await t.test('a row without a format can still be saved', async () => {
      // A database of its own: a table with rows does not let a field's type change like this.
      const legacyEnv = { ...env, SPKY_DB_NAME: 'legacy' } as Env;
      const legacy = Surreal.system(legacyEnv);
      await legacy.query('DEFINE DATABASE IF NOT EXISTS legacy;');
      await legacy.queryAll(await readFile(new URL('../../../schema/src/schema.surql', import.meta.url), 'utf8'));
      await legacy.query(`
        DEFINE FIELD OVERWRITE format ON TABLE document TYPE option<string>;
        CREATE user:u1 CONTENT { email: 'student@example.test', name: 'Student', password: crypto::argon2::generate('pw') };
        CREATE course:c1 CONTENT { owner: user:u1, title: 'Math' };
        CREATE document:noformat CONTENT { owner: user:u1, course: course:c1, title: 'Old', content: 'Old text.' };`);
      const token = await signinRecord(legacyEnv, 'account', { email: 'student@example.test', password: 'pw' });
      await Surreal.as(legacyEnv, token!).query(`UPDATE document:noformat SET content = 'Old text. Saved.'`);
      const stored = await legacy.queryAll('RETURN (SELECT VALUE content FROM ONLY document:noformat); SELECT n, format FROM document_version ORDER BY n;');
      assert.equal(stored[0], 'Old text. Saved.');
      assert.deepEqual(stored[1], [{ n: 1, format: 'markdown' }, { n: 2, format: 'markdown' }]);
    });

    await t.test('versions are the owner\'s only, and no client can write them', async () => {
      const doc = await start('Private.');
      await assert.rejects(listVersions(other, doc.id), /not found/i);
      await assert.rejects(readVersion(other, doc.id, 1), /no version/);
      await assert.rejects(restoreVersion(other, system, 'user:u2', 'assistant', doc.id, 1), /no version/);
      // Refused or silently nothing, depending on the statement: what counts is what is left.
      const attempt = (sql: string) => student.query(sql, { id: doc.id }).catch(() => {});
      await attempt(`UPDATE document_version SET author = 'system' WHERE document = type::record($id)`);
      await attempt(`DELETE document_delta WHERE document = type::record($id)`);
      await attempt(`DELETE document_version WHERE document = type::record($id)`);
      await attempt(`CREATE document_delta:forged CONTENT { owner: $auth.id, document: type::record($id), n: 2, hunks: [], sha: '' }`);
      await attempt(`CREATE document_version:forged CONTENT { owner: $auth.id, document: type::record($id), n: 2, author: 'student', format: 'markdown', added: 0, removed: 0, created_at: time::now(), updated_at: time::now() }`);
      assert.deepEqual(await system.query('SELECT VALUE id FROM [document_delta:forged, document_version:forged]'), []);
      assert.equal((await versions(doc.id))[0].author, 'student');
      assert.equal(await textOf(doc.id, 1), 'Private.');
    });

    await t.test('deleting a document takes its versions with it', async () => {
      const doc = await start('Gone.');
      await doc.type('Gone soon.');
      await deleteDocument(student, doc.id);
      const left = await system.queryAll(
        `SELECT VALUE id FROM document_version WHERE document = type::record($id); SELECT VALUE id FROM document_delta WHERE document = type::record($id);`,
        { id: doc.id },
      );
      assert.deepEqual(left, [[], []]);
    });

    await t.test('fuzz: every version reads back exactly, across full copies and any kind of text', async () => {
      for (const seed of [1, 2, 3]) {
        const { r, change } = random(seed);
        const doc = await start('');
        const expected: string[] = [''];
        let text = '';
        for (let step = 0; step < 170; step++) {
          // Mostly new versions, so the chain passes full copies; some saves extend the latest.
          if (r(3)) await age(doc.id, '6m');
          const next = change(text);
          if (next === text) continue;
          await doc.type(next);
          text = next;
          const list = await listVersions(student, doc.id);
          expected[list.versions[0].n - 1] = text;
          expected.length = list.versions[0].n;
        }
        const list = await listVersions(student, doc.id);
        assert.equal(list.versions.length, expected.length);
        assert.ok(expected.length > 100, 'the chain should pass a full copy');
        for (let n = expected.length; n >= 1; n--) {
          const version = await readVersion(student, doc.id, n);
          assert.equal(version.content, expected[n - 1], `seed ${seed}, version ${n}`);
          if (n > 1) assert.equal(version.previous, expected[n - 2], `seed ${seed}, before version ${n}`);
        }
        const copies = await system.query<number[]>('SELECT VALUE n FROM document_delta WHERE document = type::record($id) AND snapshot != NONE ORDER BY n', { id: doc.id });
        assert.deepEqual(copies, Array.from({ length: Math.floor((expected.length - 1) / 50) }, (_, i) => (i + 1) * 50));
      }
    });

    await t.test('a changeset is small: a long document stores what changed, not the document', async () => {
      const body = 'A paragraph of a long document, with enough in it to notice. '.repeat(2000);
      const doc = await start(body);
      for (let i = 0; i < 20; i++) {
        await age(doc.id, '6m');
        await doc.type((await readDocument(student, links, plain, doc.id)).content.replace('notice', `notice${i}`));
      }
      const stored = await system.query<{ size: number }[]>(
        `SELECT math::sum(hunks.map(|$h| string::len($h.old))) + string::len(snapshot ?? '') AS size FROM document_delta WHERE document = type::record($id)`,
        { id: doc.id },
      );
      assert.equal(stored.length, 21);
      assert.ok(stored.reduce((sum, row) => sum + row.size, 0) < 200, 'twenty small edits store next to nothing');
    });

    await t.test('writes stay quick on the longest document', async () => {
      const doc = await start('word '.repeat(80_000));
      const began = performance.now();
      await doc.type(`${'word '.repeat(40_000)}new ${'word '.repeat(40_000)}`);
      assert.ok(performance.now() - began < 2000);
    });
  } finally {
    await system.query(`REMOVE NAMESPACE ${namespace};`);
  }
});
