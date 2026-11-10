import { type EditorState, type Extension, type Range, StateEffect, StateField } from '@codemirror/state';
import { Decoration, type DecorationSet, EditorView, showTooltip, type Tooltip, WidgetType } from '@codemirror/view';
import { ensureSyntaxTree, HighlightStyle, syntaxHighlighting, syntaxTree } from '@codemirror/language';
import { markdown, markdownLanguage } from '@codemirror/lang-markdown';
import type { BlockContext, Line, MarkdownConfig } from '@lezer/markdown';
import type { SyntaxNode } from '@lezer/common';
import { Tag, tags } from '@lezer/highlight';
import { imageSrc, renderMarkdown, renderTex } from './markdown';
import { tikzPlaceholder } from './tikz';

/**
 * Markdown that reads as rendered while you write it (CodeMirror): headings
 * at their size, emphasis, links, lists with bullets, quotes, code, images
 * tables, $math$ typeset by KaTeX and TikZ pictures drawn by TeX (./tikz.ts).
 * The markup of a construct shows only while the cursor is in it, so the
 * text stays plain Markdown underneath and prints the same through
 * `renderMarkdown`. While a formula or picture is being written its rendering
 * floats above it (inline) or sits below it (display, pictures).
 */

// ── Focus ─────────────────────────────────────────────────────────────────
//
// Without focus everything renders, wherever the cursor was left.

const setFocus = StateEffect.define<boolean>();

export const focusField = StateField.define<boolean>({
  create: () => false,
  update(value, tr) {
    for (const e of tr.effects) if (e.is(setFocus)) value = e.value;
    return value;
  },
  provide: () => EditorView.focusChangeEffect.of((_state, focusing) => setFocus.of(focusing)),
});

const focusChanged = (effects: readonly StateEffect<unknown>[]) => effects.some((e) => e.is(setFocus));

// ── Math syntax ───────────────────────────────────────────────────────────
//
// The same delimiters `renderMarkdown` reads: $…$ (not touching spaces or
// word characters, on one line), $$…$$ inline, and $$ blocks over several
// lines. An unclosed $$ block ends at the next blank line, so a formula
// being typed never swallows the rest of the document.

const mathTag = Tag.define();
const DOLLAR = 36;
const BACKSLASH = 92;

const isWord = (c: number) => (c >= 48 && c <= 57) || (c >= 65 && c <= 90) || (c >= 97 && c <= 122) || c === 95;
const isSpace = (c: number) => c === 32 || c === 9 || c === 10 || c === 13;

const math: MarkdownConfig = {
  defineNodes: [
    { name: 'InlineMath', style: mathTag },
    { name: 'BlockMath', block: true, style: mathTag },
    { name: 'MathMark', style: tags.processingInstruction },
  ],
  parseInline: [
    {
      name: 'InlineMath',
      before: 'Emphasis',
      parse(cx, next, pos) {
        if (next !== DOLLAR) return -1;
        if (cx.char(pos + 1) === DOLLAR) {
          const close = cx.slice(pos + 2, cx.end).indexOf('$$');
          if (close <= 0) return -1;
          const end = pos + 2 + close + 2;
          return cx.addElement(cx.elt('InlineMath', pos, end, [cx.elt('MathMark', pos, pos + 2), cx.elt('MathMark', end - 2, end)]));
        }
        const before = pos > cx.offset ? cx.char(pos - 1) : -1;
        const first = cx.char(pos + 1);
        if (before === BACKSLASH || isWord(before) || first < 0 || first === DOLLAR || isSpace(first)) return -1;
        for (let i = pos + 1; i < cx.end; i++) {
          const c = cx.char(i);
          if (c === BACKSLASH) {
            i++;
            continue;
          }
          if (c === 10) return -1;
          if (c !== DOLLAR) continue;
          const after = cx.char(i + 1);
          if (isSpace(cx.char(i - 1)) || isWord(after) || after === DOLLAR) return -1;
          return cx.addElement(cx.elt('InlineMath', pos, i + 1, [cx.elt('MathMark', pos, pos + 1), cx.elt('MathMark', i, i + 1)]));
        }
        return -1;
      },
    },
  ],
  parseBlock: [
    {
      name: 'BlockMath',
      before: 'FencedCode',
      parse(cx, line) {
        if (line.next !== DOLLAR || line.text.charCodeAt(line.pos + 1) !== DOLLAR) return false;
        const from = cx.lineStart + line.pos;
        const marks = [cx.elt('MathMark', from, from + 2)];
        let close = line.text.indexOf('$$', line.pos + 2);
        while (close < 0) {
          if (!cx.nextLine() || line.pos >= line.text.length) {
            cx.addElement(cx.elt('BlockMath', from, cx.prevLineEnd(), marks));
            return true;
          }
          close = line.text.indexOf('$$', line.pos);
        }
        marks.push(cx.elt('MathMark', cx.lineStart + close, cx.lineStart + close + 2));
        const end = cx.lineStart + line.text.length;
        cx.nextLine();
        cx.addElement(cx.elt('BlockMath', from, end, marks));
        return true;
      },
      endLeaf: (_cx, line) => line.next === DOLLAR && line.text.charCodeAt(line.pos + 1) === DOLLAR,
    },
  ],
};

// ── TikZ syntax ───────────────────────────────────────────────────────────
//
// What `renderMarkdown` draws as a picture besides ```tikz blocks: a
// tikzpicture or tikzcd from a line's start (in $$ or not) up to the line of
// its \end. Unclosed, it runs on like a code fence. Not in quotes.

const TIKZ_BEGIN = /^\\begin\{(tikzpicture|tikzcd)\}/;
const PICTURE = /\\begin\{(tikzpicture|tikzcd)\}[\s\S]*?\\end\{\1\}/;

/** The environment a line opens, with whether $$ is around it. */
function tikzOpen(cx: BlockContext, line: Line): { env: string; dollars: boolean } | null {
  if (!/^[ \t]*$/.test(line.text.slice(0, line.pos))) return null;
  const text = line.text.slice(line.pos);
  const bare = TIKZ_BEGIN.exec(text);
  if (bare) return { env: bare[1], dollars: false };
  if (!text.startsWith('$$')) return null;
  const rest = text.slice(2).trim();
  const wrapped = TIKZ_BEGIN.exec(rest || cx.peekLine().trim());
  return wrapped ? { env: wrapped[1], dollars: true } : null;
}

// The containers that go on at a line (Line.depth, left out of the typings).
const lineDepth = (line: Line) => (line as unknown as { depth: number }).depth;

const tikz: MarkdownConfig = {
  defineNodes: [{ name: 'TikzBlock', block: true, style: mathTag }],
  parseBlock: [
    {
      name: 'TikzBlock',
      before: 'BlockMath',
      parse(cx, line) {
        const open = tikzOpen(cx, line);
        if (!open) return false;
        const from = cx.lineStart + line.pos;
        const close = `\\end{${open.env}}`;
        for (let first = true; first || (cx.nextLine() && lineDepth(line) >= cx.depth); first = false) {
          const at = line.text.indexOf(close, first ? line.pos : 0);
          if (at < 0) continue;
          // Its closing $$, on the line or the next.
          if (open.dollars && !line.text.slice(at + close.length).trim() && cx.peekLine().trim() === '$$') cx.nextLine();
          const end = cx.lineStart + line.text.length;
          cx.nextLine();
          cx.addElement(cx.elt('TikzBlock', from, end));
          return true;
        }
        cx.addElement(cx.elt('TikzBlock', from, cx.prevLineEnd()));
        return true;
      },
      endLeaf: (cx, line) => !!tikzOpen(cx, line),
    },
  ],
};

const unindent = (text: string, indent: string) => text.split('\n').map((l) => (l.startsWith(indent) ? l.slice(indent.length) : l.trimStart())).join('\n');

/** The picture of a TikzBlock or ```tikz block, when it is closed, as `renderMarkdown` reads it. */
function picture(state: EditorState, node: SyntaxNode): string | null {
  const line = state.doc.lineAt(node.from);
  const indent = state.sliceDoc(line.from, node.from);
  if (!/^[ \t]*$/.test(indent)) return null;
  if (node.name === 'TikzBlock') {
    const m = PICTURE.exec(state.sliceDoc(node.from, node.to));
    return m ? unindent(m[0], indent) : null;
  }
  const info = node.getChild('CodeInfo');
  if (!info || state.sliceDoc(info.from, info.to) !== 'tikz' || node.getChildren('CodeMark').length < 2) return null;
  const first = line.to + 1;
  const last = state.doc.lineAt(node.to).from - 1;
  return first < last ? unindent(state.sliceDoc(first, last), indent) : null;
}

/** The formula of a math node, when it is closed. */
function formula(state: EditorState, node: SyntaxNode): string | null {
  const marks = node.getChildren('MathMark');
  if (marks.length < 2) return null;
  const tex = state.sliceDoc(marks[0].to, marks[marks.length - 1].from);
  return tex.trim() ? tex : null;
}

// ── Widgets ───────────────────────────────────────────────────────────────

class MathWidget extends WidgetType {
  constructor(
    readonly tex: string,
    readonly display: boolean,
    readonly preview = false,
  ) {
    super();
  }
  eq(other: MathWidget) {
    return other.tex === this.tex && other.display === this.display && other.preview === this.preview;
  }
  toDOM() {
    const el = document.createElement(this.display ? 'div' : 'span');
    el.className = this.display ? `cm-md-math-display${this.preview ? ' cm-md-math-preview' : ''}` : 'cm-md-math';
    el.innerHTML = renderTex(this.tex, this.display);
    return el;
  }
  // A click puts the cursor in the formula, which then shows its source.
  ignoreEvent() {
    return false;
  }
}

class ImageWidget extends WidgetType {
  constructor(
    readonly src: string,
    readonly alt: string,
  ) {
    super();
  }
  eq(other: ImageWidget) {
    return other.src === this.src && other.alt === this.alt;
  }
  toDOM(view: EditorView) {
    const wrap = document.createElement('span');
    wrap.className = 'cm-md-image';
    const img = document.createElement('img');
    img.src = imageSrc(this.src);
    img.alt = this.alt;
    img.draggable = false;
    // Its height is known once loaded.
    img.addEventListener('load', () => view.requestMeasure());
    wrap.append(img);
    return wrap;
  }
  ignoreEvent() {
    return false;
  }
}

class TableWidget extends WidgetType {
  constructor(readonly source: string) {
    super();
  }
  eq(other: TableWidget) {
    return other.source === this.source;
  }
  toDOM() {
    const el = document.createElement('div');
    el.className = 'cm-md-table-view';
    el.innerHTML = renderMarkdown(this.source);
    return el;
  }
  // A click puts the cursor in the table, which then shows its source.
  ignoreEvent() {
    return false;
  }
}

const resizing = new WeakMap<HTMLElement, ResizeObserver>();

class TikzWidget extends WidgetType {
  constructor(
    readonly source: string,
    readonly preview = false,
  ) {
    super();
  }
  eq(other: TikzWidget) {
    return other.source === this.source && other.preview === this.preview;
  }
  toDOM(view: EditorView) {
    const el = document.createElement('div');
    el.className = `cm-md-tikz${this.preview ? ' cm-md-tikz-preview' : ''}`;
    el.innerHTML = tikzPlaceholder(this.source);
    // TeX draws it later, changing its height.
    const observer = new ResizeObserver(() => view.requestMeasure());
    observer.observe(el);
    resizing.set(el, observer);
    return el;
  }
  destroy(dom: HTMLElement) {
    resizing.get(dom)?.disconnect();
  }
  // A click puts the cursor in the picture, which then shows its source.
  ignoreEvent() {
    return false;
  }
}

class BulletWidget extends WidgetType {
  eq() {
    return true;
  }
  toDOM() {
    const el = document.createElement('span');
    el.className = 'cm-md-bullet';
    el.textContent = '•';
    return el;
  }
}

class RuleWidget extends WidgetType {
  eq() {
    return true;
  }
  toDOM() {
    const el = document.createElement('span');
    el.className = 'cm-md-hr';
    return el;
  }
}

const hidden = Decoration.replace({});
const bullet = Decoration.replace({ widget: new BulletWidget() });
const rule = Decoration.replace({ widget: new RuleWidget() });

// ── Live preview ──────────────────────────────────────────────────────────

const HEADING = /^(?:ATX|Setext)Heading(\d)$/;

function preview(state: EditorState): DecorationSet {
  const focused = state.field(focusField);
  const doc = state.doc;
  const selection = state.selection.ranges;
  const touches = (from: number, to: number) => focused && selection.some((r) => r.from <= to && r.to >= from);
  const lineTouched = (from: number, to: number) => touches(doc.lineAt(from).from, doc.lineAt(to).to);
  // A mark and the space after it.
  const withSpace = (to: number) => (doc.sliceString(to, to + 1) === ' ' ? to + 1 : to);

  const out: Range<Decoration>[] = [];
  const lines = (from: number, to: number, cls: string) => {
    for (let pos = from; pos <= to; ) {
      const line = doc.lineAt(pos);
      out.push(Decoration.line({ class: cls }).range(line.from));
      pos = line.to + 1;
    }
  };

  const tree = ensureSyntaxTree(state, doc.length, 50) ?? syntaxTree(state);
  tree.iterate({
    enter(ref) {
      const { name, from, to } = ref;
      const node = ref.node;
      const parent = node.parent;
      const heading = HEADING.exec(name);
      if (heading) {
        lines(from, from, `cm-md-h${heading[1]}`);
        return;
      }
      switch (name) {
        case 'HeaderMark':
          if (parent && parent.name.startsWith('ATX') && !lineTouched(parent.from, parent.to)) out.push(hidden.range(from, withSpace(to)));
          return;
        case 'EmphasisMark':
        case 'StrikethroughMark':
          if (parent && !touches(parent.from, parent.to)) out.push(hidden.range(from, to));
          return;
        case 'InlineCode':
          out.push(Decoration.mark({ class: 'cm-md-code' }).range(from, to));
          if (!touches(from, to)) for (const mark of node.getChildren('CodeMark')) out.push(hidden.range(mark.from, mark.to));
          return false;
        case 'FencedCode':
        case 'TikzBlock': {
          const source = picture(state, node);
          if (!source && name === 'FencedCode') {
            lines(from, to, 'cm-md-codeblock');
            return false;
          }
          const start = doc.lineAt(from).from;
          const end = doc.lineAt(to).to;
          if (source && !touches(start, end)) {
            out.push(Decoration.replace({ widget: new TikzWidget(source), block: true }).range(start, end));
          } else {
            lines(from, to, name === 'FencedCode' ? 'cm-md-codeblock' : 'cm-md-math-block');
            if (source) out.push(Decoration.widget({ widget: new TikzWidget(source, true), block: true, side: 1 }).range(end));
          }
          return false;
        }
        case 'CodeBlock':
          lines(from, to, 'cm-md-codeblock');
          return false;
        case 'Link': {
          if (touches(from, to)) return;
          const marks = node.getChildren('LinkMark');
          // [text](url "title"): everything but the text.
          if (marks.length >= 2) {
            out.push(hidden.range(marks[0].from, marks[0].to));
            out.push(hidden.range(marks[1].from, to));
          }
          out.push(Decoration.mark({ class: 'cm-md-link' }).range(marks[0]?.to ?? from, marks[1]?.from ?? to));
          // Into the text, for emphasis in it: [*title*](url).
          return;
        }
        case 'Image': {
          const url = node.getChild('URL');
          const src = url ? state.sliceDoc(url.from, url.to) : '';
          if (touches(from, to) || !imageSrc(src)) return false;
          const marks = node.getChildren('LinkMark');
          const alt = marks.length >= 2 ? state.sliceDoc(marks[0].to, marks[1].from) : '';
          out.push(Decoration.replace({ widget: new ImageWidget(src, alt) }).range(from, to));
          return false;
        }
        case 'InlineMath': {
          const tex = formula(state, node);
          if (tex && !touches(from, to)) {
            const display = state.sliceDoc(from, from + 2) === '$$';
            out.push(Decoration.replace({ widget: new MathWidget(tex, display) }).range(from, to));
          } else {
            out.push(Decoration.mark({ class: 'cm-md-math-src' }).range(from, to));
          }
          return false;
        }
        case 'BlockMath': {
          const tex = formula(state, node);
          const start = doc.lineAt(from).from;
          const end = doc.lineAt(to).to;
          if (tex && !touches(start, end)) {
            out.push(Decoration.replace({ widget: new MathWidget(tex, true), block: true }).range(start, end));
          } else {
            lines(from, to, 'cm-md-math-block');
            if (tex) out.push(Decoration.widget({ widget: new MathWidget(tex, true, true), block: true, side: 1 }).range(end));
          }
          return false;
        }
        case 'ListMark':
          if (parent?.parent?.name === 'BulletList' && !lineTouched(from, to)) out.push(bullet.range(from, to));
          else out.push(Decoration.mark({ class: 'cm-md-listmark' }).range(from, to));
          return;
        case 'Blockquote':
          lines(from, to, 'cm-md-quote');
          return;
        case 'QuoteMark':
          if (!lineTouched(from, to)) out.push(hidden.range(from, withSpace(to)));
          return;
        case 'HorizontalRule':
          if (!lineTouched(from, to)) out.push(rule.range(from, to));
          return false;
        case 'Table': {
          const start = doc.lineAt(from).from;
          const end = doc.lineAt(to).to;
          if (touches(start, end)) {
            lines(from, to, 'cm-md-table');
            return;
          }
          out.push(Decoration.replace({ widget: new TableWidget(doc.sliceString(start, end)), block: true }).range(start, end));
          return false;
        }
      }
    },
  });
  return Decoration.set(out, true);
}

const previewField = StateField.define<DecorationSet>({
  create: preview,
  update(deco, tr) {
    if (tr.docChanged || tr.selection || focusChanged(tr.effects) || syntaxTree(tr.state) !== syntaxTree(tr.startState)) return preview(tr.state);
    return deco;
  },
  provide: (f) => EditorView.decorations.from(f),
});

// ── The formula being written, rendered above it ──────────────────────────

function mathAtCursor(state: EditorState): { from: number; tex: string; display: boolean } | null {
  if (!state.field(focusField) || !state.selection.main.empty) return null;
  const head = state.selection.main.head;
  for (const side of [-1, 1] as const) {
    for (let node: SyntaxNode | null = syntaxTree(state).resolveInner(head, side); node; node = node.parent) {
      if (node.name !== 'InlineMath') continue;
      const tex = formula(state, node);
      return tex ? { from: node.from, tex, display: state.sliceDoc(node.from, node.from + 2) === '$$' } : null;
    }
  }
  return null;
}

const mathTooltip = StateField.define<Tooltip | null>({
  create: () => null,
  update(tip, tr) {
    if (!tr.docChanged && !tr.selection && !focusChanged(tr.effects)) return tip;
    const at = mathAtCursor(tr.state);
    if (!at) return null;
    return {
      pos: at.from,
      above: true,
      create: () => {
        const dom = document.createElement('div');
        dom.className = 'cm-md-math-tip';
        dom.innerHTML = renderTex(at.tex, at.display);
        return { dom };
      },
    };
  },
  provide: (f) => showTooltip.from(f),
});

// ── Highlighting ──────────────────────────────────────────────────────────

const highlight = HighlightStyle.define([
  { tag: tags.heading, fontWeight: '600' },
  { tag: tags.emphasis, fontStyle: 'italic' },
  { tag: tags.strong, fontWeight: '600' },
  { tag: tags.strikethrough, textDecoration: 'line-through', color: 'var(--muted)' },
  { tag: tags.link, color: 'var(--blue)' },
  { tag: tags.url, color: 'var(--muted)' },
  { tag: [tags.processingInstruction, tags.contentSeparator], color: 'var(--faint)' },
  { tag: tags.quote, color: 'var(--muted)' },
  { tag: mathTag, color: 'color-mix(in srgb, var(--blue) 80%, var(--fg))', fontFamily: 'var(--font-mono)', fontSize: '0.9em' },
]);

/** Markdown with math, previewed in place. `syntax` extends the parser (a document's citations). */
export function liveMarkdown(syntax: MarkdownConfig[] = []): Extension {
  return [
    markdown({ base: markdownLanguage, extensions: [math, tikz, ...syntax] }),
    syntaxHighlighting(highlight),
    focusField,
    previewField,
    mathTooltip,
  ];
}
