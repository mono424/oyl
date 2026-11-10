//! LaTeX, end to end, with whatever toolchain this machine has. Every test
//! is skipped (and says so) when none is found.

mod common;

use std::fs;
use std::time::Instant;

use common::{
    Paper, assert_no_errors, data_dir, expected_size, has_ink, is_opaque_white_paper, same_file,
};
use oyl_model::{Engine, SourceLocation};
use oyl_typeset::{Compiler, PagePosition, ToolchainStatus, rasterize_pdf};

/// A compiler whose LaTeX toolchain is ready, or `None` (test skipped).
fn latex_compiler(data: &tempfile::TempDir) -> Option<Compiler> {
    let compiler = Compiler::new(data.path().to_path_buf());
    match compiler.toolchain(Engine::Latex) {
        ToolchainStatus::Ready { detail } => {
            println!("LaTeX toolchain: {detail}");
            Some(compiler)
        }
        other => {
            eprintln!("skipped: no LaTeX toolchain ({other:?})");
            None
        }
    }
}

const ARTICLE: &str = r"\documentclass{article}
\begin{document}
\section{Introduction}
Typesetting is an art, as \cite{knuth84} explains at length.

A second paragraph, on line six of the source file.

\bibliographystyle{plain}
\bibliography{refs}
\end{document}
";

const REFS: &str = "@book{knuth84,
  author = {Donald E. Knuth},
  title = {The {\\TeX}book},
  publisher = {Addison-Wesley},
  year = {1984}
}
";

#[test]
fn article_with_citation_builds_into_the_hidden_folder() {
    let data = data_dir();
    let Some(mut compiler) = latex_compiler(&data) else {
        return;
    };
    let paper = Paper::new();
    let main = paper.write("main.tex", ARTICLE);
    paper.write("refs.bib", REFS);

    let start = Instant::now();
    let output = compiler.compile(&paper.request(Engine::Latex, "main.tex"));
    println!("TIMING latex cold compile: {:?}", start.elapsed());
    assert_no_errors(&output);
    assert!(
        !output
            .diagnostics
            .iter()
            .any(|d| d.message.contains("undefined")),
        "the citation resolved: {:#?}",
        output.diagnostics
    );
    let document = output.document.expect("a PDF");
    assert_eq!(document.page_count(), 1);

    // The paper folder holds only what the writer put there.
    assert_eq!(paper.listing(), [".oyl", "main.tex", "refs.bib"]);
    let build = paper.path(".oyl/build");
    assert!(build.join("main.pdf").is_file());
    assert!(build.join("main.synctex.gz").is_file());

    // A4 or US letter, whichever this TeX installation defaults to.
    let size = document.page_size_pt(0);
    let a4 = (size.0 - 595.276).abs() < 0.5 && (size.1 - 841.89).abs() < 0.5;
    assert!(a4 || size == (612.0, 792.0), "{size:?}");
    for scale in [1.0, 2.0] {
        let start = Instant::now();
        let image = document.render_page(0, scale).expect("render");
        println!("TIMING latex page render at {scale}: {:?}", start.elapsed());
        assert_eq!((image.width, image.height), expected_size(size, scale));
        assert!(is_opaque_white_paper(&image) && has_ink(&image));
    }
    assert!(document.pdf().expect("pdf").starts_with(b"%PDF"));

    // SyncTeX: line 6 → page position → line 6.
    let at = document
        .position_of(&SourceLocation::new(&main, 6, 1))
        .expect("line 6 is on the page");
    assert_eq!(at.page, 0);
    let back = document
        .source_at(PagePosition {
            page: 0,
            x_pt: at.x_pt + 30.0,
            y_pt: at.y_pt + 4.0,
        })
        .expect("a source line");
    assert!(same_file(&back.file, &main), "{back:?}");
    assert_eq!(back.line, 6);
    let intro = document
        .position_of(&SourceLocation::new(&main, 4, 1))
        .expect("line 4");
    assert!(intro.y_pt < at.y_pt);

    // Recompiling unchanged sources keeps the page fingerprint; an edit
    // changes it.
    let start = Instant::now();
    let again = compiler.compile(&paper.request(Engine::Latex, "main.tex"));
    println!("TIMING latex recompile (unchanged): {:?}", start.elapsed());
    let again = again.document.expect("a PDF");
    assert_eq!(again.page_fingerprint(0), document.page_fingerprint(0));
    paper.write("main.tex", &ARTICLE.replace("an art", "a craft"));
    let start = Instant::now();
    let edited = compiler.compile(&paper.request(Engine::Latex, "main.tex"));
    println!("TIMING latex recompile (one edit): {:?}", start.elapsed());
    let edited = edited.document.expect("a PDF");
    assert_ne!(edited.page_fingerprint(0), document.page_fingerprint(0));

    // hayro also reads the file directly, as for a source PDF.
    let bytes = fs::read(build.join("main.pdf")).expect("pdf file");
    let source_pdf = rasterize_pdf(bytes).expect("hayro opens it");
    assert_eq!(source_pdf.page_count(), 1);
    let image = source_pdf.render_page(0, 2.0).expect("render");
    assert_eq!((image.width, image.height), expected_size(size, 2.0));
    assert!(has_ink(&image));
}

#[test]
fn broken_latex_reports_the_line_and_still_shows_the_pdf() {
    let data = data_dir();
    let Some(mut compiler) = latex_compiler(&data) else {
        return;
    };
    let paper = Paper::new();
    let main = paper.write(
        "main.tex",
        "\\documentclass{article}\n\\begin{document}\nFine text.\nBroken \\notacommand{} here.\nSee \\ref{nowhere}.\n\\end{document}\n",
    );

    let output = compiler.compile(&paper.request(Engine::Latex, "main.tex"));
    let error = output
        .diagnostics
        .iter()
        .find(|d| d.is_error())
        .expect("an error");
    assert!(
        error.message.contains("Undefined control sequence"),
        "{error:?}"
    );
    assert!(same_file(error.file.as_deref().expect("file"), &main));
    assert_eq!(error.line, Some(4));
    assert_eq!(error.column, Some(8), "the column of \\notacommand");
    let warning = output
        .diagnostics
        .iter()
        .find(|d| d.message.contains("Reference `nowhere'"))
        .expect("the undefined reference");
    assert_eq!(warning.line, Some(5));
    // pdfTeX recovers and writes a PDF anyway.
    assert!(output.document.is_some(), "{:#?}", output.diagnostics);
    assert_eq!(paper.listing(), [".oyl", "main.tex"]);
}

#[test]
fn a_missing_package_points_at_its_usepackage() {
    let data = data_dir();
    let Some(mut compiler) = latex_compiler(&data) else {
        return;
    };
    if compiler
        .toolchain(Engine::Latex)
        .detail()
        .starts_with("tectonic")
    {
        eprintln!("skipped: tectonic fetches packages itself");
        return;
    }
    let paper = Paper::new();
    paper.write(
        "main.tex",
        "\\documentclass{article}\n\\usepackage{amsmath}\n\\usepackage{quirenosuchpackage}\n\\begin{document}\nHi.\n\\end{document}\n",
    );
    let output = compiler.compile(&paper.request(Engine::Latex, "main.tex"));
    assert!(output.document.is_none());
    let errors: Vec<_> = output.diagnostics.iter().filter(|d| d.is_error()).collect();
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert!(errors[0].message.contains("quirenosuchpackage.sty"));
    assert_eq!(errors[0].line, Some(3));
    assert!(!errors[0].hints.is_empty());
}

#[test]
fn missing_main_file_is_a_diagnostic() {
    let data = data_dir();
    let Some(mut compiler) = latex_compiler(&data) else {
        return;
    };
    let paper = Paper::new();
    let output = compiler.compile(&paper.request(Engine::Latex, "main.tex"));
    assert!(output.document.is_none() && output.has_errors());
}

/// Fetches tectonic (about 22 MB) into a temporary data folder and checks
/// that it becomes the toolchain. Needs the network:
/// `cargo test -p oyl-typeset -- --ignored`.
#[test]
#[ignore = "downloads tectonic from GitHub"]
fn install_latex_fetches_tectonic() {
    let data = data_dir();
    let compiler = Compiler::new(data.path().to_path_buf());
    let messages = std::cell::RefCell::new(Vec::new());
    compiler
        .install_latex(&|message| {
            println!("install: {message}");
            messages.borrow_mut().push(message.to_owned());
        })
        .expect("install");
    let binary = data.path().join("bin/tectonic");
    assert!(binary.is_file());
    assert!(
        messages
            .borrow()
            .last()
            .is_some_and(|m| m.starts_with("Installed"))
    );
    let status = compiler.toolchain(Engine::Latex);
    assert!(status.is_ready());
    assert!(
        status.detail().starts_with("tectonic at"),
        "{}",
        status.detail()
    );
    assert!(
        data.path().join("tectonic-cache").is_dir(),
        "the cache lives in the data folder"
    );

    // tectonic compiles the article, citation and SyncTeX included.
    let mut compiler = compiler;
    let paper = Paper::new();
    let main = paper.write("main.tex", ARTICLE);
    paper.write("refs.bib", REFS);
    let start = Instant::now();
    let output = compiler.compile(&paper.request(Engine::Latex, "main.tex"));
    println!("TIMING tectonic compile: {:?}", start.elapsed());
    assert_no_errors(&output);
    assert!(
        !output
            .diagnostics
            .iter()
            .any(|d| d.message.contains("undefined")),
        "{:#?}",
        output.diagnostics
    );
    let document = output.document.expect("a PDF");
    assert!(has_ink(&document.render_page(0, 1.0).expect("render")));
    let at = document
        .position_of(&SourceLocation::new(&main, 6, 1))
        .expect("line 6");
    let back = document
        .source_at(PagePosition {
            page: 0,
            x_pt: at.x_pt + 30.0,
            y_pt: at.y_pt + 4.0,
        })
        .expect("a source line");
    assert_eq!(back.line, 6);
    assert_eq!(paper.listing(), [".oyl", "main.tex", "refs.bib"]);

    // Errors: reported with their line, and the PDF is still produced.
    paper.write(
        "main.tex",
        &ARTICLE.replace("explains at length", "explains \\nosuchmacro"),
    );
    let output = compiler.compile(&paper.request(Engine::Latex, "main.tex"));
    let error = output
        .diagnostics
        .iter()
        .find(|d| d.is_error())
        .expect("an error");
    assert_eq!(error.line, Some(4), "{error:?}");
    assert!(same_file(error.file.as_deref().expect("file"), &main));
    assert!(output.document.is_some());
}
