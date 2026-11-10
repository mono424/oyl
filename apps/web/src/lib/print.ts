/**
 * Export PDF for homework and documents: the page's `.homework-print` view
 * (global.css, @media print), printed by the browser.
 */

import { tikzSettled } from './tikz';

/** Prints once the print typeface is loaded and TikZ pictures are drawn, named after `title` (the PDF's file name). */
export async function exportPdf(title: string) {
  await Promise.all([
    tikzSettled(),
    ...['400 11pt KaTeX_Main', 'italic 400 11pt KaTeX_Main', '700 11pt KaTeX_Main', 'italic 700 11pt KaTeX_Main', '400 11pt KaTeX_Typewriter']
      .map((font) => document.fonts.load(font).catch(() => [])),
  ]);
  const before = document.title;
  document.title = title;
  addEventListener('afterprint', () => (document.title = before), { once: true });
  window.print();
}
