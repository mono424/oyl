//! The few things Oyl remembers between launches, as JSON in its data
//! folder. Every field has a default, and a file written by an older or
//! newer Oyl — or edited by hand — loads with whatever it got right.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use oyl_model::Engine;
use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, Result},
    export::write_file_atomic,
    paths,
};

/// Light or dark, or whatever macOS is set to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Follow macOS.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

/// The layouts of the window, one per tab of the header's switch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayoutMode {
    /// The source alone, across the window.
    Write,
    /// Source on the left, the paper on the right.
    #[default]
    Split,
    /// The paper alone.
    Read,
    /// The agent on the left, the paper on the right.
    Agent,
}

/// Oyl's settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Light, dark or the system's.
    pub appearance: Appearance,
    /// Write, split, read or agent.
    pub layout_mode: LayoutMode,
    /// Share of the window width given to the source in split mode.
    pub split_ratio: f32,
    /// Whether the agent thread is open above the composer.
    pub chat_open: bool,
    /// Share of the left column's height given to the agent thread.
    pub chat_ratio: f32,
    /// Engine of new papers.
    pub default_engine: Engine,
    /// The harness picked in the composer; `None` uses the default.
    pub harness: Option<String>,
    /// The model picked for each harness: harness id → model id.
    pub models: BTreeMap<String, String>,
    /// Source font size, in points.
    pub editor_font_size: f32,
    /// Preview zoom; `None` fits the page width.
    pub preview_zoom: Option<f32>,
    /// The paper open when Oyl last quit.
    pub last_project: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance::System,
            layout_mode: LayoutMode::Split,
            split_ratio: 0.5,
            chat_open: false,
            chat_ratio: 0.4,
            default_engine: Engine::Typst,
            harness: None,
            models: BTreeMap::new(),
            editor_font_size: 14.0,
            preview_zoom: None,
            last_project: None,
        }
    }
}

impl Settings {
    /// Loads the settings from Oyl's data folder. Never fails; see
    /// [`Settings::load_from`].
    pub fn load() -> Settings {
        Self::load_from(&paths::data_dir())
    }

    /// Loads the settings kept in `data_dir`. A missing or unreadable file
    /// gives the defaults; a field that is missing or cannot be read keeps
    /// its default while the others load; values out of range are brought
    /// back into range.
    pub fn load_from(data_dir: &Path) -> Settings {
        let file = paths::settings_path(data_dir);
        let text = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(err) => {
                if err.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("could not read settings {}: {err}", file.display());
                }
                return Settings::default();
            }
        };
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(serde_json::Value::Object(fields)) => Self::from_fields(fields),
            Ok(_) | Err(_) => {
                log::warn!("ignoring unreadable settings {}", file.display());
                Settings::default()
            }
        }
    }

    /// Builds settings field by field over the defaults, keeping each field
    /// that reads and dropping each that does not.
    fn from_fields(fields: serde_json::Map<String, serde_json::Value>) -> Settings {
        let serde_json::Value::Object(mut merged) =
            serde_json::to_value(Settings::default()).unwrap_or_default()
        else {
            return Settings::default();
        };
        for (key, value) in fields {
            if !merged.contains_key(&key) {
                continue;
            }
            let mut candidate = merged.clone();
            candidate.insert(key.clone(), value.clone());
            if serde_json::from_value::<Settings>(serde_json::Value::Object(candidate)).is_ok() {
                merged.insert(key, value);
            } else {
                log::warn!("ignoring the unreadable setting `{key}`");
            }
        }
        serde_json::from_value::<Settings>(serde_json::Value::Object(merged))
            .unwrap_or_default()
            .sanitized()
    }

    /// Writes the settings to Oyl's data folder.
    pub fn save(&self) -> Result<()> {
        self.save_to(&paths::data_dir())
    }

    /// Writes the settings into `data_dir` (atomically).
    pub fn save_to(&self, data_dir: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(|err| Error::Serialize {
            what: "the settings",
            message: err.to_string(),
        })?;
        write_file_atomic(&paths::settings_path(data_dir), text)
    }

    /// The model picked for a harness.
    pub fn model_for(&self, harness: &str) -> Option<&str> {
        self.models.get(harness).map(String::as_str)
    }

    /// Picks a model for a harness; `None` goes back to its default.
    pub fn set_model(&mut self, harness: &str, model: Option<&str>) {
        match model {
            Some(model) => {
                self.models.insert(harness.to_owned(), model.to_owned());
            }
            None => {
                self.models.remove(harness);
            }
        }
    }

    /// The settings with every number brought into a usable range.
    pub fn sanitized(mut self) -> Settings {
        let defaults = Settings::default();
        let clamp = |value: f32, min: f32, max: f32, default: f32| {
            if value.is_finite() {
                value.clamp(min, max)
            } else {
                default
            }
        };
        self.split_ratio = clamp(self.split_ratio, 0.2, 0.8, defaults.split_ratio);
        self.chat_ratio = clamp(self.chat_ratio, 0.15, 0.85, defaults.chat_ratio);
        self.editor_font_size = clamp(self.editor_font_size, 9.0, 32.0, defaults.editor_font_size);
        self.preview_zoom = self
            .preview_zoom
            .filter(|zoom| zoom.is_finite())
            .map(|zoom| zoom.clamp(0.25, 8.0));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let settings = Settings::default();
        assert_eq!(settings.layout_mode, LayoutMode::Split);
        assert_eq!(settings.appearance, Appearance::System);
        assert_eq!(settings.split_ratio, 0.5);
        assert_eq!(settings.default_engine, Engine::Typst);
        assert_eq!(settings.editor_font_size, 14.0);
        assert_eq!(settings.harness, None);
    }

    #[test]
    fn missing_or_corrupt_files_give_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Settings::load_from(dir.path()), Settings::default());
        fs::write(paths::settings_path(dir.path()), "[1, 2").unwrap();
        assert_eq!(Settings::load_from(dir.path()), Settings::default());
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = Settings {
            appearance: Appearance::Dark,
            layout_mode: LayoutMode::Read,
            split_ratio: 0.6,
            chat_open: true,
            harness: Some("claude-code".into()),
            preview_zoom: Some(1.5),
            last_project: Some(PathBuf::from("/papers/x")),
            ..Settings::default()
        };
        settings.set_model("sorcar", Some("cc/opus"));
        settings.save_to(dir.path()).unwrap();
        let loaded = Settings::load_from(dir.path());
        assert_eq!(loaded, settings);
        assert_eq!(loaded.model_for("sorcar"), Some("cc/opus"));
        assert_eq!(loaded.model_for("codex"), None);
    }

    #[test]
    fn old_files_and_bad_fields_keep_what_reads() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            paths::settings_path(dir.path()),
            r#"{
                "appearance": "dark",
                "layout_mode": "sideways",
                "split_ratio": 7.0,
                "editor_font_size": "big",
                "some_future_setting": true
            }"#,
        )
        .unwrap();
        let loaded = Settings::load_from(dir.path());
        assert_eq!(loaded.appearance, Appearance::Dark);
        assert_eq!(loaded.layout_mode, LayoutMode::Split);
        assert_eq!(loaded.split_ratio, 0.8);
        assert_eq!(loaded.editor_font_size, 14.0);
    }
}
