//! Typst, end to end: compile, render, diagnostics, overlays, incremental
//! recompiles, click-to-source.

mod common;

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use common::{
    Paper, assert_no_errors, data_dir, expected_size, has_ink, is_opaque_white_paper, same_file,
};
use oyl_model::{Engine, Severity, SourceLocation};
use oyl_typeset::{Compiler, PagePosition, rasterize_pdf};

const THREE_PAGES: &str = "#set page(paper: \"a4\")
= First
Some text on the first page.
#pagebreak()
= Second
Text on the second page.
#pagebreak()
= Third
And the third page, with math $sum_(i=1)^n i = (n(n+1))/2$.
";

#[test]
fn typst_paper_compiles_and_renders_at_both_scales() {
    let paper = Paper::new();
    paper.write("main.typ", THREE_PAGES);
    let data = data_dir();
    let mut compiler = Compiler::new(data.path().to_path_buf());

    let output = compiler.compile(&paper.request(Engine::Typst, "main.typ"));
    assert_no_errors(&output);
    let document = output.document.expect("a document");
    assert_eq!(document.page_count(), 3);
    let size = document.page_size_pt(0);
    assert!(
        (size.0 - 595.28).abs() < 0.1 && (size.1 - 841.89).abs() < 0.1,
        "{size:?}"
    );

    for scale in [1.0, 2.0] {
        let start = Instant::now();
        let image = document.render_page(0, scale).expect("render");
        println!("render page 1 at {scale}: {:?}", start.elapsed());
        assert_eq!((image.width, image.height), expected_size(size, scale));
        assert!(is_opaque_white_paper(&image));
        assert!(has_ink(&image), "text is drawn in dark ink");
    }
    assert!(document.render_page(3, 1.0).is_err());
    assert!(document.render_page(0, 0.0).is_err());
    assert!(document.render_page(0, 500.0).is_err());

    // Pages render concurrently from several threads.
    let handles: Vec<_> = (0..6)
        .map(|i| {
            let document = Arc::clone(&document);
            thread::spawn(move || {
                document
                    .render_page(i % 3, 1.5)
                    .map(|image| has_ink(&image))
            })
        })
        .collect();
    for handle in handles {
        assert!(handle.join().expect("thread").expect("render"));
    }

    // PDF on demand, cached, and readable by hayro.
    let pdf = document.pdf().expect("pdf");
    assert!(pdf.starts_with(b"%PDF"));
    assert!(Arc::ptr_eq(&pdf, &document.pdf().expect("pdf")));
    let reread = rasterize_pdf(pdf.to_vec()).expect("hayro reads the PDF");
    assert_eq!(reread.page_count(), 3);
    let reread_size = reread.page_size_pt(2);
    assert!((reread_size.0 - size.0).abs() < 0.5 && (reread_size.1 - size.1).abs() < 0.5);
    let image = reread.render_page(2, 2.0).expect("hayro render");
    assert_eq!((image.width, image.height), expected_size(reread_size, 2.0));
    assert!(is_opaque_white_paper(&image) && has_ink(&image));
    assert_ne!(reread.page_fingerprint(0), reread.page_fingerprint(1));
}

#[test]
fn overlays_take_precedence_over_disk() {
    let paper = Paper::new();
    paper.write("main.typ", "Hello from disk.\n#include \"chapter.typ\"\n");
    paper.write("chapter.typ", "Chapter on disk.\n");
    let data = data_dir();
    let mut compiler = Compiler::new(data.path().to_path_buf());

    let mut request = paper.request(Engine::Typst, "main.typ");
    let first = compiler.compile(&request);
    assert_no_errors(&first);
    let first = first.document.expect("document");
    assert_eq!(first.page_count(), 1);

    // An unsaved edit of an included file adds a page.
    request.overlays.insert(
        paper.path("chapter.typ"),
        "Chapter in the editor.\n#pagebreak()\nA second page.\n".into(),
    );
    let second = compiler.compile(&request);
    assert_no_errors(&second);
    let second = second.document.expect("document");
    assert_eq!(second.page_count(), 2);
    assert_ne!(first.page_fingerprint(0), second.page_fingerprint(0));

    // A buffer for a file that was never saved is visible too.
    request.overlays.insert(
        paper.path("main.typ"),
        "Hello from disk.\n#include \"new.typ\"\n".into(),
    );
    request
        .overlays
        .insert(paper.path("new.typ"), "Brand new.".into());
    let third = compiler.compile(&request);
    assert_no_errors(&third);
    assert_eq!(third.document.expect("document").page_count(), 1);

    // Back to disk: identical content gives identical fingerprints.
    request.overlays.clear();
    let again = compiler.compile(&request).document.expect("document");
    assert_eq!(again.page_fingerprint(0), first.page_fingerprint(0));
}

#[test]
fn errors_point_at_file_line_and_column() {
    let paper = Paper::new();
    let main = paper.write(
        "main.typ",
        "= Title\nSome text.\n#let x = undefined-thing\nMore text.\n",
    );
    let data = data_dir();
    let mut compiler = Compiler::new(data.path().to_path_buf());

    let output = compiler.compile(&paper.request(Engine::Typst, "main.typ"));
    assert!(output.document.is_none());
    let error = output
        .diagnostics
        .iter()
        .find(|d| d.is_error())
        .expect("an error");
    assert!(error.message.contains("undefined-thing"), "{error:?}");
    assert!(same_file(error.file.as_deref().expect("file"), &main));
    assert_eq!((error.line, error.column), (Some(3), Some(10)));

    // In an included file, with unicode before the error (columns count
    // characters, not bytes).
    paper.write("main.typ", "#include \"sec/part.typ\"\n");
    let part = paper.write("sec/part.typ", "Ünïcödé text.\nÄ #nope(1)\n");
    let output = compiler.compile(&paper.request(Engine::Typst, "main.typ"));
    let error = output
        .diagnostics
        .iter()
        .find(|d| d.is_error())
        .expect("an error");
    assert!(
        same_file(error.file.as_deref().expect("file"), &part),
        "{error:?}"
    );
    assert_eq!((error.line, error.column), (Some(2), Some(4)));

    // Warnings come with positions too.
    paper.write(
        "main.typ",
        "Text.\n#set text(font: \"No Such Font Oyl\")\nMore.\n",
    );
    let output = compiler.compile(&paper.request(Engine::Typst, "main.typ"));
    assert!(
        output.document.is_some(),
        "a warning does not fail the compile"
    );
    let warning = output
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Warning)
        .expect("a warning");
    assert_eq!(warning.line, Some(2), "{warning:?}");
}

#[test]
fn bad_requests_are_diagnostics() {
    let paper = Paper::new();
    let data = data_dir();
    let mut compiler = Compiler::new(data.path().to_path_buf());

    let output = compiler.compile(&paper.request(Engine::Typst, "missing.typ"));
    assert!(output.document.is_none() && output.has_errors());

    let mut request = paper.request(Engine::Typst, "main.typ");
    request.main = std::env::temp_dir().join("outside-the-project.typ");
    let output = compiler.compile(&request);
    assert!(output.document.is_none() && output.has_errors());

    request.root = paper.path("no-such-folder");
    let output = compiler.compile(&request);
    assert!(output.document.is_none() && output.has_errors());
}

const REFS: &str = "@book{knuth84,
  author = {Donald E. Knuth},
  title = {The TeXbook},
  publisher = {Addison-Wesley},
  year = {1984},
}
";

#[test]
fn bibliography_and_citations() {
    let paper = Paper::new();
    paper.write("refs.bib", REFS);
    let main = paper.write(
        "main.typ",
        "#set page(width: 300pt, height: 400pt)\nTypesetting is an art @knuth84.\n\n#bibliography(\"refs.bib\")\n",
    );
    let data = data_dir();
    let mut compiler = Compiler::new(data.path().to_path_buf());

    let output = compiler.compile(&paper.request(Engine::Typst, "main.typ"));
    assert_no_errors(&output);
    let document = output.document.expect("document");
    assert_eq!(document.page_count(), 1);
    assert!(has_ink(&document.render_page(0, 1.0).expect("render")));

    // Clicking the citation (a link to the bibliography) opens the source.
    let column = "Typesetting is an art @".chars().count() as u32;
    let at = document
        .position_of(&SourceLocation::new(&main, 2, column))
        .expect("the citation is on the page");
    let clicked = document
        .source_at(PagePosition {
            page: 0,
            x_pt: at.x_pt + 2.0,
            y_pt: at.y_pt - 3.0,
        })
        .expect("source of the citation");
    assert!(same_file(&clicked.file, &main));
    assert_eq!(clicked.line, 2);

    // An unknown key is an error at its line.
    paper.write(
        "main.typ",
        "Text.\nAs shown @nokey2000.\n#bibliography(\"refs.bib\")\n",
    );
    let output = compiler.compile(&paper.request(Engine::Typst, "main.typ"));
    let error = output
        .diagnostics
        .iter()
        .find(|d| d.is_error())
        .expect("an error");
    assert!(error.message.contains("nokey2000"), "{error:?}");
    assert_eq!(error.line, Some(2));
}

/// A ten-page paper with headings, paragraphs, lists and math.
fn long_paper() -> String {
    let words = [
        "typesetting",
        "paper",
        "layout",
        "glyph",
        "kerning",
        "margin",
        "column",
        "figure",
        "theorem",
        "proof",
        "reference",
        "citation",
        "section",
        "paragraph",
        "line",
        "page",
        "measure",
        "leading",
        "font",
        "ligature",
        "hyphen",
        "justify",
        "ragged",
        "baseline",
    ];
    let mut seed = 7_u32;
    let mut word = || {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        words[(seed >> 16) as usize % words.len()]
    };
    let mut text = String::from(
        "#set page(paper: \"a4\", margin: 2.5cm)\n#set heading(numbering: \"1.1\")\n#set par(justify: true)\n\n",
    );
    for section in 1..=10 {
        text.push_str(&format!("= Section {section}\n\n"));
        for paragraph in 0..5 {
            let sentence: Vec<&str> = (0..90).map(|_| word()).collect();
            text.push_str(&sentence.join(" "));
            text.push_str(".\n\n");
            if paragraph == 2 {
                text.push_str("$ integral_0^1 x^2 dif x = 1/3 quad sum_(k=1)^n k = (n(n+1))/2 $\n\n- one item\n- another item\n\n");
            }
        }
        text.push_str("#pagebreak(weak: true)\n");
    }
    text
}

#[test]
fn keystroke_recompile_is_incremental() {
    let paper = Paper::new();
    let source = long_paper();
    paper.write("main.typ", &source);
    let data = data_dir();
    let mut compiler = Compiler::new(data.path().to_path_buf());
    let mut request = paper.request(Engine::Typst, "main.typ");

    let start = Instant::now();
    let cold = compiler.compile(&request);
    let cold_time = start.elapsed();
    assert_no_errors(&cold);
    let pages = cold.document.as_ref().expect("document").page_count();
    assert!(pages >= 10, "{pages} pages");

    // Type one character in the middle of section 5, twice.
    let middle = source.find("= Section 5").expect("section 5") + 40;
    let mut timings = Vec::new();
    for edit in ["x", "xy"] {
        let mut edited = source.clone();
        edited.insert_str(middle, edit);
        request.overlays.insert(paper.path("main.typ"), edited);
        let start = Instant::now();
        let warm = compiler.compile(&request);
        timings.push(start.elapsed());
        assert_no_errors(&warm);
        let warm = warm.document.expect("document");
        let before = cold.document.as_ref().expect("document");
        // Pages before the edit are untouched; the edited one changed.
        assert_eq!(warm.page_fingerprint(0), before.page_fingerprint(0));
        assert!(
            (0..pages).any(|page| warm.page_fingerprint(page) != before.page_fingerprint(page)),
            "the edit shows up"
        );
    }

    let document = cold.document.expect("document");
    let start = Instant::now();
    let image = document.render_page(0, 2.0).expect("render");
    let render_time = start.elapsed();
    assert!(has_ink(&image));

    println!(
        "TIMING typst {pages} pages: cold compile {cold_time:?} (includes font discovery), \
         warm recompiles {:?} and {:?}, page render at 2.0 {render_time:?}",
        timings[0], timings[1]
    );
    let warm = timings[1];
    assert!(
        warm * 2 < cold_time,
        "a one-character recompile ({warm:?}) should be much faster than the cold compile ({cold_time:?})"
    );
    assert!(
        warm < Duration::from_secs(2),
        "warm recompile took {warm:?}"
    );
}

#[test]
fn click_to_source_and_cursor_to_preview_round_trip() {
    let paper = Paper::new();
    let main = paper.write(
        "main.typ",
        "#set page(width: 300pt, height: 300pt, margin: 20pt)\n= A heading\nFirst paragraph, on line three.\n\nSecond paragraph, on line five of the file.\n#pagebreak()\nThe second page starts on line seven.\n",
    );
    let data = data_dir();
    let mut compiler = Compiler::new(data.path().to_path_buf());
    let output = compiler.compile(&paper.request(Engine::Typst, "main.typ"));
    assert_no_errors(&output);
    let document = output.document.expect("document");

    for (line, page) in [(3, 0), (5, 0), (7, 1)] {
        let at = document
            .position_of(&SourceLocation::new(&main, line, 8))
            .unwrap_or_else(|| panic!("line {line} is on a page"));
        assert_eq!(at.page, page, "line {line}");
        let (width, height) = document.page_size_pt(page);
        assert!(
            at.x_pt > 0.0 && at.x_pt < width && at.y_pt > 0.0 && at.y_pt < height,
            "{at:?}"
        );
        let back = document
            .source_at(PagePosition {
                page,
                x_pt: at.x_pt + 1.0,
                y_pt: at.y_pt - 3.0,
            })
            .unwrap_or_else(|| panic!("text of line {line} under {at:?}"));
        assert!(same_file(&back.file, &main));
        assert_eq!(back.line, line);
        assert!(
            (7..=10).contains(&back.column),
            "column {} near 8",
            back.column
        );
    }

    // The position follows the cursor along the line.
    let near = document
        .position_of(&SourceLocation::new(&main, 3, 2))
        .expect("line 3");
    let far = document
        .position_of(&SourceLocation::new(&main, 3, 25))
        .expect("line 3");
    assert!(far.x_pt > near.x_pt + 50.0, "{near:?} {far:?}");
    assert!((far.y_pt - near.y_pt).abs() < 0.5);

    // The cursor on heading markup still finds the heading.
    let heading = document
        .position_of(&SourceLocation::new(&main, 2, 1))
        .expect("heading");
    assert_eq!(heading.page, 0);
    // Lines further down are further down the page.
    let three = document
        .position_of(&SourceLocation::new(&main, 3, 1))
        .expect("line 3");
    let five = document
        .position_of(&SourceLocation::new(&main, 5, 1))
        .expect("line 5");
    assert!(heading.y_pt < three.y_pt && three.y_pt < five.y_pt);
    // Other files and empty margins have no answer.
    assert!(
        document
            .position_of(&SourceLocation::new(paper.path("other.typ"), 1, 1))
            .is_none()
    );
    assert!(
        document
            .source_at(PagePosition {
                page: 0,
                x_pt: 2.0,
                y_pt: 2.0
            })
            .is_none()
    );
    assert!(
        document
            .source_at(PagePosition {
                page: 9,
                x_pt: 2.0,
                y_pt: 2.0
            })
            .is_none()
    );
}

#[test]
fn a_missing_package_is_a_diagnostic() {
    let paper = Paper::new();
    paper.write(
        "main.typ",
        "#import \"@preview/oyl-surely-missing-package:0.0.1\": *\nText.\n",
    );
    let data = data_dir();
    let mut compiler = Compiler::new(data.path().to_path_buf());
    let output = compiler.compile(&paper.request(Engine::Typst, "main.typ"));
    assert!(output.document.is_none());
    let error = output
        .diagnostics
        .iter()
        .find(|d| d.is_error())
        .expect("an error");
    assert_eq!(error.line, Some(1), "{error:?}");
}

/// Downloads a real package from Typst Universe into the data folder.
/// Needs the network: `cargo test -p oyl-typeset -- --ignored`.
#[test]
#[ignore = "downloads from packages.typst.org"]
fn preview_packages_download_on_demand() {
    let paper = Paper::new();
    paper.write(
        "main.typ",
        "#import \"@preview/example:0.1.0\": add\nOne plus two is #add(1, 2).\n",
    );
    let data = data_dir();
    let mut compiler = Compiler::new(data.path().to_path_buf());
    let output = compiler.compile(&paper.request(Engine::Typst, "main.typ"));
    assert_no_errors(&output);
    assert!(data.path().join("packages/preview/example/0.1.0").is_dir());
    // The second compile is served from the cache.
    let output = compiler.compile(&paper.request(Engine::Typst, "main.typ"));
    assert_no_errors(&output);
}
