import assert from 'node:assert/strict';
import { test } from 'node:test';
import { latexLayout, latexToMarkdown, markdownMacros, texImageKeys, texWithImageFiles } from '../../../shared/latex.ts';
import { chunkMarkdown, lectureMarkdown } from '../../../shared/lecture.ts';
import { MARKDOWN_MATH } from '../../../shared/math.ts';

const doc = (body: string, preamble = '') => `\\documentclass{article}\n${preamble}\n\\begin{document}\n${body}\n\\end{document}\n`;

test('a problem set becomes sections with its math and macros', () => {
  const md = latexToMarkdown(doc(
    `\\section*{CS 171: Problem Set 1\\\\ {\\small Due: \\duedate}}
\\subsection*{1. Shift cipher (10 points)} % graded
Break it with keys $k_1, k_2 \\in \\Z$, i.e.\\ 50\\% of \\$ cases --- easily.
\\[ \\Enc_k(m) = m + k \\bmod 26 \\]`,
    '\\newcommand{\\Z}{\\mathbb{Z}}\n\\newcommand{\\Enc}{\\mathsf{Enc}}\n\\newcommand{\\duedate}{September 11}',
  ));
  assert.equal(markdownMacros(md), '\\newcommand{\\Z}{\\mathbb{Z}}\n\\newcommand{\\Enc}{\\mathsf{Enc}}\n\\newcommand{\\duedate}{September 11}');
  const body = md.slice(md.indexOf('-->') + 3).trim();
  assert.equal(body, [
    '## CS 171: Problem Set 1 - Due: September 11',
    '',
    '### 1. Shift cipher (10 points)',
    '',
    'Break it with keys $k_1, k_2 \\in \\Z$, i.e. 50% of $ cases - easily.',
    '',
    '$$',
    '\\Enc_k(m) = m + k \\bmod 26',
    '$$',
  ].join('\n'));
  // Sections come from the converted text; the macros comment is not indexed.
  const chunks = chunkMarkdown(lectureMarkdown('tex', doc('\\section{A}\nSome text about ciphers and keys.', '\\newcommand{\\x}{y}')));
  assert.deepEqual(chunks.map((c) => c.heading), ['A']);
  assert.doesNotMatch(chunks[0].text, /newcommand/);
  assert.equal(lectureMarkdown('markdown', '# Same'), '# Same');
});

test('lists, environments, tables and verbatim', () => {
  const md = latexToMarkdown(doc(`\\title{Notes}\\author{Ada \\and Alan}\\maketitle
\\begin{theorem}[Shannon]
Perfect secrecy needs $|K| \\ge |M|$.
\\end{theorem}
\\begin{align*}
a &= b \\\\
c &= d
\\end{align*}
\\begin{enumerate}[label=(\\alph*)]
  \\item First \\textbf{bold} and \\emph{it}
  \\begin{itemize}
    \\item nested \\texttt{code}
  \\end{itemize}
  \\item Second, see \\href{https://x.example}{the site}
\\end{enumerate}
\\begin{tabular}{|l|c|}\\hline
Scheme & Secure? \\\\ \\hline
OTP & yes \\\\
\\end{tabular}
\\begin{verbatim}
  x % 2
\\end{verbatim}
\\begin{proof}
Trivial.
\\end{proof}
Vigen\\\`{e}re and na\\"ive.`));
  assert.equal(md, [
    '# Notes',
    '',
    'Ada, Alan',
    '',
    '**Theorem (Shannon).** Perfect secrecy needs $|K| \\ge |M|$.',
    '',
    '$$',
    '\\begin{aligned}a &= b \\\\',
    'c &= d\\end{aligned}',
    '$$',
    '',
    '- (a) First **bold** and _it_',
    '',
    '  - nested `code`',
    '- (b) Second, see [the site](https://x.example)',
    '',
    '| Scheme | Secure? |',
    '| --- | --- |',
    '| OTP | yes |',
    '',
    '```',
    '  x % 2',
    '```',
    '',
    '_Proof._ Trivial. ∎',
    '',
    'Vigenère and naïve.',
  ].join('\n'));
});

test('fragments and malformed input never throw', () => {
  assert.equal(latexToMarkdown('Just $x$ and \\unknown{kept} \\relax'), 'Just $x$ and kept');
  for (const bad of ['\\begin{itemize}\\item open', '{{{', '$unclosed', '\\section{', '\\newcommand{\\a}{\\a}\\a', '\\newcommand{\\a}{\\a\\a}\\a', '']) {
    assert.equal(typeof latexToMarkdown(bad), 'string');
  }
});

test('colors stay as spans when asked, and only their text otherwise', () => {
  const tex = doc(
    `\\placeholder{TBD} and \\textcolor[HTML]{00FF7F}{mint}, \\textcolor{red!50!black}{dark}, \\colorbox{note}{boxed}, \\textcolor{nosuch}{plain}.`,
    `\\usepackage{xcolor}\n\\definecolor{note}{rgb}{1,1,0.5}\n\\newcommand{\\placeholder}[1]{\\textcolor{red}{#1}}`,
  );
  assert.equal(latexToMarkdown(tex).split('\n').at(-1), 'TBD and mint, dark, boxed, plain.');
  assert.equal(
    latexToMarkdown(tex, { colors: true }).split('\n').at(-1),
    '<span style="color:#ff0000">TBD</span> and <span style="color:#00ff7f">mint</span>, <span style="color:#800000">dark</span>, <span style="background-color:#ffff80">boxed</span>, plain.',
  );
});

test('TikZ pictures stay TikZ, with what they need of the preamble', () => {
  const md = latexToMarkdown(doc(
    `\\begin{figure}\\centering
\\resizebox{0.5\\textwidth}{!}{\\begin{tikzpicture}
\\draw[->, accent] (0,0) -- (\\len,0) node {$\\R$}; % axis
\\end{tikzpicture}}
\\caption{Axis}
\\end{figure}
\\begin{tikzcd} A \\arrow[r] & B \\end{tikzcd}`,
    '\\usepackage[utf8]{inputenc}\n\\usepackage{amsmath,tikz,tikz-cd}\n\\usetikzlibrary{arrows.meta}\n\\tikzstyle{box}=[draw]\n\\definecolor{accent}{HTML}{3366CC}\n\\newcommand{\\R}{\\mathbb{R}}\n\\newcommand{\\len}{2}\n\\newcommand{\\unused}{x}',
  ));
  const body = md.slice(md.indexOf('-->') + 3).trim();
  const preamble = '\\usepackage{amsmath,tikz-cd}\n\\usetikzlibrary{arrows.meta}\n\\tikzstyle{box}=[draw]\n\\definecolor{accent}{HTML}{3366CC}';
  assert.equal(body, [
    '```tikz',
    preamble,
    '\\providecommand{\\R}{}\\renewcommand{\\R}{\\mathbb{R}}',
    '\\providecommand{\\len}{}\\renewcommand{\\len}{2}',
    '\\begin{tikzpicture}',
    '\\draw[->, accent] (0,0) -- (\\len,0) node {$\\R$};',
    '\\end{tikzpicture}',
    '```',
    '',
    '_Axis_',
    '',
    '```tikz',
    preamble,
    '\\begin{tikzcd} A \\arrow[r] & B \\end{tikzcd}',
    '```',
  ].join('\n'));
});

test('colors and styles set up in the text reach the pictures after them', () => {
  const md = latexToMarkdown(doc(
    `\\begin{tikzpicture}\\draw (0,0) -- (1,0);\\end{tikzpicture}
\\begin{center}
\\definecolor{gold}{HTML}{BD7B13}
\\colorlet{soft}{gold!20}
\\tikzset{beam/.style={draw=gold}}
\\begin{tikzpicture}\\draw[beam,fill=soft] (0,0) circle (1);\\end{tikzpicture}
\\end{center}
\\definecolor{gold}{HTML}{BD7B13}
\\begin{tikzpicture}\\fill[gold] (0,0) circle (1);\\end{tikzpicture}`,
    '\\usepackage{tikz}\n\\usetikzlibrary{arrows.meta}',
  ));
  const setup = '\\usetikzlibrary{arrows.meta}\n\\definecolor{gold}{HTML}{BD7B13}\n\\colorlet{soft}{gold!20}\n\\tikzset{beam/.style={draw=gold}}';
  assert.equal(md, [
    '```tikz',
    '\\usetikzlibrary{arrows.meta}',
    '\\begin{tikzpicture}\\draw (0,0) -- (1,0);\\end{tikzpicture}',
    '```',
    '',
    '```tikz',
    setup,
    '\\begin{tikzpicture}\\draw[beam,fill=soft] (0,0) circle (1);\\end{tikzpicture}',
    '```',
    '',
    '```tikz',
    setup,
    '\\begin{tikzpicture}\\fill[gold] (0,0) circle (1);\\end{tikzpicture}',
    '```',
  ].join('\n'));
});

test('every inline formula LaTeX allows reaches KaTeX, whatever it touches', () => {
  const md = latexToMarkdown(doc(String.raw`Gives 0.0004$^\circ$ at 5$\,\mu$m, $\mu$s and $a$$b$, $x +
y$ wrapped, \(q\)r. Prices \$5 and \$6 stay.`));
  assert.equal(md.replace(MARKDOWN_MATH, 'M'), 'Gives 0.0004M at 5Mm, Ms and MM, M wrapped, Mr. Prices $5 and $6 stay.');
  assert.match(md, /\\\(\\mu\\\)s/);
  assert.match(md, /\$x \+ y\$ wrapped/);
});

test('formulas that are not next to a letter stay in dollars', () => {
  assert.equal(latexToMarkdown(doc(String.raw`Keys $k_1$, and ($x$).`)), 'Keys $k_1$, and ($x$).');
});

test('brackets followed by parentheses are text, while a real link stays a link', () => {
  assert.equal(latexToMarkdown(doc(String.raw`Rotation [$\alpha$]($\lambda$) of [a](b).`)), String.raw`Rotation [$\alpha$\]($\lambda$) of [a\](b).`);
  assert.equal(latexToMarkdown(doc(String.raw`See \href{https://x.org}{the site}.`)), 'See [the site](https://x.org).');
});

test('a two-column document keeps the head it prints across both columns', () => {
  const tex = `\\documentclass[10pt,twocolumn]{article}
\\begin{document}
% head
\\twocolumn[{%
\\noindent\\textcolor{red!70!black}{\\textbf{NEWS \\& VIEWS}}\\hfill BioE 163\\par\\vspace{2pt}
\\hrule\\vspace{10pt}
\\section*{Listening to glucose}
\\noindent\\textbf{Rotation $[\\alpha]cL$ is small.}\\par
}]
% end head
\\section{Introduction}
Text.
\\end{document}`;
  assert.deepEqual(latexLayout(tex), { columns: 2, ownHead: true });
  const plain = latexToMarkdown(tex);
  assert.ok(plain.startsWith('**NEWS & VIEWS** BioE 163'), plain);
  assert.ok(plain.includes('## Listening to glucose'));
  assert.ok(plain.includes('**Rotation $[\\alpha]cL$ is small.**'));
  assert.ok(!plain.includes('article-head'), 'the text for search has no markup');
  const shown = latexToMarkdown(tex, { colors: true });
  assert.ok(shown.startsWith('<div class="article-head">\n\n<span style="color:'), shown);
  assert.ok(shown.indexOf('</div>') < shown.indexOf('## Introduction'));
});

test('an image added in the app renders, one from a TeX folder stays named', () => {
  const md = latexToMarkdown(doc(String.raw`Before.

\includegraphics[width=\linewidth]{answer-image:kabc_1}

\begin{figure}
\centering
\includegraphics{answer-image:kdef}
\caption{A plot}
\end{figure}

\includegraphics{figs/plot.pdf}`));
  assert.match(md, /!\[\]\(answer-image:kabc_1\)/);
  assert.match(md, /!\[\]\(answer-image:kdef\)/);
  assert.match(md, /_A plot_/);
  assert.match(md, /\[Figure: plot\.pdf\]/);
});

test('a download points added images at their files and loads graphicx', () => {
  const tex = String.raw`\documentclass[11pt]{article} % paper
\begin{document}
\includegraphics[width=\linewidth]{answer-image:ka}
\includegraphics{answer-image:ka}
\includegraphics{answer-image:kb}
\includegraphics{figs/own.png}
\end{document}
`;
  assert.deepEqual(texImageKeys(tex), ['ka', 'kb']);
  const out = texWithImageFiles(tex, { ka: 'images/ka.png', kb: 'images/kb.jpg' });
  assert.equal(out, String.raw`\documentclass[11pt]{article} % paper
\usepackage{graphicx}
\begin{document}
\includegraphics[width=\linewidth]{images/ka.png}
\includegraphics{images/ka.png}
\includegraphics{images/kb.jpg}
\includegraphics{figs/own.png}
\end{document}
`);
  const loaded = String.raw`\documentclass{article}
\usepackage[final]{graphicx}
\begin{document}\includegraphics{answer-image:ka}\end{document}`;
  assert.equal((texWithImageFiles(loaded, { ka: 'a.png' }).match(/graphicx/g) ?? []).length, 1);
  const commented = String.raw`\documentclass{article}
% \usepackage{graphicx}
\begin{document}\includegraphics{answer-image:ka}\end{document}`;
  assert.match(texWithImageFiles(commented, { ka: 'a.png' }), /article\}\n\\usepackage\{graphicx\}\n%/);
});

test('a one-column document has no layout of its own', () => {
  assert.deepEqual(latexLayout(doc('Text.')), { columns: 1, ownHead: false });
  assert.deepEqual(latexLayout('\\documentclass{article}\n% \\twocolumn[x]\n\\begin{document}Text.\\end{document}'), { columns: 1, ownHead: false });
  assert.deepEqual(latexLayout('\\documentclass[twocolumn]{article}\n\\begin{document}Text.\\end{document}'), { columns: 2, ownHead: false });
});
