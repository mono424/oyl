//! Compiler problems: their order, where they point, and the prompt that
//! hands them to the agent.

use std::fmt::Write as _;

use oyl_model::{Diagnostic, Severity, SourceLocation};

use super::{file_name, plural};

fn rank(severity: Severity) -> u8 {
    match severity {
        Severity::Error => 0,
        Severity::Warning => 1,
        Severity::Info => 2,
    }
}

/// Errors first, then warnings, then notes; otherwise in the order given.
pub fn ordered(mut diagnostics: Vec<Diagnostic>) -> Vec<Diagnostic> {
    diagnostics.sort_by_key(|d| rank(d.severity));
    diagnostics
}

/// Where a problem points, to jump there: its file and line (column 1 when
/// none is given). `None` when it does not say.
pub fn location(diagnostic: &Diagnostic) -> Option<SourceLocation> {
    let file = diagnostic.file.as_ref()?;
    let line = diagnostic.line?;
    Some(SourceLocation::new(
        file.clone(),
        line.max(1),
        diagnostic.column.unwrap_or(1).max(1),
    ))
}

/// "main.typ:42", "main.typ", or `None`.
pub fn place(diagnostic: &Diagnostic) -> Option<String> {
    let file = diagnostic.file.as_ref()?;
    Some(match diagnostic.line {
        Some(line) => format!("{}:{line}", file_name(file)),
        None => file_name(file),
    })
}

/// "2 errors · 1 warning", "1 error", "3 warnings", or empty.
pub fn summary(diagnostics: &[Diagnostic]) -> String {
    let errors = diagnostics.iter().filter(|d| d.severity == Severity::Error).count();
    let warnings = diagnostics.iter().filter(|d| d.severity == Severity::Warning).count();
    let mut parts = Vec::new();
    if errors > 0 {
        parts.push(plural(errors, "error", "errors"));
    }
    if warnings > 0 {
        parts.push(plural(warnings, "warning", "warnings"));
    }
    parts.join(" · ")
}

fn describe(out: &mut String, n: usize, d: &Diagnostic) {
    let place = match (&d.file, d.line, d.column) {
        (Some(file), Some(line), Some(col)) => format!("{}:{line}:{col}", file.display()),
        (Some(file), Some(line), None) => format!("{}:{line}", file.display()),
        (Some(file), None, _) => file.display().to_string(),
        (None, _, _) => "(no file given)".to_string(),
    };
    let message = d.message.trim().replace('\n', "\n   ");
    let _ = writeln!(out, "{n}. {place} — {message}");
    for hint in &d.hints {
        let _ = writeln!(out, "   hint: {}", hint.trim());
    }
}

/// What "Fix with agent" asks: the errors, each with its file, line and
/// message, and the request to fix them and keep the paper compiling. With
/// no errors, the warnings. `None` when there is nothing to fix.
pub fn fix_prompt(diagnostics: &[Diagnostic]) -> Option<String> {
    let errors: Vec<&Diagnostic> = diagnostics.iter().filter(|d| d.is_error()).collect();
    let warnings: Vec<&Diagnostic> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .collect();
    let mut out = String::new();
    if !errors.is_empty() {
        let _ = writeln!(
            out,
            "The paper does not compile. Fix {}:\n",
            if errors.len() == 1 {
                "this error"
            } else {
                "these errors"
            }
        );
        for (ix, d) in errors.iter().enumerate() {
            describe(&mut out, ix + 1, d);
        }
        out.push_str(
            "\nChange only what is needed to fix them, without rewriting the text around them. \
             Keep the paper compiling: when you are done it must compile without errors.",
        );
    } else if !warnings.is_empty() {
        let _ = writeln!(
            out,
            "The paper compiles, with {}. Resolve {}:\n",
            plural(warnings.len(), "warning", "warnings"),
            if warnings.len() == 1 { "it" } else { "them" }
        );
        for (ix, d) in warnings.iter().enumerate() {
            describe(&mut out, ix + 1, d);
        }
        out.push_str(
            "\nChange only what is needed, without changing what the text says. \
             Keep the paper compiling.",
        );
    } else {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostics() -> Vec<Diagnostic> {
        vec![
            Diagnostic::warning("unused label").at("/p/main.typ", 3, 1),
            Diagnostic::error("unknown variable: foo").at("/p/main.typ", 42, 5),
            Diagnostic {
                hints: vec!["did you mean `bar`?".into()],
                ..Diagnostic::error("expected expression").at("/p/ch/intro.typ", 7, 2)
            },
            Diagnostic::error("the compiler crashed"),
        ]
    }

    #[test]
    fn errors_come_first_in_their_order() {
        let ordered = ordered(diagnostics());
        let messages: Vec<&str> = ordered.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "unknown variable: foo",
                "expected expression",
                "the compiler crashed",
                "unused label"
            ]
        );
        assert_eq!(summary(&ordered), "3 errors · 1 warning");
        assert_eq!(place(&ordered[0]).as_deref(), Some("main.typ:42"));
        assert_eq!(place(&ordered[2]), None);
        assert_eq!(
            location(&ordered[0]),
            Some(SourceLocation::new("/p/main.typ", 42, 5))
        );
        assert_eq!(location(&ordered[2]), None);
    }

    #[test]
    fn the_fix_prompt_lists_every_error_and_asks_to_keep_compiling() {
        let prompt = fix_prompt(&ordered(diagnostics())).unwrap();
        assert!(prompt.starts_with("The paper does not compile. Fix these errors:"));
        assert!(prompt.contains("1. /p/main.typ:42:5 — unknown variable: foo"));
        assert!(prompt.contains("2. /p/ch/intro.typ:7:2 — expected expression\n   hint: did you mean `bar`?"));
        assert!(prompt.contains("3. (no file given) — the compiler crashed"));
        assert!(!prompt.contains("unused label"));
        assert!(prompt.contains("Keep the paper compiling"));
    }

    #[test]
    fn warnings_alone_are_offered_too_and_nothing_means_no_prompt() {
        let warnings = vec![Diagnostic::warning("overfull hbox").at("/p/main.tex", 9, 1)];
        let prompt = fix_prompt(&warnings).unwrap();
        assert!(prompt.starts_with("The paper compiles, with 1 warning. Resolve it:"));
        assert!(prompt.contains("1. /p/main.tex:9:1 — overfull hbox"));
        assert_eq!(fix_prompt(&[]), None);
        assert_eq!(summary(&[]), "");
    }
}
