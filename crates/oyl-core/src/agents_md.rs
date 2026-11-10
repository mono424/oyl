//! `AGENTS.md`: how an agent should work on a paper. Written into every
//! paper so that any harness — Sorcar, Claude Code, Codex, or one Oyl does
//! not know — follows the same rules. `CLAUDE.md` only points at it.

use oyl_model::Engine;

use crate::project::{CiteStyle, FIGURES_DIR, Manifest, SOURCES_DIR, STATE_DIR};

/// The text of `CLAUDE.md`: Claude Code reads it and imports `AGENTS.md`.
pub const CLAUDE_MD: &str =
    "# CLAUDE.md\n\nThe instructions for working on this paper are in AGENTS.md.\n\n@AGENTS.md\n";

/// The text of `AGENTS.md` for a paper: what the folder is, its layout,
/// and the writing rules.
pub fn agents_md(manifest: &Manifest) -> String {
    let engine = manifest.engine;
    let main = manifest.main.display();
    let bib = manifest.bibliography.display();
    let format = engine.label();
    let compiled_by = match engine {
        Engine::Typst => "Oyl compiles it with Typst 0.15, in-process, on every change.",
        Engine::Latex => {
            "Oyl compiles it with tectonic, latexmk or pdflatex, with BibTeX for the \
             bibliography, on every save."
        }
    };
    format!(
        "# {name}\n\
         \n\
         This folder is a paper written in {format} with Oyl. It is plain files: \
         edit them in place. {compiled_by}\n\
         \n\
         ## Layout\n\
         \n\
         - `{main}`: the entry file. Compiling it produces the paper.\n\
         - `{bib}`: the bibliography, in BibTeX. It is the single source of truth for \
         citations.\n\
         - `{FIGURES_DIR}/`: images and plots used by the paper.\n\
         - `{SOURCES_DIR}/`: PDFs of the cited works.\n\
         - `oyl.toml`: the paper's name, engine, main file, bibliography and citation style.\n\
         - `{STATE_DIR}/`: Oyl's own state (history, chats, build output).\n\
         \n\
         ## Rules\n\
         \n\
         {rules}",
        name = manifest.name,
        rules = writing_rules(manifest),
    )
}

/// The writing rules every agent turn follows, as a Markdown list. Part of
/// `AGENTS.md`, and suitable for a harness's system prompt.
pub fn writing_rules(manifest: &Manifest) -> String {
    let bib = manifest.bibliography.display();
    let main = manifest.main.display();
    let (stay, cite, figure, compiling) = match manifest.engine {
        Engine::Typst => (
            "Write Typst. Do not convert the paper to LaTeX or Markdown, and keep the \
             existing set and show rules unless asked to change the look.",
            "In Typst, cite with `@key` (or `#cite(<key>)`).",
            "Place a figure with `#figure(image(\"figures/name.png\"), caption: [...]) <fig:name>` \
             and refer to it with `@fig:name`.",
            "brackets and braces balanced, every `#include`d file present, and no packages \
             beyond what the paper already imports.",
        ),
        Engine::Latex => (
            "Write LaTeX. Do not convert the paper to Typst or Markdown, and do not switch \
             the document class or the bibliography setup (natbib with BibTeX; no biblatex, \
             no biber).",
            match manifest.cite_style {
                CiteStyle::AuthorYear => {
                    "In LaTeX, cite with `\\citep{key}` (parenthetical) or `\\citet{key}` \
                     (textual), from natbib."
                }
                CiteStyle::Numeric => {
                    "In LaTeX, cite with `\\cite{key}` or `\\citep{key}`, from natbib."
                }
            },
            "Place a figure in a `figure` environment with `\\includegraphics`, a `\\caption` \
             and a `\\label{fig:name}`, and refer to it with `\\ref{fig:name}`.",
            "every `\\begin` closed, braces balanced, every `\\input` file present, and only \
             packages from a standard TeX distribution (no minted, no shell escape).",
        ),
    };
    format!(
        "1. Stay in the paper's format. {stay}\n\
         2. Cite only keys that exist in `{bib}`. {cite} Check the key in `{bib}` before \
         using it.\n\
         3. Never invent references. To add a source, look it up and add its real BibTeX \
         entry to `{bib}`:\n\
         \x20  - by DOI: `curl -sL -H \"Accept: application/x-bibtex\" https://doi.org/<DOI>`\n\
         \x20  - by search (Crossref): `curl -s \"https://api.crossref.org/works?query.bibliographic=<title+and+authors>&rows=5\"`, \
         then fetch the DOI of the match as above\n\
         \x20  - by arXiv id: `curl -s https://arxiv.org/bibtex/<id>`, or search with \
         `curl -s \"https://export.arxiv.org/api/query?search_query=all:<words>&max_results=5\"`\n\
         \x20  Name new keys after the first author's last name and the year (`knuth1984`, then \
         `knuth1984b`). If a source cannot be found, say so instead of citing it.\n\
         4. Figures go in `{FIGURES_DIR}/`. {figure}\n\
         5. Keep the paper compiling from `{main}`: {compiling} After a change, re-read what \
         you wrote for syntax errors; Oyl reports compile errors back to you.\n\
         6. Edit files in place, with focused changes. Do not rename, move or delete files \
         unless asked, and do not make copies such as `main-v2`: Oyl keeps every version in \
         its history.\n\
         7. Do not touch `{STATE_DIR}/`.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_md_names_the_files_and_rules() {
        for engine in Engine::ALL {
            let manifest = Manifest::new("On Writing", engine, engine.default_main());
            let text = agents_md(&manifest);
            assert!(text.starts_with("# On Writing\n"));
            assert!(text.contains(engine.label()));
            assert!(text.contains(&format!("`{}`", engine.default_main())));
            assert!(text.contains("`refs.bib`"));
            assert!(text.contains("figures/"));
            assert!(text.contains("Never invent references"));
            assert!(text.contains("https://doi.org/"));
            assert!(text.contains("api.crossref.org"));
            assert!(text.contains("arxiv.org"));
            assert!(text.contains("Do not touch `.oyl/`"));
        }
        let latex = agents_md(&Manifest::new("X", Engine::Latex, "main.tex"));
        assert!(latex.contains("\\citep{key}"));
        assert!(!latex.contains("#cite"));
        let typst = agents_md(&Manifest::new("X", Engine::Typst, "main.typ"));
        assert!(typst.contains("@key"));
        assert!(!typst.contains("\\citep"));
    }

    #[test]
    fn claude_md_imports_agents_md() {
        assert!(CLAUDE_MD.contains("@AGENTS.md"));
    }
}
