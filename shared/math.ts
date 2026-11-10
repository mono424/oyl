/**
 * How rendered Markdown finds math. An inline `$x$` must not touch a letter or
 * digit, so prices in Markdown ("$5 and $6") stay text.
 */
export const MARKDOWN_MATH = /\$\$([\s\S]+?)\$\$|\\\[([\s\S]+?)\\\]|\\\(([\s\S]+?)\\\)|(?<![\\$\w])\$(?![\s$])((?:\\.|[^$\\\n])+?)(?<![\s\\])\$(?![\w$])/g;

/**
 * Inline math for Markdown, from math LaTeX already delimits. In LaTeX a dollar
 * is always math, so `0.5$^\circ$` and `$\mu$m` are valid, but in Markdown those
 * dollars touch a digit or letter and would stay text: they go out as `\(x\)`,
 * which has no such rule. A line break would end the formula too.
 */
export function inlineMarkdownMath(math: string, before = '', after = ''): string {
  const tex = math.trim().replace(/\s*\n\s*/g, ' ');
  return /[\w$\\]$/.test(before) || /^[\w$]/.test(after) || tex.endsWith('\\') ? `\\(${tex}\\)` : `$${tex}$`;
}
