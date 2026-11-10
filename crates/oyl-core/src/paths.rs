//! Where Oyl keeps its own files, and where papers go by default.
//!
//! Everything that reads or writes these locations also has a variant that
//! takes the base folder explicitly, so tests (and the app, if it wants to)
//! can point Oyl somewhere else.

use std::path::{Path, PathBuf};

/// Name of Oyl's folder inside the platform's application-support folder.
pub const APP_DIR_NAME: &str = "Oyl";

/// The app-support folder's name before the product was called Oyl.
const LEGACY_APP_DIR_NAME: &str = "Quire";

/// Name of the default papers folder inside the home folder.
pub const PAPERS_DIR_NAME: &str = "Papers";

/// File name of the library (the list of known papers) in the data folder.
pub const LIBRARY_FILE: &str = "library.json";

/// File name of the settings in the data folder.
pub const SETTINGS_FILE: &str = "settings.json";

/// Oyl's own data folder: `~/Library/Application Support/Oyl` on macOS.
///
/// Falls back to `~/.oyl-data`, then to a folder in the temporary
/// directory, when the platform cannot say where application data goes.
pub fn data_dir() -> PathBuf {
    if let Some(base) = dirs::data_dir() {
        let dir = base.join(APP_DIR_NAME);
        // Carry over what the app kept before it was called Oyl.
        let legacy = base.join(LEGACY_APP_DIR_NAME);
        if !dir.exists()
            && legacy.is_dir()
            && let Err(err) = std::fs::rename(&legacy, &dir)
        {
            log::warn!(
                "could not move {} to {}: {err}",
                legacy.display(),
                dir.display()
            );
        }
        return dir;
    }
    if let Some(home) = dirs::home_dir() {
        log::warn!("no application-support folder; keeping Oyl's data in the home folder");
        return home.join(".oyl-data");
    }
    log::warn!("no home folder; keeping Oyl's data in the temporary folder");
    std::env::temp_dir().join(APP_DIR_NAME)
}

/// Where new papers are created unless the user picked another folder:
/// `~/Papers`.
pub fn default_papers_dir() -> PathBuf {
    match dirs::home_dir() {
        Some(home) => default_papers_dir_in(&home),
        None => data_dir().join(PAPERS_DIR_NAME),
    }
}

/// The default papers folder for the given home folder.
pub fn default_papers_dir_in(home: &Path) -> PathBuf {
    home.join(PAPERS_DIR_NAME)
}

/// The library file inside a data folder.
pub fn library_path(data_dir: &Path) -> PathBuf {
    data_dir.join(LIBRARY_FILE)
}

/// The settings file inside a data folder.
pub fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SETTINGS_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_are_under_their_base() {
        let base = Path::new("/base");
        assert_eq!(library_path(base), Path::new("/base/library.json"));
        assert_eq!(settings_path(base), Path::new("/base/settings.json"));
        assert_eq!(default_papers_dir_in(base), Path::new("/base/Papers"));
    }

    #[test]
    fn data_dir_is_named_after_the_app() {
        let dir = data_dir();
        assert!(dir.is_absolute());
        assert!(dir.ends_with(APP_DIR_NAME) || dir.ends_with(".oyl-data"));
    }
}
