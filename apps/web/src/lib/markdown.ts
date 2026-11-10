import { Marked } from 'marked';
import DOMPurify from 'dompurify';
import katex from 'katex';
import { MARKDOWN_MATH as MATH } from '../../../../shared/math';
import 'katex/dist/katex.min.css';
import { markdownMacros } from '../../../../shared/latex';
import { answerImageUrl, assetUrl } from './api';
import { tikzPlaceholder } from './tikz';

const marked = new Marked({ gfm: true, breaks: false });

// Images extracted from a PDF are referenced as `asset:<id>` in the material's
// markdown, so the text stays independent of where the API is hosted. They
// resolve to the API's asset route at render time.
const ASSET_REF = /^asset:([A-Za-z0-9_-]+)$/;
// Images pasted into a homework answer (apps/api/src/answers.ts).
const ANSWER_IMAGE_REF = /^answer-image:([A-Za-z0-9_-]+)$/;

DOMPurify.addHook('afterSanitizeAttributes', (node) => {
  if (node.tagName === 'IMG') {
    const src = node.getAttribute('src') ?? '';
    const m = ASSET_REF.exec(src);
    if (m) node.setAttribute('src', assetUrl(m[1]));
    const a = ANSWER_IMAGE_REF.exec(src);
    if (a) node.setAttribute('src', answerImageUrl(a[1]));
    node.setAttribute('loading', 'lazy');
  }
  if (node.tagName === 'A') {
    node.setAttribute('target', '_blank');
    node.setAttribute('rel', 'noopener noreferrer');
  }
});

// ── Math ──────────────────────────────────────────────────────────────────
//
// Math ($…$, $$…$$, \(…\), \[…\]) is taken out before Markdown sees it (which
// would turn `_` and `\\` into formatting) and rendered by KaTeX into the
// sanitized HTML. KaTeX escapes what it renders and runs without `trust`, so
// its output needs no sanitizing. Code spans and blocks keep their dollars.

const CODE = /(^|\n)(```|~~~)[\s\S]*?(\n\2[^\n]*(?=\n|$)|$)|`+[^`\n]+?`+/g;
const PLACEHOLDER = /(<p>)?KATEXMATH(\d+)Z(<\/p>)?/g;

interface Formula {
  tex: string;
  display: boolean;
}

function extractMath(md: string): { text: string; math: Formula[] } {
  const math: Formula[] = [];
  const take = (part: string) =>
    part.replace(MATH, (...args: unknown[]) => {
      const [, dd, br, pa, d, offset, whole] = args as [string, string | undefined, string | undefined, string | undefined, string | undefined, number, string];
      const display = dd !== undefined || br !== undefined;
      math.push({ tex: (dd ?? br ?? pa ?? d ?? '').trim(), display });
      const ph = `KATEXMATH${math.length - 1}Z`;
      if (!display) return ph;
      // Display math is a paragraph of its own, indented like its line (a list item's).
      const before = whole.slice(whole.lastIndexOf('\n', offset - 1) + 1, offset);
      const indent = /^[ \t]*$/.test(before) ? before : '';
      return `\n\n${indent}${ph}\n\n${indent}`;
    });
  let text = '';
  let at = 0;
  for (const m of md.matchAll(CODE)) {
    text += take(md.slice(at, m.index)) + m[0];
    at = m.index + m[0].length;
  }
  text += take(md.slice(at));
  return { text, math };
}

function renderMath(f: Formula, macros: Record<string, string>): string {
  try {
    return katex.renderToString(f.tex, { displayMode: f.display, throwOnError: false, strict: 'ignore', trust: false, macros, maxExpand: 1000 });
  } catch {
    // A formula KaTeX cannot read is shown as written.
    const code = document.createElement('code');
    code.textContent = f.display ? `$$${f.tex}$$` : `$${f.tex}$`;
    return code.outerHTML;
  }
}

/** One formula as HTML (the homework editor's live preview). */
export function renderTex(tex: string, display: boolean): string {
  return renderMath({ tex: tex.trim(), display }, {});
}

/** Where an image of the markdown loads from (`asset:` and `answer-image:` refs resolved). */
export function imageSrc(src: string): string {
  const asset = ASSET_REF.exec(src);
  if (asset) return assetUrl(asset[1]);
  const answer = ANSWER_IMAGE_REF.exec(src);
  if (answer) return answerImageUrl(answer[1]);
  return /^https?:/i.test(src) ? src : '';
}

// ── TikZ ──────────────────────────────────────────────────────────────────
//
// ```tikz blocks (a LaTeX document's pictures, shared/latex.ts) are taken out
// before math, and come back as pictures TeX draws in the browser (./tikz.ts).
// A block in a list item is indented like the item. A tikzpicture or tikzcd
// written as it is, from a line's start to a line's end, or as display math
// ($$ around it), is such a block too.

const TIKZ = /(^|\n)([ \t]*)(`{3,}|~{3,})tikz[ \t]*\n([\s\S]*?)\n[ \t]*\3[ \t]*(?=\n|$)/g;
const TIKZ_PLACEHOLDER = /(<p>)?TIKZPICTURE(\d+)Z(<\/p>)?/g;
const BARE_TIKZ =
  /(^|\n)([ \t]*)(?:\$\$[ \t]*(?:\n[ \t]*)?(\\begin\{(tikzpicture|tikzcd)\}[\s\S]*?\\end\{\4\})[ \t]*(?:\n[ \t]*)?\$\$|(\\begin\{(tikzpicture|tikzcd)\}[\s\S]*?\\end\{\6\}))[ \t]*(?=\n|$)/g;

/** Pictures written as LaTeX become ```tikz blocks; the ones in code stay code. */
function fenceTikz(md: string): string {
  if (!md.includes('\\begin{tikz')) return md;
  const code = [...md.matchAll(CODE)].map((m) => [m.index, m.index + m[0].length]);
  return md.replace(BARE_TIKZ, (m, lead: string, indent: string, wrapped: string | undefined, _w: string, bare: string | undefined, _b: string, offset: number) => {
    const from = offset + lead.length;
    const to = offset + m.length;
    if (code.some(([start, end]) => start < to && end > from)) return m;
    return `${lead}${indent}\`\`\`tikz\n${indent}${wrapped ?? bare}\n${indent}\`\`\``;
  });
}

function extractTikz(md: string): { text: string; pictures: string[] } {
  const pictures: string[] = [];
  const text = md.replace(TIKZ, (_m, lead: string, indent: string, _fence: string, body: string) => {
    pictures.push(body.split('\n').map((l) => (l.startsWith(indent) ? l.slice(indent.length) : l.trimStart())).join('\n'));
    return `${lead}\n\n${indent}TIKZPICTURE${pictures.length - 1}Z\n\n`;
  });
  return { text, pictures };
}

/** Lecture markdown (with its math) to sanitized HTML. */
export function renderMarkdown(md: string): string {
  const tikz = extractTikz(fenceTikz(md ?? ''));
  const html = withMath(md ?? '', tikz.text);
  if (!tikz.pictures.length) return html;
  return html.replace(TIKZ_PLACEHOLDER, (m, open: string | undefined, n: string, close: string | undefined) => {
    const source = tikz.pictures[Number(n)];
    if (source === undefined) return m;
    return open && close ? tikzPlaceholder(source) : `${open ?? ''}${tikzPlaceholder(source)}${close ?? ''}`;
  });
}

function withMath(md: string, source: string): string {
  const { text, math } = extractMath(source);
  const html = DOMPurify.sanitize(marked.parse(text, { async: false }) as string, {
    ADD_URI_SAFE_ATTR: ['src'],
    ALLOWED_URI_REGEXP: /^(?:(?:https?|mailto|asset|answer-image):|[^a-z]|[a-z+.-]+(?:[^a-z+.\-:]|$))/i,
  });
  if (!math.length) return html;
  // A converted LaTeX document carries its \newcommands (shared/latex.ts);
  // KaTeX keeps what they define in `macros` for every formula after them.
  const macros: Record<string, string> = {};
  const defs = markdownMacros(md);
  if (defs) {
    try {
      katex.renderToString(defs, { globalGroup: true, throwOnError: false, strict: 'ignore', macros });
    } catch {
      // Definitions KaTeX rejects are left out.
    }
  }
  return html.replace(PLACEHOLDER, (m, open: string | undefined, n: string, close: string | undefined) => {
    const f = math[Number(n)];
    if (!f) return m;
    const out = renderMath(f, macros);
    if (f.display) return `<div class="math-display">${out}</div>`;
    return `${open ?? ''}${out}${close ?? ''}`;
  });
}
