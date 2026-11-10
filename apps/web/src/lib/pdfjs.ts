// pdf.js is large: it loads the first time a PDF is opened.
let pdfjs: Promise<typeof import('pdfjs-dist')> | null = null;

export const loadPdfjs = () =>
  (pdfjs ??= Promise.all([import('pdfjs-dist'), import('pdfjs-dist/build/pdf.worker.min.mjs?url')]).then(([lib, worker]) => {
    lib.GlobalWorkerOptions.workerSrc = worker.default;
    return lib;
  }));
