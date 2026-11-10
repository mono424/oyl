//! The one line of truth in the header, and the setup line of the library.
//!
//! Plain functions over plain data, so what the user reads is tested.

use oyl_model::agent::{HarnessId, HarnessInfo, Readiness};
use oyl_model::typeset::ToolchainStatus;

/// How the header's status text is painted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Ordinary secondary text.
    Plain,
    /// Something is wrong (errors, a failed save).
    Danger,
    /// Work in progress.
    Faint,
}

/// The status text and how to paint it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusLine {
    /// What the header says.
    pub text: String,
    /// How it is painted.
    pub tone: Tone,
}

/// Everything the status depends on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusInput {
    /// The paper is being opened.
    pub opening: bool,
    /// The last write of a buffer failed and has not succeeded since.
    pub save_failed: bool,
    /// A compile has been running for longer than a glance.
    pub compiling: bool,
    /// At least one compile of this paper finished.
    pub compiled: bool,
    /// Errors of the last compile.
    pub errors: usize,
    /// Pages of the document on screen.
    pub pages: Option<usize>,
    /// Words of the paper's prose.
    pub words: Option<usize>,
}

/// The status: the most important true thing, in a few words.
///
/// Saving trouble first (the user's text is at stake), then work in
/// progress, then errors, then the size of the paper.
pub fn status_line(input: &StatusInput) -> StatusLine {
    let line = |text: String, tone| StatusLine { text, tone };
    if input.save_failed {
        return line("Saving failed".into(), Tone::Danger);
    }
    if input.opening {
        return line("Opening…".into(), Tone::Faint);
    }
    if input.compiling || !input.compiled {
        return line("Compiling…".into(), Tone::Faint);
    }
    if input.errors > 0 {
        return line(count(input.errors, "error", "errors"), Tone::Danger);
    }
    let parts: Vec<String> = [
        input.pages.map(|pages| count(pages, "page", "pages")),
        input.words.map(|words| count(words, "word", "words")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        line("No pages yet".into(), Tone::Faint)
    } else {
        line(parts.join(" · "), Tone::Plain)
    }
}

/// "1 page", "12 pages", "3,204 words".
pub fn count(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", thousands(n), if n == 1 { one } else { many })
}

/// A number with thousands separators: `3204` → "3,204".
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// What the library's setup line says about LaTeX.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LatexSetup {
    /// Not looked at yet.
    Checking,
    /// Ready; the tool that will be used ("latexmk").
    Ready(String),
    /// Missing, but Oyl can download tectonic.
    Installable,
    /// Missing, and the user has to install it.
    Missing,
    /// Being installed; the latest progress line.
    Installing(String),
}

impl LatexSetup {
    /// From the compiler's report.
    pub fn from_status(status: &ToolchainStatus) -> Self {
        match status {
            ToolchainStatus::Ready { detail } => {
                let tool = detail.split(" at ").next().unwrap_or(detail).trim();
                LatexSetup::Ready(if tool.is_empty() {
                    "ready".into()
                } else {
                    tool.to_string()
                })
            }
            ToolchainStatus::Installable { .. } => LatexSetup::Installable,
            ToolchainStatus::Unavailable { .. } => LatexSetup::Missing,
        }
    }

    /// "LaTeX: latexmk", "LaTeX: not installed".
    pub fn label(&self) -> String {
        match self {
            LatexSetup::Checking => "LaTeX: checking…".into(),
            LatexSetup::Ready(tool) => format!("LaTeX: {tool}"),
            LatexSetup::Installable => "LaTeX: not installed".into(),
            LatexSetup::Missing => "LaTeX: not found (install TeX Live or MacTeX)".into(),
            LatexSetup::Installing(progress) => format!("LaTeX: {progress}"),
        }
    }

    /// Whether Oyl can set LaTeX up from here.
    pub fn can_install(&self) -> bool {
        matches!(self, LatexSetup::Installable)
    }
}

/// How the agent reaches a model, from what was detected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentRoute {
    /// Through the `claude` CLI and its subscription.
    ClaudeCode,
    /// Through the `codex` CLI.
    Codex,
    /// With a provider API key from the environment.
    ApiKey,
    /// No way found.
    None,
}

/// "Agent: Sorcar via Claude Code", "Agent: Claude Code", "Agent: none".
///
/// `setting_up` is the progress line of a harness preparing itself.
pub fn agent_label(
    selected: Option<&HarnessInfo>,
    route: AgentRoute,
    setting_up: Option<&str>,
) -> String {
    let Some(info) = selected else {
        return "Agent: looking…".into();
    };
    if let Some(progress) = setting_up {
        return format!("Agent: {} — {progress}", info.name);
    }
    if info.readiness == Readiness::Unavailable {
        return "Agent: none (install Claude Code or Codex)".into();
    }
    let via = match (info.id.as_str(), route) {
        (HarnessId::SORCAR, AgentRoute::ClaudeCode) => " via Claude Code",
        (HarnessId::SORCAR, AgentRoute::Codex) => " via Codex",
        (HarnessId::SORCAR, AgentRoute::ApiKey) => " via an API key",
        _ => "",
    };
    format!("Agent: {}{via}", info.name)
}

/// The whole setup line: "Typst built in · LaTeX: latexmk · Agent: …".
pub fn setup_line(latex: &LatexSetup, agent: &str) -> String {
    format!("Typst built in · {} · {agent}", latex.label())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oyl_model::agent::ModelChoice;

    fn input() -> StatusInput {
        StatusInput {
            compiled: true,
            pages: Some(12),
            words: Some(3204),
            ..StatusInput::default()
        }
    }

    #[test]
    fn a_healthy_paper_reads_pages_and_words() {
        assert_eq!(
            status_line(&input()),
            StatusLine {
                text: "12 pages · 3,204 words".into(),
                tone: Tone::Plain
            }
        );
        let one = StatusInput {
            pages: Some(1),
            words: Some(1),
            ..input()
        };
        assert_eq!(status_line(&one).text, "1 page · 1 word");
        let no_words = StatusInput {
            words: None,
            ..input()
        };
        assert_eq!(status_line(&no_words).text, "12 pages");
    }

    #[test]
    fn errors_are_counted_in_the_danger_color() {
        let failed = StatusInput {
            errors: 2,
            ..input()
        };
        assert_eq!(
            status_line(&failed),
            StatusLine {
                text: "2 errors".into(),
                tone: Tone::Danger
            }
        );
        let one = StatusInput {
            errors: 1,
            ..input()
        };
        assert_eq!(status_line(&one).text, "1 error");
    }

    #[test]
    fn work_in_progress_comes_before_the_old_result() {
        let compiling = StatusInput {
            compiling: true,
            errors: 3,
            ..input()
        };
        assert_eq!(status_line(&compiling).text, "Compiling…");
        let first = StatusInput {
            compiled: false,
            ..StatusInput::default()
        };
        assert_eq!(status_line(&first).text, "Compiling…");
        let opening = StatusInput {
            opening: true,
            ..StatusInput::default()
        };
        assert_eq!(status_line(&opening).text, "Opening…");
    }

    #[test]
    fn a_failed_save_wins_over_everything() {
        let broken = StatusInput {
            save_failed: true,
            compiling: true,
            errors: 4,
            opening: true,
            ..input()
        };
        assert_eq!(
            status_line(&broken),
            StatusLine {
                text: "Saving failed".into(),
                tone: Tone::Danger
            }
        );
    }

    #[test]
    fn an_empty_result_says_so() {
        let empty = StatusInput {
            compiled: true,
            ..StatusInput::default()
        };
        assert_eq!(status_line(&empty).text, "No pages yet");
    }

    #[test]
    fn thousands_are_separated() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(3204), "3,204");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn latex_setup_reads_the_tool_name() {
        let ready = LatexSetup::from_status(&ToolchainStatus::Ready {
            detail: "latexmk at /Library/TeX/texbin/latexmk".into(),
        });
        assert_eq!(ready, LatexSetup::Ready("latexmk".into()));
        assert_eq!(ready.label(), "LaTeX: latexmk");
        let installable = LatexSetup::from_status(&ToolchainStatus::Installable {
            detail: "No LaTeX found.".into(),
        });
        assert!(installable.can_install());
        assert_eq!(installable.label(), "LaTeX: not installed");
        assert!(!LatexSetup::Missing.can_install());
    }

    fn info(id: &str, name: &str, readiness: Readiness) -> HarnessInfo {
        HarnessInfo {
            id: HarnessId::new(id),
            name: name.into(),
            tagline: String::new(),
            readiness,
            status: String::new(),
            models: vec![ModelChoice {
                id: "m".into(),
                label: "M".into(),
            }],
            default_model: None,
        }
    }

    #[test]
    fn the_agent_label_names_the_route() {
        let sorcar = info("sorcar", "Sorcar", Readiness::Ready);
        assert_eq!(
            agent_label(Some(&sorcar), AgentRoute::ClaudeCode, None),
            "Agent: Sorcar via Claude Code"
        );
        assert_eq!(
            agent_label(Some(&sorcar), AgentRoute::Codex, None),
            "Agent: Sorcar via Codex"
        );
        let claude = info("claude-code", "Claude Code", Readiness::Ready);
        assert_eq!(
            agent_label(Some(&claude), AgentRoute::ClaudeCode, None),
            "Agent: Claude Code"
        );
        let missing = info("sorcar", "Sorcar", Readiness::Unavailable);
        assert_eq!(
            agent_label(Some(&missing), AgentRoute::None, None),
            "Agent: none (install Claude Code or Codex)"
        );
        assert_eq!(
            agent_label(
                Some(&sorcar),
                AgentRoute::ClaudeCode,
                Some("Installing uv…")
            ),
            "Agent: Sorcar — Installing uv…"
        );
        assert_eq!(agent_label(None, AgentRoute::None, None), "Agent: looking…");
    }

    #[test]
    fn the_setup_line_joins_the_parts() {
        assert_eq!(
            setup_line(
                &LatexSetup::Ready("latexmk".into()),
                "Agent: Sorcar via Claude Code"
            ),
            "Typst built in · LaTeX: latexmk · Agent: Sorcar via Claude Code"
        );
    }
}
