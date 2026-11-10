import assert from 'node:assert/strict';
import { test } from 'node:test';
import { parseSheet } from '../../../shared/homework.ts';
import { lectureMarkdown } from '../../../shared/lecture.ts';
import { appendText, applyEdits, rebaseParts } from '../src/homework.ts';

const brief = (parts: ReturnType<typeof parseSheet>) => parts.map((p) => `${p.key} ${p.label}|${p.points ?? '-'}|p${p.page ?? '-'}|${p.anchor}`);

test('a PDF problem set splits into questions, lettered parts and roman sub-parts', () => {
  const sheet = [
    '<!-- page 1 -->',
    '',
    'PHYS 101 HOMEWORK 3',
    'Instructions: show all work. Include labeled diagrams for',
    'Question 3.',
    'Question 1. Optics basics (20 points)',
    'Part (a). Refraction (10 points)',
    '(i) 4 points. State Snell’s law.',
    '(ii) 6 points. Derive the critical angle.',
    '<!-- page 2 -->',
    'Part (b). Lenses (10 points)',
    'A thin lens has focal length f = 5 cm.',
    '(i), 5 points. Find the image distance.',
    'Use the lens equation.',
    '(ii) 5 points. Is the image real?',
    'Question 2. Absorbance (20 points)',
    'A sample absorbs at 280 nm.',
    '(i) 10 points. Use Beer–Lambert.',
    '(c) 10 points. Discuss errors.',
    '<!-- page 3 -->',
    'Question 3. Design an assay (30 points)',
    'For each assay: (1) components; (2) observable.',
    '(1) identify its components;',
    '(2) explain the observable.',
    '<!-- page 4 -->',
    'Constants and useful relationships',
    'Planck constant, h 6.626e-34 J s',
  ].join('\n');
  const parts = parseSheet(sheet);
  assert.deepEqual(brief(parts), [
    '1.a.i a (i)|4|p1|Question 1',
    '1.a.ii a (ii)|6|p1|(ii)',
    '1.b.i b (i)|5|p2|Part (b)',
    '1.b.ii b (ii)|5|p2|(ii)',
    '2.i (i)|10|p2|Question 2',
    '2.c c|10|p2|(c)',
    '3 |30|p3|Question 3',
  ]);
  const b1 = parts[2];
  assert.equal(b1.questionTitle, 'Optics basics');
  assert.equal(b1.prompt, '5 points. Find the image distance.\nUse the lens equation.');
  assert.match(b1.context ?? '', /Lenses \(10 points\)\nA thin lens has focal length/);
  // Numbered items inside a question are its text, and the formula sheet is not.
  assert.match(parts[6].prompt, /\(1\) identify its components;\n\(2\) explain the observable\./);
  assert.doesNotMatch(parts[6].prompt, /Planck/);
});

test('a LaTeX sheet splits along its numbered sections and lists', () => {
  const tex = `\\documentclass{article}\\begin{document}
\\section*{Problem Set 1}
\\subsection*{1. Shift cipher (10 points)}
Break the shift cipher.
\\subsection*{2. Kerckhoffs}
Pick a tool and:
\\begin{enumerate}[label=(\\alph*)]
\\item Try it out.
\\item Check the source.
\\end{enumerate}
\\end{document}`;
  const parts = parseSheet(lectureMarkdown('tex', tex));
  assert.deepEqual(brief(parts), ['1 |10|p-|1.', '2.a a|-|p-|2.', '2.b b|-|p-|(b)']);
  assert.equal(parts[0].questionTitle, 'Shift cipher');
  assert.equal(parts[1].prompt, 'Try it out.');
});

test('a PDF sheet with plain numbered headings splits into its questions, not their numbered steps', () => {
  const hw3 = [
    '<!-- page 1 -->',
    'CS 171: Problem Set 3',
    'Due Date: October 3rd, 2026 at 8:59pm',
    '1. Two Versions of CPA security (10 points)',
    'Prove that definitions 0.1 and 0.2 are equivalent.',
    '1. The challenger samples a key k ← Gen(1n).',
    '2. The adversary A is given input 1n and oracle access to Enc(k, ·), and outputs a pair of',
    'messages (m0, m1) with |m0| = |m1|.',
    '3. The challenger computes the ciphertext c ← Enc(k, mb). Then they give c to A.',
    '<!-- page 3 -->',
    '2. Feistel Network (10 points)',
    'Consider the following three-round Feistel network.',
    '1. Let f : {0, 1}n × {0, 1}n → {0, 1}n.',
    '3. Computation:',
    '(a) F computes L1 := R0 and R1 := L0 ⊕ f (k1, R0).',
    '(b) F computes L2 := R1 and R2 := L1 ⊕ f (k2, R1).',
    'Show that there exists some efficient adversary A.',
  ].join('\n');
  const parts = parseSheet(hw3);
  assert.deepEqual(brief(parts), ['1 |10|p1|1. Two', '2 |10|p3|2. Feistel']);
  assert.equal(parts[0].questionTitle, 'Two Versions of CPA security');
  assert.match(parts[1].prompt, /\(a\) F computes L1[\s\S]*efficient adversary/);

  // Headings without a dot; lettered lines with no numbered item before them are parts.
  const hw5 = [
    '1 Encrypt-Then-Authenticate',
    '1. Describe a MAC MAC′ := (Gen′, Mac′, Verify′) that is secure but not strongly secure.',
    '2. Prove that MAC′ is secure or cite a security proof given in discussion or lecture.2',
    '2 Randomized MACs',
    '1. Construction of ΠD:',
    '(a) GenD(1n): Sample k and output it.',
    '2. Claim 2.1 ΠD is a secure MAC.',
    '3 Composing a One-Way Function with Itself',
    '4. (optional) Exploring Kerckhoff ’s Principle',
    '(a) Try out the tool.',
    '(b) Confirm that the codebase is open source.',
  ].join('\n');
  assert.deepEqual(parseSheet(hw5).map((p) => p.key), ['1', '2', '3', '4.a', '4.b']);
});

test('a sheet without structure is one answer, and repeated labels stay unique', () => {
  assert.deepEqual(brief(parseSheet('Write an essay about light.')), ['1 Answer|-|p1|']);
  const twice = parseSheet('Question 1. A\n(i) x\n(i) y');
  assert.deepEqual(twice.map((p) => p.key), ['1.i', '1.i~2']);
  // "(i)" right after "(h)" is the letter i.
  const letters = parseSheet('Question 1. A\n(g) g\n(h) h\n(i) i\n(j) j');
  assert.deepEqual(letters.map((p) => p.key), ['1.g', '1.h', '1.i', '1.j']);
});

test('AI edits to an answer apply in order and must each match exactly once', () => {
  const answer = 'Let $x = 2$. Then $x^2 = 4$, so $y = 4$.';
  assert.equal(applyEdits(answer, [{ old_text: '$x = 2$', new_text: '$x = 3$' }, { old_text: '$x^2 = 4$, so $y = 4$', new_text: '$x^2 = 9$, so $y = 9$' }]),
    'Let $x = 3$. Then $x^2 = 9$, so $y = 9$.');
  assert.throws(() => applyEdits(answer, [{ old_text: 'z', new_text: '' }]), /not found/);
  assert.throws(() => applyEdits(answer, [{ old_text: '$y', new_text: '' }, { old_text: '= 4', new_text: '' }]), /Edit 2: old_text occurs more than once/);
});

test('appended text becomes its own paragraph', () => {
  assert.equal(appendText('', 'First.'), 'First.');
  assert.equal(appendText('First.\n\n', '\nSecond.'), 'First.\n\nSecond.');
});

test('a new sheet lines up with the parts by key and loses no answer', () => {
  const rows = [{ id: 'answer:a', key: '1' }, { id: 'answer:b', key: '2.a' }, { id: 'answer:c', key: 'x' }];
  const sheet = parseSheet('<!-- page 1 -->\nQuestion 1. A\nDo it.\n<!-- page 2 -->\nQuestion 2. B\n(a) one\n(b) two');
  const plan = rebaseParts(rows, sheet);
  assert.deepEqual(plan.update.map((p) => `${p.id} ${p.key} p${p.page} #${p.position}`), ['answer:a 1 p1 #0', 'answer:b 2.a p2 #1']);
  assert.deepEqual(plan.create.map((p) => `${p.key} #${p.position}`), ['2.b #2']);
  assert.match(plan.create[0].id, /^answer:k[0-9a-f]{32}$/);
  assert.deepEqual(plan.kept, [{ id: 'answer:c', key: 'x', position: 3 }]);
  // The answer itself is never part of the plan.
  assert.ok(plan.update.every((p) => !('content' in p)));
});
