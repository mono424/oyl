//! Finding a LaTeX toolchain on a machine where the app may have been
//! launched from the Dock, with almost nothing on `PATH`.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

/// The tool Oyl drives for a LaTeX compile, in order of preference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LatexTool {
    /// tectonic: self-contained, fetches packages, runs BibTeX itself.
    Tectonic(PathBuf),
    /// latexmk: runs the engine and BibTeX/Biber as often as needed.
    Latexmk(PathBuf),
    /// A bare engine (pdflatex); Oyl runs BibTeX and the reruns.
    Engine(PathBuf),
}

impl LatexTool {
    /// The executable.
    pub(crate) fn path(&self) -> &Path {
        match self {
            LatexTool::Tectonic(path) | LatexTool::Latexmk(path) | LatexTool::Engine(path) => path,
        }
    }

    /// "latexmk at /path/latexmk", for the toolchain status.
    pub(crate) fn describe(&self) -> String {
        let name = match self {
            LatexTool::Tectonic(_) => "tectonic",
            LatexTool::Latexmk(_) => "latexmk",
            LatexTool::Engine(path) => path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("pdflatex"),
        };
        format!("{name} at {}", self.path().display())
    }
}

/// Directories searched for TeX executables: `PATH` first (the user's own
/// choice), then the usual install locations, then Oyl's own `bin/`.
pub(crate) fn search_dirs(data_dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect())
        .unwrap_or_default();
    let home = dirs::home_dir().unwrap_or_default();
    let known = [
        PathBuf::from("/Library/TeX/texbin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".nix-profile/bin"),
        PathBuf::from("/run/current-system/sw/bin"),
        PathBuf::from("/nix/var/nix/profiles/default/bin"),
        PathBuf::from("/opt/local/bin"),
        PathBuf::from("/usr/texbin"),
        home.join(".cargo/bin"),
        home.join(".local/bin"),
        home.join("bin"),
        PathBuf::from("/usr/bin"),
        data_dir.join("bin"),
    ];
    for dir in known {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs.retain(|dir| dir.is_absolute());
    dirs
}

/// Whether `path` is an executable file.
fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The first executable called `name` in `dirs`.
pub(crate) fn find_in(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    dirs.iter()
        .map(|dir| dir.join(&file))
        .find(|path| is_executable(path))
}

/// The preferred LaTeX tool on this machine.
pub(crate) fn detect(data_dir: &Path) -> Option<LatexTool> {
    let dirs = search_dirs(data_dir);
    if let Some(path) = find_in(&dirs, "tectonic") {
        return Some(LatexTool::Tectonic(path));
    }
    if let Some(path) = find_in(&dirs, "latexmk") {
        return Some(LatexTool::Latexmk(path));
    }
    find_in(&dirs, "pdflatex").map(LatexTool::Engine)
}

/// A `PATH` for child processes: the tool's own directory, then the search
/// directories, then whatever the app inherited.
pub(crate) fn child_path(tool: &Path, data_dir: &Path) -> OsString {
    let mut dirs = Vec::new();
    if let Some(parent) = tool.parent() {
        dirs.push(parent.to_path_buf());
    }
    for dir in search_dirs(data_dir) {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    env::join_paths(dirs).unwrap_or_else(|_| env::var_os("PATH").unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_executables_in_the_data_dir() {
        let data = tempfile::tempdir().unwrap();
        let bin = data.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("oyl-test-tool");
        fs::write(&fake, "#!/bin/sh\n").unwrap();
        let dirs = search_dirs(data.path());
        assert_eq!(dirs.last(), Some(&bin));
        assert_eq!(
            find_in(&dirs, "oyl-test-tool"),
            None,
            "not executable yet"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(find_in(&dirs, "oyl-test-tool"), Some(fake));
        }
    }

    #[test]
    fn describes_tools() {
        let tool = LatexTool::Engine(PathBuf::from("/usr/bin/pdflatex"));
        assert_eq!(tool.describe(), "pdflatex at /usr/bin/pdflatex");
        let tool = LatexTool::Latexmk(PathBuf::from("/x/latexmk"));
        assert_eq!(tool.describe(), "latexmk at /x/latexmk");
    }
}
