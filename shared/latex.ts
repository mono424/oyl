/**
 * LaTeX source (a `tex` material: a problem set, notes, a template) to the
 * Markdown the rest of the app works with: the search index, the tutor and
 * the student's progress read sections of it, and the web app renders it with
 * its math. Covers what course handouts use: sections, lists, theorem-like
 * environments, text formatting, links, tables, verbatim, and every kind of
 * math, which stays LaTeX for KaTeX. TikZ pictures stay TikZ, in a ```tikz block
 * with what they need of the preamble, for the web app to draw. `\newcommand`s become KaTeX macros
 * (in a `latex-macros` comment, which the index drops) and are expanded in
 * the text. Deterministic, so server and client get the same sections.
 */

import { inlineMarkdownMath } from './math';

interface Macro {
  args: number;
  /** Default of the first argument, when it is optional. */
  optional: string | null;
  body: string;
}

const MATH_ENVS = new Set([
  'equation', 'equation*', 'align', 'align*', 'gather', 'gather*', 'multline', 'multline*', 'eqnarray', 'eqnarray*',
  'displaymath', 'math', 'flalign', 'flalign*', 'alignat', 'alignat*',
]);
const VERBATIM_ENVS = new Set(['verbatim', 'verbatim*', 'Verbatim', 'lstlisting', 'minted', 'comment']);
const TIKZ_ENVS = new Set(['tikzpicture', 'tikzcd']);
// The packages a picture may load: the ones TikZJax (apps/web/src/lib/tikz.ts) carries. It preloads tikz and xcolor.
const TIKZ_PACKAGES = new Set([
  'amsmath', 'amssymb', 'amsfonts', 'amsbsy', 'amstext', 'amsopn', 'amsgen', 'array', 'etoolbox', 'xparse',
  'pgfplots', 'tikz-cd', 'tikz-3dplot', 'hf-tikz',
]);
const LIST_ENVS = new Set(['itemize', 'enumerate', 'description', 'compactitem', 'compactenum', 'asparaenum', 'inparaenum']);
const THEOREMS: Record<string, string> = {
  theorem: 'Theorem', lemma: 'Lemma', corollary: 'Corollary', proposition: 'Proposition', claim: 'Claim', fact: 'Fact',
  definition: 'Definition', remark: 'Remark', example: 'Example', exercise: 'Exercise', problem: 'Problem',
  question: 'Question', solution: 'Solution', note: 'Note', observation: 'Observation', conjecture: 'Conjecture',
  assumption: 'Assumption', construction: 'Construction', hint: 'Hint', answer: 'Answer',
};
const HEADINGS: Record<string, number> = { part: 1, chapter: 1, section: 2, subsection: 3, subsubsection: 4, paragraph: 5, subparagraph: 6 };
// Switches and spacing that carry no text.
const DROP = new Set([
  'noindent', 'indent', 'medskip', 'bigskip', 'smallskip', 'newpage', 'clearpage', 'cleardoublepage', 'centering',
  'raggedright', 'raggedleft', 'vfill', 'null', 'tiny', 'scriptsize', 'footnotesize', 'small', 'normalsize',
  'large', 'Large', 'LARGE', 'huge', 'Huge', 'bfseries', 'itshape', 'ttfamily', 'rmfamily', 'sffamily', 'upshape',
  'mdseries', 'normalfont', 'em', 'bf', 'it', 'tt', 'rm', 'sf', 'sc', 'hline', 'toprule', 'midrule', 'bottomrule',
  'tableofcontents', 'protect', 'relax', 'linebreak', 'pagebreak', 'nopagebreak', 'allowbreak', 'frenchspacing',
  'maketitle', 'appendix', 'printbibliography', 'onecolumn', 'today', 'label', 'vspace', 'hspace',
  'setlength', 'addtolength', 'pagestyle', 'thispagestyle', 'fancyhead', 'fancyfoot', 'lhead', 'rhead', 'chead',
  'lfoot', 'rfoot', 'cfoot', 'setcounter', 'addtocounter', 'usepackage', 'documentclass', 'bibliographystyle',
  'bibliography', 'input', 'include', 'graphicspath', 'newtheorem', 'theoremstyle', 'hypersetup', 'geometry',
  'definecolor', 'colorlet', 'tikzset', 'usetikzlibrary', 'pgfplotsset', 'color', 'pagenumbering', 'renewenvironment', 'newenvironment', 'setlist', 'captionsetup',
]);
const WORDS: Record<string, string> = {
  LaTeX: 'LaTeX', TeX: 'TeX', ldots: '...', dots: '...', textellipsis: '...', quad: ' ', qquad: ' ', enspace: ' ',
  thinspace: ' ', hfill: ' ', textbackslash: '\\', textasciitilde: '~', textasciicircum: '^', textbar: '|', textless: '<',
  textgreater: '>', textendash: '-', textemdash: '-', textquoteleft: "'", textquoteright: "'", textquotedblleft: '"',
  textquotedblright: '"', S: '§', P: '¶', copyright: '©', ss: 'ß', ae: 'æ', AE: 'Æ', o: 'ø', O: 'Ø', aa: 'å',
  AA: 'Å', l: 'ł', L: 'Ł', i: 'ı', checkmark: '✓', qed: '∎', newline: '\n', par: '\n\n', item: '', and: ', ',
};
const ACCENTS: Record<string, string> = {
  '`': '̀', "'": '́', '^': '̂', '"': '̈', '~': '̃', '=': '̄', '.': '̇',
  c: '̧', v: '̌', u: '̆', H: '̋', k: '̨', r: '̊',
};

const isLetter = (c: string | undefined) => !!c && /[A-Za-z@]/.test(c);

/** `{...}` starting at `i` (after spaces): its content and the index after it. */
function group(s: string, i: number): { text: string; end: number } | null {
  let j = i;
  while (j < s.length && /[ \t]/.test(s[j])) j++;
  if (s[j] !== '{') return null;
  let depth = 0;
  for (let k = j; k < s.length; k++) {
    if (s[k] === '\\') {
      k++;
      continue;
    }
    if (s[k] === '{') depth++;
    else if (s[k] === '}' && --depth === 0) return { text: s.slice(j + 1, k), end: k + 1 };
  }
  return { text: s.slice(j + 1), end: s.length };
}

/** `[...]` starting at `i` (after spaces), braces respected. */
function optional(s: string, i: number): { text: string; end: number } | null {
  let j = i;
  while (j < s.length && /[ \t]/.test(s[j])) j++;
  if (s[j] !== '[') return null;
  let depth = 0;
  for (let k = j + 1; k < s.length; k++) {
    if (s[k] === '\\') {
      k++;
      continue;
    }
    if (s[k] === '{') depth++;
    else if (s[k] === '}') depth--;
    else if (s[k] === ']' && depth === 0) return { text: s.slice(j + 1, k), end: k + 1 };
  }
  return null;
}

/** A required argument: a group, or the next token. */
function argument(s: string, i: number): { text: string; end: number } {
  const g = group(s, i);
  if (g) return g;
  let j = i;
  while (j < s.length && /\s/.test(s[j])) j++;
  if (s[j] === '\\') {
    let k = j + 1;
    if (isLetter(s[k])) while (isLetter(s[k])) k++;
    else k++;
    return { text: s.slice(j, k), end: k };
  }
  return { text: s[j] ?? '', end: Math.min(s.length, j + 1) };
}

/** Where `\end{name}` closes the environment whose content starts at `from`, nesting respected. */
function envEnd(s: string, name: string, from: number): { inner: string; end: number } {
  const open = `\\begin{${name}}`;
  const close = `\\end{${name}}`;
  let depth = 1;
  let i = from;
  while (i < s.length) {
    const o = s.indexOf(open, i);
    const c = s.indexOf(close, i);
    if (c < 0) return { inner: s.slice(from), end: s.length };
    if (o >= 0 && o < c) {
      depth++;
      i = o + open.length;
    } else {
      if (--depth === 0) return { inner: s.slice(from, c), end: c + close.length };
      i = c + close.length;
    }
  }
  return { inner: s.slice(from), end: s.length };
}

function stripComments(s: string): string {
  let verbatim: string | null = null;
  return s
    .split('\n')
    .map((line) => {
      // Verbatim text keeps its percent signs.
      if (verbatim) {
        if (line.includes(`\\end{${verbatim}}`)) verbatim = null;
        return line;
      }
      const v = /\\begin\{(verbatim\*?|Verbatim|lstlisting|minted)\}/.exec(line);
      if (v && !line.includes(`\\end{${v[1]}}`)) {
        verbatim = v[1];
        return line;
      }
      for (let i = 0; i < line.length; i++) {
        if (line[i] === '\\') i++;
        else if (line[i] === '%') return line.slice(0, i);
      }
      return line;
    })
    .join('\n');
}

/** Pulls `\newcommand` and friends out of `s`, into `macros`. */
function takeMacros(s: string, macros: Map<string, Macro>): string {
  let out = '';
  let i = 0;
  const def = /\\(?:(re)?newcommand\*?|providecommand\*?|DeclareMathOperator(\*?)|def|let)(?![A-Za-z@])/g;
  for (let m = def.exec(s); m; m = def.exec(s)) {
    if (m.index < i) continue;
    let j = m.index + m[0].length;
    let name: string;
    const kind = m[0];
    if (kind === '\\def' || kind === '\\let') {
      const n = /^\s*(\\[A-Za-z@]+)/.exec(s.slice(j));
      if (!n) continue;
      name = n[1];
      j += n[0].length;
      if (kind === '\\let') {
        const t = /^\s*=?\s*(\\[A-Za-z@]+|.)/.exec(s.slice(j));
        if (!t) continue;
        macros.set(name, { args: 0, optional: null, body: t[1] });
        out += s.slice(i, m.index);
        i = j + t[0].length;
        def.lastIndex = i;
        continue;
      }
      const params = /^(?:#\d)*/.exec(s.slice(j))![0];
      j += params.length;
      const body = group(s, j);
      if (!body) continue;
      macros.set(name, { args: params.length / 2, optional: null, body: body.text });
      out += s.slice(i, m.index);
      i = body.end;
      def.lastIndex = i;
      continue;
    }
    const n = argument(s, j);
    name = n.text.trim();
    j = n.end;
    if (!name.startsWith('\\')) continue;
    if (kind.startsWith('\\DeclareMathOperator')) {
      const body = group(s, j);
      if (!body) continue;
      macros.set(name, { args: 0, optional: null, body: `\\operatorname${m[2] ? '*' : ''}{${body.text}}` });
      out += s.slice(i, m.index);
      i = body.end;
      def.lastIndex = i;
      continue;
    }
    const count = optional(s, j);
    let args = 0;
    if (count) {
      args = Number.parseInt(count.text, 10) || 0;
      j = count.end;
    }
    const opt = optional(s, j);
    if (opt) j = opt.end;
    const body = group(s, j);
    if (!body) continue;
    // \providecommand never overrides; the others do.
    if (!(kind.startsWith('\\providecommand') && macros.has(name))) macros.set(name, { args, optional: opt?.text ?? null, body: body.text });
    out += s.slice(i, m.index);
    i = body.end;
    def.lastIndex = i;
  }
  return out + s.slice(i);
}

/** Replaces `#1`... in a macro body with its arguments. */
const fill = (body: string, args: string[]) => body.replace(/#(\d)/g, (_m, n: string) => args[Number(n) - 1] ?? '');

/** The macros as definitions KaTeX accepts. */
function katexMacros(macros: Map<string, Macro>): string {
  return [...macros]
    .filter(([name]) => /^\\[A-Za-z]+$/.test(name))
    .map(([name, m]) => `\\newcommand{${name}}${m.args ? `[${m.args}]` : ''}{${m.body.replace(/\n/g, ' ')}}`)
    .join('\n');
}

type Rgb = [number, number, number];

// xcolor's base colors.
const BASE_COLORS: Record<string, Rgb> = {
  red: [1, 0, 0], green: [0, 1, 0], blue: [0, 0, 1], cyan: [0, 1, 1], magenta: [1, 0, 1], yellow: [1, 1, 0],
  black: [0, 0, 0], white: [1, 1, 1], gray: [0.5, 0.5, 0.5], darkgray: [0.25, 0.25, 0.25],
  lightgray: [0.75, 0.75, 0.75], brown: [0.75, 0.5, 0.25], lime: [0.75, 1, 0], olive: [0.5, 0.5, 0],
  orange: [1, 0.5, 0], pink: [1, 0.75, 0.75], purple: [0.75, 0, 0.25], teal: [0, 0.5, 0.5], violet: [0.5, 0, 0.5],
};

/** A color given in an xcolor model (rgb, RGB, HTML, gray, cmyk), or by name with `!` mixes (`red!50!black`). */
function parseColor(spec: string, model: string | null, named: Map<string, Rgb>): Rgb | null {
  const n = spec.split(',').map((x) => Number.parseFloat(x));
  const unit = (v: Rgb): Rgb | null => (v.every((x) => Number.isFinite(x)) ? (v.map((x) => Math.min(1, Math.max(0, x))) as Rgb) : null);
  switch (model?.trim()) {
    case 'rgb':
      return n.length === 3 ? unit([n[0], n[1], n[2]]) : null;
    case 'RGB':
      return n.length === 3 ? unit([n[0] / 255, n[1] / 255, n[2] / 255]) : null;
    case 'HTML': {
      const h = /^\s*([0-9A-Fa-f]{6})\s*$/.exec(spec)?.[1];
      return h ? [0, 2, 4].map((k) => Number.parseInt(h.slice(k, k + 2), 16) / 255) as Rgb : null;
    }
    case 'gray':
      return n.length === 1 ? unit([n[0], n[0], n[0]]) : null;
    case 'cmyk':
      return n.length === 4 ? unit([(1 - n[0]) * (1 - n[3]), (1 - n[1]) * (1 - n[3]), (1 - n[2]) * (1 - n[3])]) : null;
    case undefined:
    case '':
    case 'named':
      break;
    default:
      return null;
  }
  // c1!p1!c2!p2!c3...: p1% of c1 with c2, p2% of that with c3; a missing last color is white.
  const parts = spec.split('!').map((p) => p.trim());
  const color = (name: string) => named.get(name) ?? BASE_COLORS[name] ?? null;
  let out = color(parts[0]);
  for (let k = 1; out && k < parts.length; k += 2) {
    const p = Number.parseFloat(parts[k]) / 100;
    const other = k + 1 < parts.length ? color(parts[k + 1]) : BASE_COLORS.white;
    if (!Number.isFinite(p) || !other) return null;
    const a = out;
    out = [0, 1, 2].map((c) => a[c] * Math.min(1, Math.max(0, p)) + other[c] * (1 - Math.min(1, Math.max(0, p)))) as Rgb;
  }
  return out;
}

/** `\definecolor` and `\colorlet` anywhere in `s`, by name. */
function takeColors(s: string): Map<string, Rgb> {
  const named = new Map<string, Rgb>();
  const def = /\\(definecolor|colorlet)(?![A-Za-z@])/g;
  for (let m = def.exec(s); m; m = def.exec(s)) {
    let k = m.index + m[0].length;
    const args: string[] = [];
    for (let n = m[1] === 'definecolor' ? 3 : 2; n > 0; n--) {
      for (let o = optional(s, k); o; o = optional(s, k)) k = o.end;
      const g = group(s, k);
      if (!g) break;
      args.push(g.text.trim());
      k = g.end;
    }
    const rgb = m[1] === 'definecolor' && args.length === 3 ? parseColor(args[2], args[1], named)
      : m[1] === 'colorlet' && args.length === 2 ? parseColor(args[1], null, named) : null;
    if (rgb) named.set(args[0], rgb);
  }
  return named;
}

const TIKZ_SETUP = /\\(usepackage|usetikzlibrary|usepgfplotslibrary|tikzset|pgfplotsset|definecolor|colorlet|tikzstyle)(?![A-Za-z@])/g;

/** The TikZ setup command `m` found in `s` (a package TikZJax carries, a library, a style or a color), as a picture needs it, and where it ends. */
function tikzSetup(s: string, m: RegExpExecArray): { text: string; end: number } | null {
  let k = m.index + m[0].length;
  if (m[1] === 'usepackage') {
    const o = optional(s, k);
    const g = group(s, o ? o.end : k);
    if (!g) return null;
    const names = g.text.split(',').map((n) => n.trim()).filter((n) => TIKZ_PACKAGES.has(n));
    return { text: names.length ? `\\usepackage${o ? `[${o.text}]` : ''}{${names.join(',')}}` : '', end: g.end };
  }
  if (m[1] === 'tikzstyle') {
    // \tikzstyle{name}=[options]
    const g = group(s, k);
    const eq = g && /^\s*=/.exec(s.slice(g.end));
    const o = g && eq ? optional(s, g.end + eq[0].length) : null;
    return o ? { text: s.slice(m.index, o.end), end: o.end } : null;
  }
  for (let n = m[1] === 'definecolor' ? 3 : m[1] === 'colorlet' ? 2 : 1; n > 0; n--) {
    for (let o = optional(s, k); o; o = optional(s, k)) k = o.end;
    const g = group(s, k);
    if (!g) return null;
    k = g.end;
  }
  return { text: s.slice(m.index, k), end: k };
}

/** What TikZ pictures need of the preamble: packages, libraries, styles and colors. */
function tikzPreamble(preamble: string): string {
  const lines: string[] = [];
  const cmd = new RegExp(TIKZ_SETUP);
  for (let m = cmd.exec(preamble); m; m = cmd.exec(preamble)) {
    const setup = tikzSetup(preamble, m);
    if (!setup) continue;
    if (setup.text) lines.push(setup.text);
    cmd.lastIndex = setup.end;
  }
  return lines.join('\n');
}

const hex = (c: Rgb) => `#${c.map((x) => Math.round(x * 255).toString(16).padStart(2, '0')).join('')}`;

const placeholder = (n: number) => `\u0000${n}\u0000`;

/** How deep macros may expand into macros, and how often per document: a self-referencing one stops. */
const MAX_EXPANSION = 20;
const MAX_EXPANSIONS = 5000;

class Converter {
  private readonly held: string[] = [];
  private expanding = 0;
  private expansions = 0;
  title: string | null = null;
  author: string | null = null;
  date: string | null = null;
  /** What the document's TikZ pictures need of its preamble. */
  tikzPreamble = '';

  /** `colors`: the defined colors, when the text keeps its colors (as HTML spans); null drops them. */
  constructor(
    private readonly macros: Map<string, Macro>,
    private readonly colors: Map<string, Rgb> | null = null,
  ) {}

  hold(text: string): string {
    this.held.push(text);
    return placeholder(this.held.length - 1);
  }

  restore(s: string): string {
    // Held text may hold placeholders itself (math inside a code block never does).
    for (let k = 0; k < 3 && s.includes('\u0000'); k++) s = s.replace(/\u0000(\d+)\u0000/g, (_m, n: string) => this.held[Number(n)] ?? '');
    return s;
  }

  /** Math and verbatim out of the way, so the text pass cannot touch them. */
  protect(s: string): string {
    let out = '';
    let i = 0;
    while (i < s.length) {
      const begin = /^\\begin\{([A-Za-z]+\*?)\}/.exec(s.slice(i, i + 40));
      if (begin && TIKZ_ENVS.has(begin[1])) {
        const { end } = envEnd(s, begin[1], i + begin[0].length);
        out += this.hold(`\n\n\`\`\`tikz\n${this.picture(s.slice(i, end))}\n\`\`\`\n\n`);
        i = end;
        continue;
      }
      if (begin && (MATH_ENVS.has(begin[1]) || VERBATIM_ENVS.has(begin[1]))) {
        const name = begin[1];
        let from = i + begin[0].length;
        if (name === 'minted' || name === 'lstlisting') {
          const o = optional(s, from);
          if (o) from = o.end;
          if (name === 'minted') from = group(s, from)?.end ?? from;
        }
        if (name === 'alignat' || name === 'alignat*') from = group(s, from)?.end ?? from;
        const { inner, end } = envEnd(s, name, from);
        if (VERBATIM_ENVS.has(name)) {
          out += name === 'comment' ? '' : this.hold(`\n\n\`\`\`\n${inner.replace(/^\n+|\s+$/g, '')}\n\`\`\`\n\n`);
        } else {
          out += this.hold(`\n\n$$\n${displayMath(name, inner, s.slice(i + begin[0].length, from))}\n$$\n\n`);
        }
        i = end;
        continue;
      }
      if (s.startsWith('\\verb', i) && !isLetter(s[i + 5])) {
        const delim = s[i + 5];
        const close = s.indexOf(delim, i + 6);
        if (delim && close > 0) {
          out += this.hold(`\`${s.slice(i + 6, close)}\``);
          i = close + 1;
          continue;
        }
      }
      if (s[i] === '\\' && (s[i + 1] === '[' || s[i + 1] === '(')) {
        const display = s[i + 1] === '[';
        const close = s.indexOf(display ? '\\]' : '\\)', i + 2);
        if (close > 0) {
          const math = s.slice(i + 2, close).trim();
          out += this.hold(display ? `\n\n$$\n${math}\n$$\n\n` : inlineMarkdownMath(math, s[i - 1], s[close + 2]));
          i = close + 2;
          continue;
        }
      }
      if (s[i] === '\\') {
        this.setup(s, i);
        out += s.slice(i, i + 2);
        i += 2;
        continue;
      }
      if (s[i] === '$') {
        const display = s[i + 1] === '$';
        const start = i + (display ? 2 : 1);
        let close = -1;
        for (let k = start; k < s.length; k++) {
          if (s[k] === '\\') {
            k++;
            continue;
          }
          if (s[k] === '$' && (!display || s[k + 1] === '$')) {
            close = k;
            break;
          }
        }
        if (close > 0) {
          const math = s.slice(start, close).trim();
          out += this.hold(display ? `\n\n$$\n${math}\n$$\n\n` : inlineMarkdownMath(math, s[i - 1], s[close + 1]));
          i = close + (display ? 2 : 1);
          continue;
        }
      }
      out += s[i];
      i++;
    }
    return out;
  }

  /** A color, style or library set up in the text (TeX reads it there too): the pictures after it get it. */
  private setup(s: string, i: number) {
    const cmd = new RegExp(TIKZ_SETUP.source, 'y');
    cmd.lastIndex = i;
    const m = cmd.exec(s);
    if (!m || m[1] === 'usepackage') return;
    const text = tikzSetup(s, m)?.text;
    if (text && !this.tikzPreamble.split('\n').includes(text)) this.tikzPreamble = [this.tikzPreamble, text].filter(Boolean).join('\n');
  }

  /** A TikZ picture as TeX can draw it on its own: the preamble it needs, the macros it uses, the picture. */
  private picture(src: string): string {
    const used = new Set<string>();
    const visit = (t: string) => {
      for (const [name] of t.matchAll(/\\[A-Za-z]+/g)) {
        const m = this.macros.get(name);
        if (m && !used.has(name)) {
          used.add(name);
          visit(m.body);
        }
      }
    };
    visit(this.tikzPreamble);
    visit(src);
    // \providecommand first: \renewcommand then works whether TeX knows the name or not.
    const defs = [...this.macros]
      .filter(([name]) => used.has(name))
      .map(([name, m]) => `\\providecommand{${name}}{}\\renewcommand{${name}}${m.args ? `[${m.args}]` : ''}${m.args && m.optional !== null ? `[${m.optional}]` : ''}{${m.body}}`);
    return [this.tikzPreamble, ...defs, src.trim().replace(/[ \t]+$/gm, '')].filter(Boolean).join('\n');
  }

  convert(s: string): string {
    let out = '';
    let i = 0;
    while (i < s.length) {
      const c = s[i];
      if (c === '\u0000') {
        const close = s.indexOf('\u0000', i + 1);
        out += s.slice(i, close + 1);
        i = close + 1;
        continue;
      }
      if (c === '\\') {
        const next = s[i + 1];
        if (next === '\\') {
          // A line break; `\\[2pt]` spacing goes with it.
          i += 2;
          const o = optional(s, i);
          if (o && /^[\d.\s]*[a-z]*$/.test(o.text)) i = o.end;
          out += '\n';
          continue;
        }
        if (next && '%$&#_{} '.includes(next)) {
          out += next === ' ' ? ' ' : next;
          i += 2;
          continue;
        }
        if (next && next in ACCENTS && !(isLetter(next) && isLetter(s[i + 2]))) {
          const a = argument(s, i + 2);
          out += `${this.convert(a.text)}${ACCENTS[next]}`.normalize('NFC');
          i = a.end;
          continue;
        }
        if (next === ',' || next === ';' || next === ':' || next === '!' || next === '/' || next === '-') {
          out += next === ',' || next === ';' || next === ':' ? ' ' : '';
          i += 2;
          continue;
        }
        if (!isLetter(next)) {
          out += next ?? '';
          i += 2;
          continue;
        }
        let j = i + 1;
        while (isLetter(s[j])) j++;
        const name = s.slice(i + 1, j);
        const r = this.command(name, s, j);
        out += r.text;
        i = r.end;
        continue;
      }
      if (c === '{' || c === '}') {
        i++;
        continue;
      }
      if (c === '\n') {
        // The source's indentation is not Markdown's.
        out += '\n';
        i++;
        while (s[i] === ' ' || s[i] === '\t') i++;
        continue;
      }
      if (c === '~') {
        out += ' ';
        i++;
        continue;
      }
      if (c === '-' && s[i + 1] === '-') {
        out += '-';
        i += s[i + 2] === '-' ? 3 : 2;
        continue;
      }
      if ((c === '`' && s[i + 1] === '`') || (c === "'" && s[i + 1] === "'")) {
        out += '"';
        i += 2;
        continue;
      }
      if (c === '`') {
        out += "'";
        i++;
        continue;
      }
      // Plain text, or Markdown would read `[x](y)` as a link (\href makes its own).
      if (c === ']' && s[i + 1] === '(') out += '\\';
      out += c;
      i++;
    }
    return out;
  }

  private command(name: string, s: string, j: number): { text: string; end: number } {
    // Spaces after a control word belong to it.
    const skip = (k: number) => {
      while (s[k] === ' ' || s[k] === '\t') k++;
      return k;
    };
    const macro = this.macros.get(`\\${name}`);
    if (macro) {
      const args: string[] = [];
      let k = j;
      if (macro.optional !== null) {
        const o = optional(s, k);
        args.push(o ? o.text : macro.optional);
        if (o) k = o.end;
      }
      while (args.length < macro.args) {
        const a = argument(s, k);
        args.push(a.text);
        k = a.end;
      }
      const end = macro.args ? k : skip(k);
      if (this.expanding >= MAX_EXPANSION || ++this.expansions > MAX_EXPANSIONS) return { text: '', end };
      this.expanding++;
      try {
        return { text: this.convert(this.protect(fill(macro.body, args))), end };
      } finally {
        this.expanding--;
      }
    }
    if (name in HEADINGS) {
      let k = j;
      if (s[k] === '*') k++;
      const o = optional(s, k);
      if (o) k = o.end;
      const g = argument(s, k);
      // An explicit `\\` separates parts of the heading; other line breaks are spaces.
      const text = this.convert(g.text.replace(/\\\\/g, '\u0001'))
        .replace(/\s+/g, ' ')
        .replace(/\s*\u0001\s*/g, ' - ')
        .trim();
      return { text: `\n\n${'#'.repeat(HEADINGS[name])} ${text}\n\n`, end: g.end };
    }
    if (name === 'begin') {
      const g = group(s, j);
      if (!g) return { text: '', end: j };
      return this.environment(g.text, s, g.end);
    }
    if (name === 'end') return { text: '', end: group(s, j)?.end ?? j };
    if (name === 'title' || name === 'author' || name === 'date') {
      const o = optional(s, j);
      const g = argument(s, o ? o.end : j);
      const text = this.convert(g.text).replace(/\s*\n\s*/g, ', ').replace(/\s+/g, ' ').replace(/\s+,/g, ',').trim();
      this[name] = text || null;
      return { text: '', end: g.end };
    }
    if (name === 'maketitle') {
      const lines = [this.title ? `# ${this.title}` : '', [this.author, this.date].filter(Boolean).join(', ')].filter(Boolean);
      return { text: lines.length ? `\n\n${lines.join('\n\n')}\n\n` : '', end: skip(j) };
    }
    const wrap = (left: string, right = left) => {
      const g = argument(s, j);
      const inner = this.convert(g.text).trim();
      return { text: inner ? `${left}${inner}${right}` : '', end: g.end };
    };
    switch (name) {
      case 'textbf':
      case 'mathbf':
        return wrap('**');
      case 'textit':
      case 'emph':
      case 'textsl':
        return wrap('_');
      case 'texttt': {
        const g = argument(s, j);
        return { text: `\`${this.convert(g.text).replace(/`/g, "'")}\``, end: g.end };
      }
      case 'underline':
      case 'textrm':
      case 'textsf':
      case 'textsc':
      case 'textup':
      case 'textmd':
      case 'textnormal':
      case 'mbox':
      case 'hbox':
      case 'text':
      case 'makebox':
      case 'fbox':
      case 'framebox': {
        let k = j;
        if (name === 'makebox' || name === 'framebox') for (let o = optional(s, k); o; o = optional(s, k)) k = o.end;
        const g = argument(s, k);
        return { text: this.convert(g.text), end: g.end };
      }
      case 'scalebox':
      case 'resizebox': {
        // \scalebox{factor}[v]{content}, \resizebox{width}{height}{content}: the content.
        let k = argument(s, j).end;
        if (name === 'resizebox') k = argument(s, k).end;
        else k = optional(s, k)?.end ?? k;
        const g = argument(s, k);
        return { text: this.convert(g.text), end: g.end };
      }
      case 'textcolor':
      case 'colorbox': {
        const model = optional(s, j);
        const spec = argument(s, model ? model.end : j);
        const g = argument(s, spec.end);
        const text = this.convert(g.text);
        const rgb = this.colors && parseColor(spec.text.trim(), model?.text ?? null, this.colors);
        if (!rgb) return { text, end: g.end };
        const style = `${name === 'colorbox' ? 'background-color' : 'color'}:${hex(rgb)}`;
        // A span per paragraph: Markdown ends inline HTML at a blank line.
        const colored = text.split(/(\n{2,})/).map((p, n) => (n % 2 || !p.trim() ? p : `<span style="${style}">${p}</span>`)).join('');
        return { text: colored, end: g.end };
      }
      case 'twocolumn': {
        // Its optional argument runs across both columns: usually the article's head.
        const o = optional(s, j);
        if (!o) return { text: '', end: skip(j) };
        const head = this.convert(o.text).trim();
        return { text: this.colors ? `\n\n<div class="article-head">\n\n${head}\n\n</div>\n\n` : `\n\n${head}\n\n`, end: o.end };
      }
      case 'url': {
        const g = argument(s, j);
        return { text: `<${g.text.trim()}>`, end: g.end };
      }
      case 'href': {
        const u = argument(s, j);
        const t = argument(s, u.end);
        return { text: `[${this.convert(t.text).trim()}](${u.text.trim()})`, end: t.end };
      }
      case 'footnote': {
        const g = argument(s, j);
        return { text: ` (${this.convert(g.text).trim()})`, end: g.end };
      }
      case 'caption': {
        const o = optional(s, j);
        const g = argument(s, o ? o.end : j);
        return { text: `\n\n_${this.convert(g.text).trim()}_\n\n`, end: g.end };
      }
      case 'includegraphics': {
        const o = optional(s, j);
        const g = argument(s, o ? o.end : j);
        const added = /^answer-image:([A-Za-z0-9_-]+)$/.exec(g.text.trim());
        if (added) return { text: `![](answer-image:${added[1]})`, end: g.end };
        return { text: `[Figure: ${g.text.split('/').pop()}]`, end: g.end };
      }
      case 'ref':
      case 'eqref':
      case 'pageref':
      case 'autoref':
      case 'cref':
      case 'Cref': {
        const g = argument(s, j);
        return { text: '(ref)', end: g.end };
      }
      case 'cite':
      case 'citep':
      case 'citet': {
        const o = optional(s, j);
        const g = argument(s, o ? o.end : j);
        return { text: `[${g.text.trim()}]`, end: g.end };
      }
      case 'item': {
        const o = optional(s, j);
        return { text: o ? `${this.convert(o.text)} ` : '', end: o ? o.end : skip(j) };
      }
    }
    if (name in WORDS) return { text: WORDS[name], end: WORDS[name] === '' || /[A-Za-z]$/.test(WORDS[name]) ? skip(j) : j };
    if (DROP.has(name)) {
      // Their arguments carry no text either.
      let k = j;
      if (s[k] === '*') k++;
      for (let o = optional(s, k); o; o = optional(s, k)) k = o.end;
      const args = ['vspace', 'hspace', 'label', 'usepackage', 'documentclass', 'setcounter', 'addtocounter', 'pagestyle', 'thispagestyle', 'bibliographystyle', 'bibliography', 'input', 'include', 'graphicspath', 'hypersetup', 'geometry', 'color', 'pagenumbering', 'setlist', 'captionsetup', 'tikzset', 'usetikzlibrary', 'pgfplotsset'].includes(name)
        ? 1
        : ['setlength', 'addtolength', 'colorlet', 'fancyhead', 'fancyfoot', 'lhead', 'rhead', 'chead', 'lfoot', 'rfoot', 'cfoot', 'theoremstyle'].includes(name)
          ? (['lhead', 'rhead', 'chead', 'lfoot', 'rfoot', 'cfoot', 'theoremstyle', 'fancyhead', 'fancyfoot'].includes(name) ? 1 : 2)
          : name === 'newtheorem' || name === 'definecolor'
            ? 3
            : name === 'newenvironment' || name === 'renewenvironment'
              ? 3
              : 0;
      for (let n = 0; n < args; n++) {
        const g = group(s, k);
        if (!g) break;
        k = g.end;
        for (let o = optional(s, k); o; o = optional(s, k)) k = o.end;
      }
      return { text: '', end: args ? k : skip(k) };
    }
    // Anything else: keep the text of its argument, if it takes one.
    const g = group(s, j);
    return g ? { text: this.convert(g.text), end: g.end } : { text: '', end: skip(j) };
  }

  private environment(name: string, s: string, from: number): { text: string; end: number } {
    let k = from;
    const opt = optional(s, k);
    if (opt) k = opt.end;
    if (name === 'tabular' || name === 'tabular*' || name === 'tabularx' || name === 'array' || name === 'longtable') {
      if (name === 'tabular*' || name === 'tabularx') k = group(s, k)?.end ?? k;
      k = group(s, k)?.end ?? k;
    }
    if (name === 'minipage' || name === 'wrapfigure') k = group(s, k)?.end ?? k;
    const { inner, end } = envEnd(s, name, k);
    const block = (text: string) => ({ text: `\n\n${text.trim()}\n\n`, end });

    if (name === 'document') return block(this.convert(inner));
    if (LIST_ENVS.has(name)) return block(this.list(name, inner, opt?.text ?? null));
    if (name === 'tabular' || name === 'tabular*' || name === 'tabularx' || name === 'array' || name === 'longtable') return block(this.table(inner));
    const theorem = THEOREMS[name.replace(/\*$/, '')];
    if (theorem) {
      const label = opt ? ` (${this.convert(opt.text).trim()})` : '';
      return block(`**${theorem}${label}.** ${this.convert(inner).trim()}`);
    }
    if (name === 'proof') {
      const text = this.convert(inner).trim();
      return block(`_Proof${opt ? ` (${this.convert(opt.text).trim()})` : ''}._ ${text}${text.endsWith('∎') ? '' : ' ∎'}`);
    }
    if (name === 'abstract') return block(`**Abstract.** ${this.convert(inner).trim()}`);
    if (name === 'quote' || name === 'quotation' || name === 'verse') {
      return block(this.convert(inner).trim().split('\n').map((l) => `> ${l}`).join('\n'));
    }
    if (name === 'thebibliography') return block(`## References\n\n${this.list('itemize', inner.replace(/^\s*\{[^}]*\}/, ''), null)}`);
    return block(this.convert(inner));
  }

  private list(name: string, inner: string, opt: string | null): string {
    const items = splitItems(inner);
    const label = enumLabel(opt);
    const lines = items.map((item, n) => {
      let body = item;
      let tag: string | null = null;
      const o = optional(body, 0);
      if (o && /^\s*\[/.test(body)) {
        tag = this.convert(o.text).trim();
        body = body.slice(o.end);
      }
      // Display math and code inside an item stay in the item.
      const text = this.restore(this.convert(body)).trim().replace(/\n{3,}/g, '\n\n');
      const lead = name === 'description'
        ? `- **${tag ?? ''}** `
        : tag
          ? `- ${tag} `
          : name === 'itemize' || name === 'compactitem'
            ? '- '
            : label
              ? `- ${label(n)} `
              : `${n + 1}. `;
      // Everything after the first line lines up under the item's text.
      const pad = /^\d/.test(lead) ? '   ' : '  ';
      const [first, ...rest] = text.split('\n');
      return `${lead}${first ?? ''}${rest.length ? `\n${rest.map((l) => (l.trim() ? `${pad}${l}` : '')).join('\n')}` : ''}`;
    });
    return lines.join('\n');
  }

  private table(inner: string): string {
    const rows = inner
      .split(/\\\\(?:\s*\[[^\]]*\])?/)
      .map((r) => r.replace(/\\(hline|toprule|midrule|bottomrule|cline\{[^}]*\})/g, '').trim())
      .filter(Boolean)
      .map((r) => splitCells(r).map((cell) => this.convert(cell).replace(/\s+/g, ' ').replace(/\|/g, '\\|').trim()));
    if (!rows.length) return '';
    const width = Math.max(...rows.map((r) => r.length));
    const line = (r: string[]) => `| ${Array.from({ length: width }, (_v, n) => r[n] ?? '').join(' | ')} |`;
    return [line(rows[0]), `|${' --- |'.repeat(width)}`, ...rows.slice(1).map(line)].join('\n');
  }
}

/** The body of a display math environment, as KaTeX takes it inside `$$`. */
function displayMath(name: string, inner: string, args: string): string {
  const body = inner.trim();
  const bare = name.replace(/\*$/, '');
  if (bare === 'equation' || bare === 'displaymath' || bare === 'math') return body;
  if (bare === 'eqnarray') return `\\begin{aligned}${body.replace(/&\s*([=<>]|\\[a-z]+)\s*&/g, '&$1')}\\end{aligned}`;
  if (bare === 'multline') return `\\begin{gathered}${body}\\end{gathered}`;
  if (bare === 'align' || bare === 'flalign') return `\\begin{aligned}${body}\\end{aligned}`;
  if (bare === 'gather') return `\\begin{gathered}${body}\\end{gathered}`;
  if (bare === 'alignat') return `\\begin{alignedat}${args.trim()}${body}\\end{alignedat}`;
  return body;
}

/** `\item`s of a list, at its own level only. */
function splitItems(inner: string): string[] {
  const items: string[] = [];
  let depth = 0;
  let start = -1;
  for (let i = 0; i < inner.length; i++) {
    const ch = inner[i];
    if (ch === '\\') {
      if (inner.startsWith('\\begin{', i)) depth++;
      else if (inner.startsWith('\\end{', i)) depth--;
      else if (depth === 0 && inner.startsWith('\\item', i) && !isLetter(inner[i + 5])) {
        if (start >= 0) items.push(inner.slice(start, i));
        start = i + 5;
        i += 4;
        continue;
      }
      i++;
      continue;
    }
    if (ch === '{') depth++;
    else if (ch === '}') depth--;
  }
  if (start >= 0) items.push(inner.slice(start));
  return items;
}

function splitCells(row: string): string[] {
  const cells: string[] = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < row.length; i++) {
    if (row[i] === '\\') {
      i++;
      continue;
    }
    if (row[i] === '{') depth++;
    else if (row[i] === '}') depth--;
    else if (row[i] === '&' && depth === 0) {
      cells.push(row.slice(start, i));
      start = i + 1;
    }
  }
  cells.push(row.slice(start));
  return cells;
}

const roman = (n: number) => {
  const map: [number, string][] = [[10, 'x'], [9, 'ix'], [5, 'v'], [4, 'iv'], [1, 'i']];
  let out = '';
  for (const [v, r] of map) while (n >= v) (out += r), (n -= v);
  return out;
};

/** The labels of an enumerate, from `[label=(\alph*)]` or the short `[(a)]` form. */
function enumLabel(opt: string | null): ((n: number) => string) | null {
  if (!opt) return null;
  const spec = /label\s*=\s*\{?([^,}]*)\}?/.exec(opt)?.[1] ?? opt;
  const counters: [RegExp, (n: number) => string][] = [
    [/\\alph\*?/, (n) => String.fromCharCode(97 + (n % 26))],
    [/\\Alph\*?/, (n) => String.fromCharCode(65 + (n % 26))],
    [/\\roman\*?/, (n) => roman(n + 1)],
    [/\\Roman\*?/, (n) => roman(n + 1).toUpperCase()],
    [/\\arabic\*?/, (n) => String(n + 1)],
  ];
  for (const [re, f] of counters) if (re.test(spec)) return (n) => spec.replace(re, f(n)).replace(/[{}]/g, '').trim();
  // enumerate package: a, A, i, I or 1 in the label stands for the counter.
  const short = /^([^aAiI1]*)([aAiI1])(.*)$/.exec(spec.trim());
  if (!short || spec.includes('=')) return null;
  const [, pre, kind, post] = short;
  const f = kind === 'a' ? (n: number) => String.fromCharCode(97 + n) : kind === 'A' ? (n: number) => String.fromCharCode(65 + n)
    : kind === 'i' ? (n: number) => roman(n + 1) : kind === 'I' ? (n: number) => roman(n + 1).toUpperCase() : (n: number) => String(n + 1);
  return (n) => `${pre}${f(n)}${post}`;
}

/**
 * The Markdown of a LaTeX document or fragment. With `colors`, `\textcolor`
 * and `\colorbox` keep their colors as HTML spans (for rendering); without,
 * only their text stays (for search and the tutor).
 */
export function latexToMarkdown(tex: string, options: { colors?: boolean } = {}): string {
  const macros = new Map<string, Macro>();
  const source = takeMacros(stripComments((tex ?? '').replace(/\r\n?/g, '\n')), macros);
  const begin = source.indexOf('\\begin{document}');
  const preamble = begin >= 0 ? source.slice(0, begin) : '';
  const end = source.lastIndexOf('\\end{document}');
  let body = begin >= 0 ? source.slice(begin + '\\begin{document}'.length, end > begin ? end : undefined) : source;

  const conv = new Converter(macros, options.colors ? takeColors(source) : null);
  // \title, \author and \date usually sit in the preamble; \maketitle prints them.
  if (/\\(title|author|date)(?![A-Za-z])/.test(preamble)) conv.convert(conv.protect(preamble));
  // After that pass, which would add the preamble's setup in its own order.
  conv.tikzPreamble = tikzPreamble(preamble);
  body = conv.protect(body);
  const text = conv.restore(conv.convert(body));

  let fenced = false;
  const markdown = text
    .split('\n')
    .map((line) => {
      if (/^\s*```/.test(line)) fenced = !fenced;
      return fenced ? line : line.replace(/[ \t]+$/g, '').replace(/(\S)[ \t]{2,}/g, '$1 ');
    })
    .join('\n')
    .replace(/\n{3,}/g, '\n\n')
    .trim();
  const defs = katexMacros(macros);
  return defs ? `<!-- latex-macros\n${defs}\n-->\n\n${markdown}` : markdown;
}

/** How a LaTeX document is laid out: its columns, and whether it prints its own head with `\\twocolumn[...]`. */
export function latexLayout(tex: string): { columns: 1 | 2; ownHead: boolean } {
  const source = stripComments(tex ?? '');
  return {
    columns: /\\documentclass\s*\[[^\]]*\btwocolumn\b|\\twocolumn(?![A-Za-z])/.test(source) ? 2 : 1,
    ownHead: /\\twocolumn\s*\[/.test(source),
  };
}

/** How a document's text shows an image added to it. */
export const addedImage = (key: string, format: string | null | undefined) =>
  format === 'tex' ? `\\includegraphics[width=\\linewidth]{answer-image:${key}}` : `![](answer-image:${key})`;

const ADDED_IMAGE = /(\\includegraphics\s*(?:\[[^\]]*\])?\s*\{)\s*answer-image:([A-Za-z0-9_-]+)\s*\}/g;

/** The images added in the app that a LaTeX source shows, each once. */
export function texImageKeys(tex: string): string[] {
  return [...new Set([...tex.matchAll(ADDED_IMAGE)].map((m) => m[2]))];
}

/** A source as TeX reads it outside the app: its added images at `files` (by key), with graphicx loaded. */
export function texWithImageFiles(tex: string, files: Record<string, string>): string {
  const source = tex.replace(ADDED_IMAGE, (whole, head: string, key: string) => (files[key] ? `${head}${files[key]}}` : whole));
  if (/\\usepackage\s*(?:\[[^\]]*\])?\s*\{[^}]*\bgraphicx\b/.test(stripComments(source))) return source;
  return source.replace(/^[ \t]*\\documentclass\s*(?:\[[^\]]*\])?\s*\{[^}]*\}.*$/m, (line) => `${line}\n\\usepackage{graphicx}`);
}

/** The KaTeX macro definitions a converted document carries, if any. */
export function markdownMacros(markdown: string): string | null {
  return /<!-- latex-macros\n([\s\S]*?)\n-->/.exec(markdown)?.[1] ?? null;
}

