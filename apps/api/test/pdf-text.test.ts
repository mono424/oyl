import assert from 'node:assert/strict';
import { test } from 'node:test';
import { pagesToMarkdown } from '../src/pdf.ts';

/** A minimal PDF with one line of Helvetica text per page. */
function pdfWithPages(lines: string[]): Uint8Array {
  const objects: string[] = [];
  const pageIds = lines.map((_, i) => 4 + i * 2);
  objects[1] = '<< /Type /Catalog /Pages 2 0 R >>';
  objects[2] = `<< /Type /Pages /Kids [${pageIds.map((id) => `${id} 0 R`).join(' ')}] /Count ${lines.length} >>`;
  objects[3] = '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>';
  lines.forEach((line, i) => {
    const stream = `BT /F1 18 Tf 72 720 Td (${line}) Tj ET`;
    objects[pageIds[i]] = `<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents ${pageIds[i] + 1} 0 R >>`;
    objects[pageIds[i] + 1] = `<< /Length ${stream.length} >>\nstream\n${stream}\nendstream`;
  });
  let out = '%PDF-1.4\n';
  const offsets: number[] = [];
  for (let id = 1; id < objects.length; id++) {
    offsets[id] = out.length;
    out += `${id} 0 obj\n${objects[id]}\nendobj\n`;
  }
  const xref = out.length;
  out += `xref\n0 ${objects.length}\n0000000000 65535 f \n`;
  for (let id = 1; id < objects.length; id++) out += `${String(offsets[id]).padStart(10, '0')} 00000 n \n`;
  out += `trailer\n<< /Size ${objects.length} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  return new TextEncoder().encode(out);
}

test('PDF pages become Markdown from the text layer, with page markers and their images', async () => {
  const pdf = pdfWithPages(['Eigenvalues of a matrix', 'Diagonal matrices', 'Eigenvectors']);
  const md = await pagesToMarkdown(pdf, 2, 3, [
    { id: 'a1', page: 2, width: 100, height: 80 },
    { id: 'a2', page: 1, width: 100, height: 80 },
  ]);
  assert.equal(md, '<!-- page 2 -->\n\nDiagonal matrices\n\n![](asset:a1)\n\n<!-- page 3 -->\n\nEigenvectors');
});
