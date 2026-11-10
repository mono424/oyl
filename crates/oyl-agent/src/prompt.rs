//! What every harness tells the agent besides the user's message: where the
//! user is in the paper (engine, files, selection, compile errors) and the
//! writing rules Oyl holds every agent to.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

use oyl_model::{Engine, Severity, agent::TurnRequest};

/// Longest selection quoted to the agent, in characters.
const SELECTION_CHARS: usize = 20_000;
/// Most compile errors listed.
const MAX_DIAGNOSTICS: usize = 30;

/// The writing rules, for a system prompt (or the head of the prompt when a
/// harness has no system prompt of its own).
pub fn writing_rules(engine: Option<Engine>, main_file: Option<&Path>) -> String {
    let main = main_file
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| engine.unwrap_or(Engine::Typst).default_main().to_owned());
    let format = match engine {
        Some(engine) => format!("{} (`.{}` files)", engine.label(), engine.extension()),
        None => "its existing format".to_owned(),
    };
    let check = match engine {
        Some(Engine::Latex) => {
            format!("`latexmk -pdf -interaction=nonstopmode -outdir=\"$TMPDIR/oyl-check\" {main}`")
        }
        _ => format!("`typst compile {main} \"$TMPDIR/oyl-check.pdf\"`"),
    };
    format!(
        "You are the writing agent of Oyl, an app for writing academic papers. \
The paper is a folder on disk; the user watches your edits appear live.

Rules for every change:
1. Stay in the paper's format: {format}. Match its structure, macros and style. Never convert between Typst and LaTeX, and do not add packages without need.
2. Cite only keys that exist in the bibliography file (refs.bib). Never invent a reference, cite key, author, title, year or DOI.
3. When a claim needs a new source, look it up for real: Crossref (`curl -s \"https://api.crossref.org/works?rows=5&query.bibliographic=<words>\"`) or arXiv (`curl -s \"http://export.arxiv.org/api/query?search_query=all:<words>&max_results=5\"`). Add its real BibTeX to refs.bib (for a DOI: `curl -sLH \"Accept: application/x-bibtex\" https://doi.org/<DOI>`), then cite it. If you cannot verify a source, say so instead of citing it.
4. Figures go in figures/ and are included with a caption and a label.
5. Keep the paper compiling. After a structural change, check it with {check} when that tool is installed, and fix what you broke.
6. Edit the files in place in this folder. Do not create copies, git branches or worktrees; Oyl keeps a version of the folder before and after your turn.
7. Never modify anything under .oyl/ — it is Oyl's own data (history, chats, builds).
8. Finish with a 1–3 sentence summary, in plain prose, of what you changed (or found and did not change)."
    )
}

/// Where the user is: the paper folder, engine, files, selection, compile
/// errors and attachments, as a Markdown block.
pub fn paper_context(request: &TurnRequest) -> String {
    let root = &request.project_root;
    let engine = engine_of(request);
    let mut out = String::from("# Paper\n");
    let _ = writeln!(out, "- Folder: {}", root.display());
    if let Some(engine) = engine {
        let _ = writeln!(out, "- Engine: {}", engine.label());
    }
    if let Some(main) = &request.main_file {
        let _ = writeln!(out, "- Main file: {}", shown(main, root));
    }
    if let Some(active) = &request.active_file {
        let _ = writeln!(out, "- Open in the editor: {}", shown(active, root));
    }
    match bibliography(root) {
        Some(bib) => {
            let _ = writeln!(out, "- Bibliography: {}", shown(&bib, root));
        }
        None => out.push_str(
            "- Bibliography: none yet (create refs.bib when the first source is added)\n",
        ),
    }

    if let Some(selection) = &request.selection {
        let lines = if selection.start_line == selection.end_line {
            format!("line {}", selection.start_line)
        } else {
            format!("lines {}–{}", selection.start_line, selection.end_line)
        };
        let _ = write!(
            out,
            "\n# Selected text ({}, {lines})\nThe user's message is about this passage.\n{}\n",
            shown(&selection.file, root),
            fenced(&numbered(&selection.text, selection.start_line))
        );
    }

    let errors: Vec<_> = request
        .diagnostics
        .iter()
        .filter(|d| d.severity != Severity::Info)
        .collect();
    if !errors.is_empty() {
        out.push_str("\n# Current compile problems\n");
        for diagnostic in errors.iter().take(MAX_DIAGNOSTICS) {
            let location = match (&diagnostic.file, diagnostic.line) {
                (Some(file), Some(line)) => match diagnostic.column {
                    Some(column) => format!("{}:{line}:{column}: ", shown(file, root)),
                    None => format!("{}:{line}: ", shown(file, root)),
                },
                (Some(file), None) => format!("{}: ", shown(file, root)),
                _ => String::new(),
            };
            let severity = if diagnostic.is_error() {
                "error"
            } else {
                "warning"
            };
            let _ = write!(
                out,
                "- {location}{severity}: {}",
                one_line(&diagnostic.message)
            );
            for hint in &diagnostic.hints {
                let _ = write!(out, " (hint: {})", one_line(hint));
            }
            out.push('\n');
        }
        if errors.len() > MAX_DIAGNOSTICS {
            let _ = writeln!(out, "- … and {} more", errors.len() - MAX_DIAGNOSTICS);
        }
    }

    if !request.attachments.is_empty() {
        out.push_str("\n# Attached files\n");
        for attachment in &request.attachments {
            let _ = writeln!(out, "- {}", attachment.display());
        }
    }
    out
}

/// The user's message with the paper context in front of it.
pub fn user_message(request: &TurnRequest) -> String {
    format!(
        "{}\n# Request\n{}\n",
        paper_context(request),
        request.prompt.trim()
    )
}

/// Context, rules and the message in one prompt, for a harness without a
/// separate system prompt.
pub fn full_prompt(request: &TurnRequest) -> String {
    format!(
        "{}\n# Oyl's rules\n{}\n\n# Request\n{}\n",
        paper_context(request),
        writing_rules(engine_of(request), request.main_file.as_deref()),
        request.prompt.trim()
    )
}

/// The engine of a request, from the request or the main file's extension.
pub(crate) fn engine_of(request: &TurnRequest) -> Option<Engine> {
    request
        .engine
        .or_else(|| request.main_file.as_deref().and_then(Engine::from_path))
}

fn bibliography(root: &Path) -> Option<PathBuf> {
    let refs = root.join("refs.bib");
    if refs.is_file() {
        return Some(refs);
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "bib") && path.is_file())
        .collect();
    found.sort();
    found.into_iter().next()
}

fn shown(path: &Path, root: &Path) -> String {
    crate::tools::display_path(&path.to_string_lossy(), root)
}

fn numbered(text: &str, first_line: u32) -> String {
    let text = crate::tools::clip(text, SELECTION_CHARS);
    text.lines()
        .enumerate()
        .map(|(i, line)| format!("{:>5}  {line}", first_line as usize + i))
        .collect::<Vec<_>>()
        .join("\n")
}

fn fenced(text: &str) -> String {
    let mut fence = "```".to_owned();
    while text.contains(&fence) {
        fence.push('`');
    }
    format!("{fence}\n{text}\n{fence}")
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use oyl_model::{Diagnostic, agent::Selection};

    fn request(root: &Path) -> TurnRequest {
        TurnRequest {
            project_root: root.to_path_buf(),
            engine: Some(Engine::Latex),
            main_file: Some(root.join("main.tex")),
            active_file: Some(root.join("sections/intro.tex")),
            selection: Some(Selection {
                file: root.join("sections/intro.tex"),
                start_line: 12,
                end_line: 13,
                text: "Owls are birds.\nThey hunt at night.".into(),
            }),
            prompt: "  Make this more precise.  ".into(),
            diagnostics: vec![
                Diagnostic::error("Undefined control sequence \\foo").at(
                    root.join("main.tex"),
                    40,
                    3,
                ),
                Diagnostic::warning("Citation `x` undefined"),
            ],
            attachments: vec![PathBuf::from("/tmp/plot.png")],
            ..TurnRequest::default()
        }
    }

    #[test]
    fn context_lists_everything_relative_to_the_folder() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("refs.bib"), "").expect("bib");
        let context = paper_context(&request(temp.path()));
        assert!(context.contains("- Engine: LaTeX"));
        assert!(context.contains("- Main file: main.tex"));
        assert!(context.contains("- Open in the editor: sections/intro.tex"));
        assert!(context.contains("- Bibliography: refs.bib"));
        assert!(context.contains("# Selected text (sections/intro.tex, lines 12–13)"));
        assert!(context.contains("   12  Owls are birds.\n   13  They hunt at night."));
        assert!(context.contains("- main.tex:40:3: error: Undefined control sequence \\foo"));
        assert!(context.contains("- warning: Citation `x` undefined"));
        assert!(context.contains("- /tmp/plot.png"));
    }

    #[test]
    fn messages_and_rules() {
        let temp = tempfile::tempdir().expect("tempdir");
        let req = request(temp.path());
        let message = user_message(&req);
        assert!(message.ends_with("# Request\nMake this more precise.\n"));
        assert!(message.contains("Bibliography: none yet"));
        let rules = writing_rules(Some(Engine::Latex), Some(Path::new("/x/main.tex")));
        assert!(rules.contains("LaTeX (`.tex` files)"));
        assert!(rules.contains("latexmk -pdf"));
        assert!(rules.contains("refs.bib"));
        assert!(rules.contains(".oyl/"));
        assert!(rules.contains("1–3 sentence summary"));
        let typst = writing_rules(Some(Engine::Typst), None);
        assert!(typst.contains("typst compile main.typ"));
        let full = full_prompt(&req);
        assert!(
            full.contains("# Oyl's rules") && full.contains("# Request\nMake this more precise.")
        );
    }

    #[test]
    fn engine_comes_from_the_main_file_when_unset() {
        let req = TurnRequest {
            main_file: Some(PathBuf::from("/p/main.typ")),
            ..TurnRequest::default()
        };
        assert_eq!(engine_of(&req), Some(Engine::Typst));
    }

    #[test]
    fn fences_grow_past_backticks_in_the_selection() {
        assert_eq!(fenced("a ``` b"), "````\na ``` b\n````");
    }
}
