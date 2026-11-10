//! Finding the agent CLIs on this machine.
//!
//! An app launched from the Dock gets almost no `PATH` (`/usr/bin:/bin:…`),
//! so `claude`, `codex` and `uv` are looked for in three places, in order:
//! the process `PATH`, the usual install locations (Homebrew, `~/.local/bin`,
//! npm / pnpm / volta / nvm global bins, Nix, Cargo, Bun, Oyl's own
//! `<data_dir>/bin`), and finally the user's login shell, asked for its
//! `PATH` with a short timeout. The same search path becomes the `PATH` of
//! every child process, so an agent can also find `node`, `typst`,
//! `latexmk`, … the way it would in the user's terminal.

use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use parking_lot::RwLock;

use crate::process::run_captured;

/// The provider API keys Sorcar can use instead of a CLI subscription, in the
/// order Sorcar prefers them. Only their names are ever reported.
pub const PROVIDER_API_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "OPENROUTER_API_KEY",
    "TOGETHER_API_KEY",
    "ZAI_API_KEY",
    "MOONSHOT_API_KEY",
];

/// How long the login shell may take to report its `PATH`.
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(4);
/// How long `<tool> --version` may take.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
/// Environment variables that a Claude Code session sets for itself; a child
/// `claude` that inherited them would believe it is nested in that session.
const KEPT_CLAUDE_CODE_VARS: &[&str] = &[
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];
const LOGIN_PATH_MARKER: &str = "__OYL_PATH__=";

/// Where a tool was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoundVia {
    /// On the process `PATH`.
    Path,
    /// In one of the usual install locations.
    KnownLocation,
    /// On the `PATH` of the user's login shell.
    LoginShell,
}

/// One CLI found on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundTool {
    /// Absolute path of the executable.
    pub path: PathBuf,
    /// The version it reports (`2.1.321`), when `--version` answered.
    pub version: Option<String>,
    /// Where it was found.
    pub via: FoundVia,
}

impl FoundTool {
    /// "claude 2.1.321 at ~/.local/bin", for a harness status line.
    pub fn describe(&self, name: &str) -> String {
        let dir = self.path.parent().map(display_home).unwrap_or_default();
        match &self.version {
            Some(version) => format!("{name} {version} at {dir}"),
            None => format!("{name} at {dir}"),
        }
    }
}

/// What is installed: the agent CLIs, uv, the provider keys, and the search
/// path child processes get.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Detection {
    /// The `claude` CLI (Claude Code).
    pub claude: Option<FoundTool>,
    /// The `codex` CLI.
    pub codex: Option<FoundTool>,
    /// `uv`, which runs Sorcar's Python environment.
    pub uv: Option<FoundTool>,
    /// Names of the provider API keys set in the environment (never values).
    pub api_keys: Vec<String>,
    /// The `PATH` handed to child processes, most specific first.
    pub search_path: Vec<PathBuf>,
}

impl Detection {
    /// Look for everything, with `data_dir` as Oyl's own data folder (its
    /// `bin/` is searched too, which is where Oyl installs uv).
    pub fn detect(data_dir: &Path) -> Detection {
        detect_with(&Probe::from_process(data_dir))
    }

    /// Whether any provider API key is set.
    pub fn has_api_key(&self) -> bool {
        !self.api_keys.is_empty()
    }

    /// Whether the named provider API key is set.
    pub fn has_key(&self, name: &str) -> bool {
        self.api_keys.iter().any(|key| key == name)
    }

    /// The `PATH` value for child processes.
    pub fn child_path(&self) -> OsString {
        std::env::join_paths(&self.search_path).unwrap_or_else(|_| {
            // A directory containing ':' cannot be expressed; drop such entries.
            let usable: Vec<&PathBuf> = self
                .search_path
                .iter()
                .filter(|dir| !dir.as_os_str().to_string_lossy().contains(':'))
                .collect();
            std::env::join_paths(usable).unwrap_or_default()
        })
    }

    /// The full environment for a child process: this process's environment
    /// without the variables of an enclosing Claude Code session, with
    /// `PATH` replaced by [`Detection::child_path`].
    pub fn child_env(&self) -> Vec<(OsString, OsString)> {
        let mut env: Vec<(OsString, OsString)> = std::env::vars_os()
            .filter(|(name, _)| !is_scrubbed(name) && name != "PATH")
            .collect();
        env.push(("PATH".into(), self.child_path()));
        env
    }
}

/// Whether a variable is left out of every child's environment.
pub(crate) fn is_scrubbed(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    name == "CLAUDECODE"
        || name == "CLAUDE_PID"
        || name == "CLAUDE_EFFORT"
        || name.starts_with("CLAUDE_AGENT_SDK")
        || name.starts_with("CLAUDE_PREVIEW_")
        || (name.starts_with("CLAUDE_CODE_") && !KEPT_CLAUDE_CODE_VARS.contains(&name))
}

/// Detection that can be refreshed while harnesses keep a handle on it.
///
/// Every built-in harness holds an `Arc<Setup>`; [`Setup::refresh`] re-runs
/// detection and all of them see the result on their next `info()`.
#[derive(Debug)]
pub struct Setup {
    data_dir: PathBuf,
    detection: RwLock<Arc<Detection>>,
}

impl Setup {
    /// Detect what is installed, with `data_dir` as Oyl's data folder.
    pub fn detect(data_dir: impl Into<PathBuf>) -> Arc<Setup> {
        let data_dir = data_dir.into();
        let detection = Detection::detect(&data_dir);
        Self::with_detection(data_dir, detection)
    }

    /// A setup with a detection made elsewhere (tests, UI development).
    pub fn with_detection(data_dir: impl Into<PathBuf>, detection: Detection) -> Arc<Setup> {
        Arc::new(Setup {
            data_dir: data_dir.into(),
            detection: RwLock::new(Arc::new(detection)),
        })
    }

    /// Oyl's data folder.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// The current detection.
    pub fn detection(&self) -> Arc<Detection> {
        self.detection.read().clone()
    }

    /// Detect again (after an install, or when the user asks) and return
    /// the new detection.
    pub fn refresh(&self) -> Arc<Detection> {
        let fresh = Arc::new(Detection::detect(&self.data_dir));
        *self.detection.write() = fresh.clone();
        fresh
    }
}

/// Everything detection reads from the outside world, so tests can fake it.
#[derive(Clone, Debug)]
pub(crate) struct Probe {
    pub path_var: Option<OsString>,
    pub home: Option<PathBuf>,
    pub shell: Option<PathBuf>,
    pub data_dir: PathBuf,
    pub login_shell_timeout: Duration,
    pub api_keys: Vec<String>,
}

impl Probe {
    fn from_process(data_dir: &Path) -> Probe {
        Probe {
            path_var: std::env::var_os("PATH"),
            home: dirs::home_dir(),
            shell: std::env::var_os("SHELL")
                .filter(|shell| !shell.is_empty())
                .map(PathBuf::from)
                .or_else(|| Some(PathBuf::from("/bin/zsh"))),
            data_dir: data_dir.to_path_buf(),
            login_shell_timeout: LOGIN_SHELL_TIMEOUT,
            api_keys: PROVIDER_API_KEYS
                .iter()
                .filter(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
                .map(|name| (*name).to_owned())
                .collect(),
        }
    }
}

const TOOLS: [&str; 3] = ["claude", "codex", "uv"];

pub(crate) fn detect_with(probe: &Probe) -> Detection {
    let path_dirs: Vec<PathBuf> = probe
        .path_var
        .as_deref()
        .map(|path| std::env::split_paths(path).collect())
        .unwrap_or_default();
    let known = known_locations(probe.home.as_deref(), &probe.data_dir);

    let mut found: [Option<(PathBuf, FoundVia)>; 3] = Default::default();
    for (slot, name) in found.iter_mut().zip(TOOLS) {
        *slot = find_in(name, &path_dirs)
            .map(|path| (path, FoundVia::Path))
            .or_else(|| {
                find_known(name, &known, probe.home.as_deref())
                    .map(|p| (p, FoundVia::KnownLocation))
            });
    }

    // The login shell knows what the user's terminal knows. Asked only when
    // something is still missing or the process PATH is the bare one an app
    // gets from launchd.
    let bare_path = path_dirs.len() < 6;
    let mut login_dirs = Vec::new();
    if found.iter().any(Option::is_none) || bare_path {
        if let Some(shell) = &probe.shell {
            login_dirs = login_shell_path(shell, probe.login_shell_timeout);
        }
        for (slot, name) in found.iter_mut().zip(TOOLS) {
            if slot.is_none() {
                *slot = find_in(name, &login_dirs).map(|path| (path, FoundVia::LoginShell));
            }
        }
    }

    let search_path = search_path(&found, &path_dirs, &login_dirs, &known);
    let env_path = std::env::join_paths(&search_path).unwrap_or_default();

    // Versions are asked in parallel: each `--version` starts a runtime.
    let versions: Vec<Option<String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = found
            .iter()
            .map(|slot| {
                let env_path = env_path.clone();
                scope.spawn(move || {
                    slot.as_ref()
                        .and_then(|(path, _)| tool_version(path, &env_path))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or(None))
            .collect()
    });

    let mut tools = found
        .into_iter()
        .zip(versions)
        .map(|(slot, version)| slot.map(|(path, via)| FoundTool { path, version, via }));
    Detection {
        claude: tools.next().flatten(),
        codex: tools.next().flatten(),
        uv: tools.next().flatten(),
        api_keys: probe.api_keys.clone(),
        search_path,
    }
}

/// The usual places CLIs are installed in, most common first.
pub(crate) fn known_locations(home: Option<&Path>, data_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".claude/local"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    if let Some(home) = home {
        dirs.push(home.join(".nix-profile/bin"));
    }
    dirs.push(PathBuf::from("/nix/var/nix/profiles/default/bin"));
    dirs.push(PathBuf::from("/run/current-system/sw/bin"));
    if let Some(home) = home {
        for relative in [
            ".cargo/bin",
            ".bun/bin",
            ".npm-global/bin",
            ".npm-packages/bin",
            ".npm/bin",
            "Library/pnpm",
            ".local/share/pnpm",
            ".pnpm",
            ".volta/bin",
            ".yarn/bin",
            ".config/yarn/global/node_modules/.bin",
            ".asdf/shims",
            ".local/share/mise/shims",
        ] {
            dirs.push(home.join(relative));
        }
        dirs.extend(nvm_bins(home));
    }
    dirs.push(PathBuf::from("/opt/local/bin"));
    dirs.push(data_dir.join("bin"));
    dirs
}

/// `~/.nvm/versions/node/*/bin`, newest Node first.
fn nvm_bins(home: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(home.join(".nvm/versions/node")) else {
        return Vec::new();
    };
    let mut versions: Vec<(Vec<u64>, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let key = name
                .trim_start_matches('v')
                .split('.')
                .map(|part| part.parse::<u64>().unwrap_or(0))
                .collect();
            (key, entry.path().join("bin"))
        })
        .collect();
    versions.sort_by(|a, b| b.0.cmp(&a.0));
    versions.into_iter().map(|(_, dir)| dir).collect()
}

/// Search `dirs` for an executable called `name`.
pub(crate) fn find_in(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join(name))
        .find(|path| is_executable(path))
}

fn find_known(name: &str, known: &[PathBuf], home: Option<&Path>) -> Option<PathBuf> {
    find_in(name, known).or_else(|| {
        if name != "codex" {
            return None;
        }
        // The Codex desktop app bundles the CLI.
        let mut bundles = vec![PathBuf::from(
            "/Applications/Codex.app/Contents/Resources/codex",
        )];
        if let Some(home) = home {
            bundles.push(home.join("Applications/Codex.app/Contents/Resources/codex"));
        }
        bundles.into_iter().find(|path| is_executable(path))
    })
}

pub(crate) fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// The child `PATH`: the found tools' directories (so an npm-installed CLI
/// finds its `node`), then the process `PATH`, the login shell's `PATH`, the
/// usual locations that exist, TeX and the system directories.
fn search_path(
    found: &[Option<(PathBuf, FoundVia)>],
    path_dirs: &[PathBuf],
    login_dirs: &[PathBuf],
    known: &[PathBuf],
) -> Vec<PathBuf> {
    let tool_dirs = found
        .iter()
        .flatten()
        .filter_map(|(path, _)| path.parent().map(Path::to_path_buf));
    let system = [
        "/Library/TeX/texbin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ]
    .map(PathBuf::from);
    let mut seen = HashSet::new();
    tool_dirs
        .chain(path_dirs.iter().cloned())
        .chain(login_dirs.iter().cloned())
        .chain(known.iter().filter(|dir| dir.is_dir()).cloned())
        .chain(system.into_iter().filter(|dir| dir.is_dir()))
        .filter(|dir| dir.is_absolute() && seen.insert(dir.clone()))
        .collect()
}

/// Ask the login shell for its `PATH`. Interactive and login, because
/// `~/.zshrc` is where nvm and friends add themselves. `printf` and a quoted
/// `$PATH` mean the same in zsh, bash and fish.
fn login_shell_path(shell: &Path, timeout: Duration) -> Vec<PathBuf> {
    let script = format!("printf '\\n{LOGIN_PATH_MARKER}%s\\n' \"$PATH\"");
    let args: Vec<OsString> = vec!["-l".into(), "-i".into(), "-c".into(), script.into()];
    let env: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(name, _)| !is_scrubbed(name))
        .collect();
    match run_captured(shell, &args, &env, timeout) {
        Some(output) => parse_login_path(&output),
        None => {
            log::info!(
                "login shell {} did not report its PATH in time",
                shell.display()
            );
            Vec::new()
        }
    }
}

pub(crate) fn parse_login_path(output: &str) -> Vec<PathBuf> {
    output
        .lines()
        .filter_map(|line| line.split_once(LOGIN_PATH_MARKER).map(|(_, rest)| rest))
        .next_back()
        .map(|path| {
            path.trim()
                .split(':')
                .filter(|dir| dir.starts_with('/'))
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

fn tool_version(path: &Path, env_path: &OsStr) -> Option<String> {
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(name, _)| !is_scrubbed(name) && name != "PATH")
        .collect();
    env.push(("PATH".into(), env_path.to_os_string()));
    let output = run_captured(path, &["--version".into()], &env, VERSION_TIMEOUT)?;
    parse_version(&output)
}

/// The first version-looking token: `2.1.321 (Claude Code)` → `2.1.321`,
/// `uv 0.8.15 (…)` → `0.8.15`, `codex-cli 0.46.0` → `0.46.0`.
pub(crate) fn parse_version(output: &str) -> Option<String> {
    output.split_whitespace().find_map(|token| {
        let token = token
            .trim_start_matches('v')
            .trim_end_matches([',', ';', ')']);
        let first = token.chars().next()?;
        (first.is_ascii_digit() && token.contains('.')).then(|| token.to_owned())
    })
}

/// `~/…` for paths under the home directory.
pub(crate) fn display_home(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() {
            "~".to_owned()
        } else {
            format!("~/{}", rest.display())
        };
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_tool(dir: &Path, name: &str, version_line: &str) -> PathBuf {
        std::fs::create_dir_all(dir).expect("create dir");
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\necho '{version_line}'\n")).expect("write tool");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    fn probe(path_var: &str, home: &Path, data_dir: &Path, shell: Option<PathBuf>) -> Probe {
        Probe {
            path_var: Some(path_var.into()),
            home: Some(home.to_path_buf()),
            shell,
            data_dir: data_dir.to_path_buf(),
            login_shell_timeout: Duration::from_secs(3),
            api_keys: vec!["OPENAI_API_KEY".into()],
        }
    }

    #[test]
    fn versions_are_parsed() {
        assert_eq!(
            parse_version("2.1.321 (Claude Code)").as_deref(),
            Some("2.1.321")
        );
        assert_eq!(
            parse_version("uv 0.8.15 (8473ecba1 2025-09-03)").as_deref(),
            Some("0.8.15")
        );
        assert_eq!(
            parse_version("codex-cli 0.46.0\n").as_deref(),
            Some("0.46.0")
        );
        assert_eq!(parse_version("tool v1.2"), Some("1.2".into()));
        assert_eq!(parse_version("no version here"), None);
    }

    #[test]
    fn claude_session_variables_are_scrubbed() {
        assert!(is_scrubbed(OsStr::new("CLAUDECODE")));
        assert!(is_scrubbed(OsStr::new("CLAUDE_CODE_ENTRYPOINT")));
        assert!(is_scrubbed(OsStr::new("CLAUDE_CODE_SESSION_ID")));
        assert!(is_scrubbed(OsStr::new("CLAUDE_AGENT_SDK_VERSION")));
        assert!(!is_scrubbed(OsStr::new("CLAUDE_CODE_OAUTH_TOKEN")));
        assert!(!is_scrubbed(OsStr::new("CLAUDE_CONFIG_DIR")));
        assert!(!is_scrubbed(OsStr::new("ANTHROPIC_API_KEY")));
        assert!(!is_scrubbed(OsStr::new("HOME")));
    }

    #[test]
    fn child_env_replaces_path_and_drops_session_vars() {
        let detection = Detection {
            search_path: vec![PathBuf::from("/opt/x/bin"), PathBuf::from("/usr/bin")],
            ..Detection::default()
        };
        let env = detection.child_env();
        let path: Vec<_> = env.iter().filter(|(name, _)| name == "PATH").collect();
        assert_eq!(path.len(), 1);
        assert_eq!(path[0].1, OsString::from("/opt/x/bin:/usr/bin"));
        assert!(env.iter().all(|(name, _)| name != "CLAUDECODE"));
    }

    #[test]
    fn login_shell_output_is_parsed_through_noise() {
        let output = "Welcome!\n\x1b[1mprompt\x1b[0m\n__OYL_PATH__=/a/bin:/b/bin:relative:/c\n";
        assert_eq!(
            parse_login_path(output),
            vec![
                PathBuf::from("/a/bin"),
                PathBuf::from("/b/bin"),
                PathBuf::from("/c")
            ]
        );
        assert!(parse_login_path("nothing").is_empty());
    }

    #[test]
    fn path_wins_over_known_locations() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let on_path = temp.path().join("path-bin");
        fake_tool(&on_path, "claude", "9.9.9 (Claude Code)");
        fake_tool(&home.join(".local/bin"), "claude", "1.0.0 (Claude Code)");
        fake_tool(&home.join(".local/bin"), "uv", "uv 0.8.15 (abc)");
        let codex = fake_tool(
            &home.join(".nvm/versions/node/v22.1.0/bin"),
            "codex",
            "codex-cli 0.50.0",
        );
        fake_tool(
            &home.join(".nvm/versions/node/v9.0.0/bin"),
            "codex",
            "codex-cli 0.1.0",
        );
        let data = temp.path().join("data");
        let path_var = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin:/x:/y", on_path.display());
        let detection = detect_with(&probe(&path_var, &home, &data, None));

        let claude = detection.claude.expect("claude found");
        assert_eq!(claude.path, on_path.join("claude"));
        assert_eq!(claude.via, FoundVia::Path);
        assert_eq!(claude.version.as_deref(), Some("9.9.9"));
        let uv = detection.uv.expect("uv found");
        assert_eq!(uv.via, FoundVia::KnownLocation);
        assert_eq!(uv.version.as_deref(), Some("0.8.15"));
        let found_codex = detection.codex.expect("codex found");
        assert_eq!(found_codex.path, codex, "newest nvm node wins");
        assert_eq!(found_codex.version.as_deref(), Some("0.50.0"));
        assert_eq!(detection.api_keys, vec!["OPENAI_API_KEY".to_owned()]);
        // The tools' directories lead the child PATH.
        assert_eq!(detection.search_path[0], on_path);
    }

    #[test]
    fn data_dir_bin_is_searched() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data = temp.path().join("data");
        let uv = fake_tool(&data.join("bin"), "uv", "uv 0.9.0");
        let home = temp.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let detection = detect_with(&probe("/usr/bin:/bin:/a:/b:/c:/d", &home, &data, None));
        assert_eq!(detection.uv.map(|tool| tool.path), Some(uv));
    }

    #[test]
    fn login_shell_path_finds_the_rest() {
        let temp = tempfile::tempdir().expect("tempdir");
        let hidden = temp.path().join("only-in-shell");
        let codex = fake_tool(&hidden, "codex", "codex-cli 0.60.1");
        // A fake "login shell" that ignores its flags and prints a PATH.
        let shell = temp.path().join("fakesh");
        std::fs::write(
            &shell,
            format!(
                "#!/bin/sh\necho 'motd noise'\nprintf '__OYL_PATH__=%s\\n' '{}:/usr/bin'\n",
                hidden.display()
            ),
        )
        .expect("write shell");
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let home = temp.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let detection = detect_with(&probe(
            "/usr/bin:/bin",
            &home,
            &temp.path().join("data"),
            Some(shell),
        ));
        let found = detection.codex.expect("codex via login shell");
        assert_eq!(found.path, codex);
        assert_eq!(found.via, FoundVia::LoginShell);
        assert!(detection.search_path.contains(&hidden));
    }

    #[test]
    fn describe_abbreviates_home() {
        let Some(home) = dirs::home_dir() else { return };
        let tool = FoundTool {
            path: home.join(".local/bin/claude"),
            version: Some("2.1.321".into()),
            via: FoundVia::Path,
        };
        assert_eq!(tool.describe("claude"), "claude 2.1.321 at ~/.local/bin");
    }
}
