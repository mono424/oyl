//! The harnesses Oyl offers, in picker order.

use std::{path::PathBuf, sync::Arc};

use oyl_model::agent::{FactChecker, Harness, HarnessId, HarnessInfo, Readiness, Rewriter};
use parking_lot::RwLock;

use crate::{
    claude::ClaudeCodeHarness,
    codex::CodexHarness,
    detect::{Detection, Setup},
    factcheck::{ClaudeFactChecker, CodexFactChecker},
    rewrite::{ClaudeRewriter, CodexRewriter},
    sorcar::SorcarHarness,
};

/// Preference order for the default harness.
const DEFAULT_ORDER: [&str; 3] = [HarnessId::SORCAR, HarnessId::CLAUDE_CODE, HarnessId::CODEX];

/// Holds the harnesses. Adding one is a single [`Registry::register`] call.
pub struct Registry {
    setup: Arc<Setup>,
    /// Each harness with the id it had when registered (an id never changes,
    /// and `info()` may touch the file system).
    harnesses: RwLock<Vec<(HarnessId, Arc<dyn Harness>)>>,
}

impl Registry {
    /// Detect what is installed and register the built-in harnesses: Sorcar,
    /// Claude Code and Codex. `data_dir` is Oyl's data folder (Sorcar's
    /// environment and a uv Oyl installs live there). Takes up to a few
    /// seconds (it runs `--version` and may ask the login shell): call it
    /// off the UI thread.
    pub fn detect(data_dir: impl Into<PathBuf>) -> Registry {
        Self::with_builtins(Setup::detect(data_dir))
    }

    /// The built-in harnesses over an existing setup.
    pub fn with_builtins(setup: Arc<Setup>) -> Registry {
        let registry = Self::empty(Arc::clone(&setup));
        registry.register(Arc::new(SorcarHarness::new(Arc::clone(&setup))));
        registry.register(Arc::new(ClaudeCodeHarness::new(Arc::clone(&setup))));
        registry.register(Arc::new(CodexHarness::new(setup)));
        registry
    }

    /// No harness yet; add them with [`Registry::register`].
    pub fn empty(setup: Arc<Setup>) -> Registry {
        Registry {
            setup,
            harnesses: RwLock::new(Vec::new()),
        }
    }

    /// Add a harness at the end of the picker, or replace the one with the
    /// same id in place.
    pub fn register(&self, harness: Arc<dyn Harness>) {
        let id = harness.info().id;
        let mut harnesses = self.harnesses.write();
        match harnesses.iter().position(|(existing, _)| *existing == id) {
            Some(index) => harnesses[index].1 = harness,
            None => harnesses.push((id, harness)),
        }
    }

    /// Every harness, in picker order.
    pub fn harnesses(&self) -> Vec<Arc<dyn Harness>> {
        self.harnesses
            .read()
            .iter()
            .map(|(_, harness)| Arc::clone(harness))
            .collect()
    }

    /// The harness with this id.
    pub fn get(&self, id: &HarnessId) -> Option<Arc<dyn Harness>> {
        self.harnesses
            .read()
            .iter()
            .find(|(existing, _)| existing == id)
            .map(|(_, harness)| Arc::clone(harness))
    }

    /// The picker entries, in order.
    pub fn infos(&self) -> Vec<HarnessInfo> {
        self.harnesses
            .read()
            .iter()
            .map(|(_, harness)| harness.info())
            .collect()
    }

    /// The harness to use when the user picked none: Sorcar when it can run
    /// (also when it sets itself up on first use), else Claude Code, else
    /// Codex, else any other usable harness. Sorcar when nothing can run, so
    /// the UI shows what to install.
    pub fn default_id(&self) -> HarnessId {
        let infos = self.infos();
        let usable = |info: &&HarnessInfo| info.readiness != Readiness::Unavailable;
        DEFAULT_ORDER
            .iter()
            .find_map(|id| {
                infos
                    .iter()
                    .filter(usable)
                    .find(|info| info.id.as_str() == *id)
            })
            .or_else(|| infos.iter().find(usable))
            .map(|info| info.id.clone())
            .unwrap_or_else(HarnessId::sorcar)
    }

    /// Detect again (after the user installed something); every harness
    /// sees the result.
    pub fn refresh(&self) -> Arc<Detection> {
        self.setup.refresh()
    }

    /// The current detection.
    pub fn detection(&self) -> Arc<Detection> {
        self.setup.detection()
    }

    /// The shared setup the built-in harnesses use.
    pub fn setup(&self) -> &Arc<Setup> {
        &self.setup
    }

    /// What polishes a selection (⌘B): Claude Code when it is installed,
    /// else Codex, else nothing ([`Registry::rewriter_status`] says why).
    pub fn rewriter(&self) -> Option<Arc<dyn Rewriter>> {
        let detection = self.setup.detection();
        if detection.claude.is_some() {
            Some(Arc::new(ClaudeRewriter::new(Arc::clone(&self.setup))))
        } else if detection.codex.is_some() {
            Some(Arc::new(CodexRewriter::new(Arc::clone(&self.setup))))
        } else {
            None
        }
    }

    /// One line on [`Registry::rewriter`], for the UI.
    pub fn rewriter_status(&self) -> String {
        match self.rewriter() {
            Some(rewriter) => format!("Polishes text with {}", rewriter.name()),
            None => "Install Claude Code or Codex to polish text with ⌘B".into(),
        }
    }

    /// What fact-checks a selection (⇧⌘B): Claude Code with web search when
    /// it is installed, else Codex, else nothing.
    pub fn fact_checker(&self) -> Option<Arc<dyn FactChecker>> {
        let detection = self.setup.detection();
        if detection.claude.is_some() {
            Some(Arc::new(ClaudeFactChecker::new(Arc::clone(&self.setup))))
        } else if detection.codex.is_some() {
            Some(Arc::new(CodexFactChecker::new(Arc::clone(&self.setup))))
        } else {
            None
        }
    }

    /// One line on [`Registry::fact_checker`], for the UI.
    pub fn fact_checker_status(&self) -> String {
        match self.fact_checker() {
            Some(checker) => format!("Checks facts with {}", checker.name()),
            None => "Install Claude Code or Codex to fact-check text with ⇧⌘B".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{
        detect::{FoundTool, FoundVia},
        scripted::ScriptedHarness,
    };

    fn tool(path: &str) -> Option<FoundTool> {
        Some(FoundTool {
            path: path.into(),
            version: Some("1.0".into()),
            via: FoundVia::Path,
        })
    }

    fn registry(detection: Detection) -> (tempfile::TempDir, Registry) {
        let temp = tempfile::tempdir().expect("tempdir");
        let registry = Registry::with_builtins(Setup::with_detection(temp.path(), detection));
        (temp, registry)
    }

    #[test]
    fn builtins_in_picker_order() {
        let (_temp, registry) = registry(Detection::default());
        let ids: Vec<String> = registry.infos().into_iter().map(|info| info.id.0).collect();
        assert_eq!(ids, vec!["sorcar", "claude-code", "codex"]);
        assert!(registry.get(&HarnessId::codex()).is_some());
        assert!(registry.get(&HarnessId::new("nope")).is_none());
    }

    #[test]
    fn default_prefers_sorcar_then_claude_then_codex() {
        // Claude installed, uv missing: Sorcar sets itself up, so it wins.
        let (_t, claude_only) = registry(Detection {
            claude: tool("/x/claude"),
            ..Detection::default()
        });
        assert_eq!(claude_only.default_id(), HarnessId::sorcar());

        // Nothing at all: Sorcar, which tells the user what to install.
        let (_t, nothing) = registry(Detection::default());
        assert_eq!(nothing.default_id(), HarnessId::sorcar());
        assert!(nothing.infos().iter().all(|info| !info.is_usable()));
    }

    #[test]
    fn default_falls_back_when_sorcar_cannot_run() {
        let (_t, registry) = registry(Detection {
            claude: tool("/x/claude"),
            ..Detection::default()
        });
        let unavailable = HarnessInfo {
            readiness: Readiness::Unavailable,
            ..ScriptedHarness::from_events("sorcar", Vec::new(), Duration::ZERO).info()
        };
        registry.register(Arc::new(ScriptedHarness::new(unavailable, Vec::new())));
        assert_eq!(registry.default_id(), HarnessId::claude_code());
        assert_eq!(registry.harnesses().len(), 3, "replaced in place");
        assert_eq!(registry.infos()[0].id, HarnessId::sorcar());
    }

    #[test]
    fn register_adds_a_new_harness() {
        let (_t, registry) = registry(Detection::default());
        registry.register(Arc::new(ScriptedHarness::demo(Duration::ZERO)));
        assert_eq!(registry.harnesses().len(), 4);
        // Nothing built in can run, the scripted one can.
        assert_eq!(registry.default_id(), HarnessId::new("scripted"));
    }

    #[test]
    fn rewriter_and_fact_checker_follow_what_is_installed() {
        let (_t, claude) = registry(Detection {
            claude: tool("/x/claude"),
            codex: tool("/x/codex"),
            ..Detection::default()
        });
        assert_eq!(
            claude.rewriter().map(|r| r.name()).as_deref(),
            Some("Claude Code (Haiku)")
        );
        assert_eq!(
            claude.fact_checker().map(|c| c.name()).as_deref(),
            Some("Claude Code (Sonnet, web search)")
        );
        assert_eq!(
            claude.rewriter_status(),
            "Polishes text with Claude Code (Haiku)"
        );
        let (_t, codex) = registry(Detection {
            codex: tool("/x/codex"),
            ..Detection::default()
        });
        assert_eq!(codex.rewriter().map(|r| r.name()).as_deref(), Some("Codex"));
        assert_eq!(
            codex.fact_checker().map(|c| c.name()).as_deref(),
            Some("Codex (web search)")
        );
        let (_t, nothing) = registry(Detection::default());
        assert!(nothing.rewriter().is_none() && nothing.fact_checker().is_none());
        assert!(
            nothing
                .rewriter_status()
                .contains("Install Claude Code or Codex")
        );
        assert!(nothing.fact_checker_status().contains("⇧⌘B"));
    }

    #[test]
    fn real_detection_runs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let registry = Registry::detect(temp.path());
        assert_eq!(registry.infos().len(), 3);
        assert!(!registry.detection().search_path.is_empty());
        let refreshed = registry.refresh();
        assert_eq!(
            refreshed.claude.is_some(),
            registry.detection().claude.is_some()
        );
    }
}
