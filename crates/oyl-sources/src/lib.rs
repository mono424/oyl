//! Oyl sources: the paper's bibliography, its citations, the PDFs of the
//! sources, and its figures. No UI.
//!
//! - [`bib`]: `refs.bib` as [`Source`]s — [`Bibliography`] reads any
//!   real-world `.bib` file and saves it back changed as little as possible;
//!   cite keys follow the `lastname + year` rule ([`cite_key`]).
//! - [`lookup`]: one input box for anything — DOI, arXiv id, link, BibTeX,
//!   or a title — resolved through Crossref, arXiv and the page's own meta
//!   tags. Network calls block; run them on a background thread.
//! - [`citations`]: citations in LaTeX and Typst text, usage counts,
//!   unknown keys, and the snippets to insert.
//! - [`pdfs`]: a source's PDF in `sources/<key>.pdf`, linked from
//!   `.oyl/sources.json`; a dropped PDF identified by its DOI or arXiv id.
//! - [`figures`]: images in `figures/` — import, list, insert.
//!
//! Every function takes plain paths: the paper folder root, or the `.bib`.

pub mod bib;
pub mod citations;
mod error;
pub mod figures;
mod fsutil;
pub mod latex;
pub mod lookup;
pub mod pdfs;
mod pdftext;

pub use bib::{BibProblem, Bibliography, Source, cite_key, parse_bibtex};
pub use citations::{Citation, CiteForm};
pub use error::{Error, Result};
pub use figures::Figure;
pub use lookup::Query;
pub use pdfs::{PdfIdentity, PdfLink};
