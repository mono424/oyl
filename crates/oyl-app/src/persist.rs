//! Where the app keeps its state, and the two small files it keeps besides
//! `oyl_core`'s settings and library: the window's frame, and which
//! history version each agent turn made.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Overrides Oyl's data folder (settings, library, compiler caches, agent
/// environments). Tests and `--self-test` always set it.
pub const DATA_DIR_ENV: &str = "OYL_DATA_DIR";

/// The window's frame, next to the settings.
const WINDOW_FILE: &str = "window.json";

/// Turn id → version id, in the paper's `.oyl/`.
const TURNS_FILE: &str = "agent-turns.json";
const BRANCHES_FILE: &str = "agent-branches.json";

/// Where the app reads and writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppPaths {
    /// Settings, library, caches, agent environments.
    pub data_dir: PathBuf,
    /// Where new papers go until the user picks another folder.
    pub default_papers_dir: PathBuf,
}

impl AppPaths {
    /// The real locations, or `$OYL_DATA_DIR` (with papers in its
    /// `Papers/`) when it is set, so a run with it never touches the
    /// user's own folders.
    pub fn from_env() -> Self {
        match std::env::var_os(DATA_DIR_ENV).filter(|dir| !dir.is_empty()) {
            Some(dir) => Self::in_dir(PathBuf::from(dir)),
            None => Self {
                data_dir: oyl_core::paths::data_dir(),
                default_papers_dir: oyl_core::paths::default_papers_dir(),
            },
        }
    }

    /// Everything inside one folder (tests, the self-test).
    pub fn in_dir(dir: PathBuf) -> Self {
        let dir = std::path::absolute(&dir).unwrap_or(dir);
        Self {
            default_papers_dir: dir.join(oyl_core::paths::PAPERS_DIR_NAME),
            data_dir: dir,
        }
    }

    fn window_file(&self) -> PathBuf {
        self.data_dir.join(WINDOW_FILE)
    }
}

/// How the window was left.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowFrame {
    /// Left edge, in screen points.
    pub x: f32,
    /// Top edge, in screen points.
    pub y: f32,
    /// Width in points.
    pub width: f32,
    /// Height in points.
    pub height: f32,
    /// Zoomed to fill the screen (its restore size is the frame).
    #[serde(default)]
    pub maximized: bool,
    /// In full screen (its restore size is the frame).
    #[serde(default)]
    pub fullscreen: bool,
}

impl WindowFrame {
    /// The frame saved by the last run, if it reads and makes sense.
    pub fn load(paths: &AppPaths) -> Option<WindowFrame> {
        let text = fs::read_to_string(paths.window_file()).ok()?;
        let frame: WindowFrame = serde_json::from_str(&text).ok()?;
        frame.is_sane().then_some(frame)
    }

    /// Writes the frame (atomically).
    pub fn save(&self, paths: &AppPaths) -> anyhow::Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        oyl_core::write_file_atomic(&paths.window_file(), text)?;
        Ok(())
    }

    /// Finite numbers and a size the window can have.
    pub fn is_sane(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|value| value.is_finite())
            && self.width >= crate::layout::WINDOW_MIN.0
            && self.height >= crate::layout::WINDOW_MIN.1
            && self.width <= 20_000.
            && self.height <= 20_000.
    }
}

/// Which history version each agent turn of a paper made, so "Undo" works
/// after a restart too.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnVersions {
    /// Turn id → version id (its `Display` form).
    pub turns: BTreeMap<String, String>,
}

impl TurnVersions {
    fn file(root: &Path) -> PathBuf {
        root.join(oyl_core::project::STATE_DIR).join(TURNS_FILE)
    }

    /// The paper's record; empty when there is none or it does not read.
    pub fn load(root: &Path) -> TurnVersions {
        fs::read_to_string(Self::file(root))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Writes the record (atomically).
    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        oyl_core::write_file_atomic(&Self::file(root), text)?;
        Ok(())
    }
}

/// Which thread (and turn) each agent branch of a paper belongs to, so a
/// review or an unfinished branch finds its thread after a restart.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchThreads {
    /// Branch id (its `Display` form) → its thread.
    pub branches: BTreeMap<String, BranchThread>,
}

/// The thread and turn of a branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchThread {
    /// The chat thread id.
    pub thread: String,
    /// The turn that made the branch.
    pub turn: String,
}

impl BranchThreads {
    fn file(root: &Path) -> PathBuf {
        root.join(oyl_core::project::STATE_DIR).join(BRANCHES_FILE)
    }

    /// The paper's record; empty when there is none or it does not read.
    pub fn load(root: &Path) -> BranchThreads {
        fs::read_to_string(Self::file(root))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Writes the record (atomically).
    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        oyl_core::write_file_atomic(&Self::file(root), text)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_threads_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(BranchThreads::load(dir.path()), BranchThreads::default());
        let mut record = BranchThreads::default();
        record.branches.insert(
            "b1".into(),
            BranchThread {
                thread: "t1".into(),
                turn: "u1".into(),
            },
        );
        record.save(dir.path()).unwrap();
        assert_eq!(BranchThreads::load(dir.path()), record);
    }

    #[test]
    fn a_data_dir_override_keeps_papers_inside_it() {
        let paths = AppPaths::in_dir("/tmp/oyl-test".into());
        assert_eq!(paths.data_dir, Path::new("/tmp/oyl-test"));
        assert_eq!(paths.default_papers_dir, Path::new("/tmp/oyl-test/Papers"));
    }

    #[test]
    fn the_window_frame_round_trips_and_rejects_nonsense() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::in_dir(dir.path().to_path_buf());
        assert_eq!(WindowFrame::load(&paths), None);
        let frame = WindowFrame {
            x: 40.,
            y: 60.,
            width: 1200.,
            height: 800.,
            maximized: false,
            fullscreen: false,
        };
        frame.save(&paths).unwrap();
        assert_eq!(WindowFrame::load(&paths), Some(frame));
        let tiny = WindowFrame {
            width: 10.,
            ..frame
        };
        tiny.save(&paths).unwrap();
        assert_eq!(WindowFrame::load(&paths), None);
        fs::write(paths.window_file(), "{ not json").unwrap();
        assert_eq!(WindowFrame::load(&paths), None);
    }

    #[test]
    fn turn_versions_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(TurnVersions::load(dir.path()), TurnVersions::default());
        let mut record = TurnVersions::default();
        record.turns.insert("turn-1".into(), "abc".into());
        record.save(dir.path()).unwrap();
        assert_eq!(TurnVersions::load(dir.path()), record);
    }
}
