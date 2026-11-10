//! The Sorcar harness, Oyl's default: KISS Sorcar
//! (<https://github.com/ksenxx/kiss_ai>, PyPI `kiss-agent-framework`) run
//! through a small Python bridge.
//!
//! Sorcar lives in a persistent virtual environment,
//! `<data_dir>/sorcar/venv`, built once by uv (`uv venv --python 3.13`, then
//! `uv pip install kiss-agent-framework==…`). A turn runs
//! `<venv>/bin/python -I -u <data_dir>/bridge/sorcar_bridge.py` directly —
//! no uv in the turn's path — with the request as JSON on stdin and one JSON
//! event per line on stdout.
//!
//! Setup (also available ahead of time through [`Harness::prepare`]) makes
//! sure the environment exists for the pinned release, rebuilding it when it
//! is missing, broken or for another version, under one lock per data folder
//! so a turn never starts a second build. Where things live:
//! - `<data_dir>/kiss/` is Sorcar's `KISS_HOME` (chat history, which resumes
//!   conversations) — always Oyl's own.
//! - With the user's uv, uv's own cache and Python downloads are used, so
//!   Sorcar's packages are shared with whatever the user already has.
//! - When Oyl installed uv itself (into `<data_dir>/bin`), its cache and
//!   Python downloads stay in `<data_dir>/uv/` too.
//!
//! Sorcar edits the paper folder in place: the bridge gives it no
//! `create_worktree` / `commit_and_merge` tools (which also switches off its
//! worktree write guard) and removes the worktree rules from its system
//! prompt.

use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, mpsc},
    time::Duration,
};

use oyl_model::agent::{
    AgentEvent, Harness, HarnessId, HarnessInfo, ModelChoice, Readiness, TurnHandle, TurnRequest,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    detect::{Detection, Setup, is_executable},
    process::{self, STOPPED_SUMMARY, Spawn, StreamParser, Turn},
    prompt, tools,
};

/// The Sorcar release the bridge is written and verified against.
pub const SORCAR_PACKAGE: &str = "kiss-agent-framework==2026.11.7";
/// The Python Sorcar runs on.
pub const PYTHON_VERSION: &str = "3.13";
/// The bridge script, written to `<data_dir>/bridge/` before each turn.
pub const BRIDGE_SOURCE: &str = include_str!("sorcar_bridge.py");
const BRIDGE_FILE: &str = "sorcar_bridge.py";
/// Written into the environment once it is complete.
const RECORD_FILE: &str = ".oyl-sorcar.json";
/// The official standalone uv installer.
const UV_INSTALLER_URL: &str = "https://astral.sh/uv/install.sh";
const UV_INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
/// How long creating the environment may take (it may download Python).
const VENV_TIMEOUT: Duration = Duration::from_secs(600);
/// How long installing Sorcar may take (a first download).
const INSTALL_TIMEOUT: Duration = Duration::from_secs(1800);
/// How long importing Sorcar in the environment may take.
const CHECK_TIMEOUT: Duration = Duration::from_secs(120);
/// How often a turn waiting for the setup lock looks at its Stop button.
const LOCK_POLL: Duration = Duration::from_millis(200);

/// KISS Sorcar, Oyl's default harness.
pub struct SorcarHarness {
    setup: Arc<Setup>,
}

impl SorcarHarness {
    /// A harness that runs Sorcar with what `setup` found.
    pub fn new(setup: Arc<Setup>) -> Self {
        Self { setup }
    }

    fn layout(&self) -> Layout {
        Layout {
            data_dir: self.setup.data_dir().to_path_buf(),
        }
    }
}

/// Where Sorcar's files live inside Oyl's data folder.
#[derive(Clone, Debug)]
pub(crate) struct Layout {
    pub data_dir: PathBuf,
}

impl Layout {
    pub fn bridge(&self) -> PathBuf {
        self.data_dir.join("bridge").join(BRIDGE_FILE)
    }
    pub fn kiss_home(&self) -> PathBuf {
        self.data_dir.join("kiss")
    }
    pub fn uv_cache(&self) -> PathBuf {
        self.data_dir.join("uv").join("cache")
    }
    pub fn uv_python(&self) -> PathBuf {
        self.data_dir.join("uv").join("python")
    }
    pub fn bin(&self) -> PathBuf {
        self.data_dir.join("bin")
    }
    /// Sorcar's virtual environment.
    pub fn venv(&self) -> PathBuf {
        self.data_dir.join("sorcar").join("venv")
    }
    /// The environment's interpreter, which every turn runs.
    pub fn python(&self) -> PathBuf {
        self.venv().join("bin").join("python")
    }
    /// What was installed, written last when the environment was built.
    pub fn record(&self) -> PathBuf {
        self.venv().join(RECORD_FILE)
    }
    /// A file only a complete install of Sorcar has.
    fn package_init(&self) -> PathBuf {
        self.venv()
            .join("lib")
            .join(format!("python{PYTHON_VERSION}"))
            .join("site-packages")
            .join("kiss")
            .join("__init__.py")
    }
}

/// What a finished environment holds, as recorded in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EnvRecord {
    /// The requirement installed ([`SORCAR_PACKAGE`]).
    pub package: String,
    /// The version Sorcar reported when it was imported.
    pub version: Option<String>,
    /// The Python it was built for ([`PYTHON_VERSION`]).
    pub python: String,
}

/// Whether the environment is complete and for the pinned release: the
/// record matches, the interpreter runs and Sorcar is installed. Cheap
/// (three file checks), so every turn asks.
pub(crate) fn env_is_current(layout: &Layout) -> bool {
    let Ok(text) = std::fs::read_to_string(layout.record()) else {
        return false;
    };
    let Ok(record) = serde_json::from_str::<EnvRecord>(&text) else {
        return false;
    };
    record.package == SORCAR_PACKAGE
        && record.python == PYTHON_VERSION
        && is_executable(&layout.python())
        && layout.package_init().is_file()
}

/// Whose cache uv uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UvCache {
    /// The user's own uv: its default cache and Python downloads, shared
    /// with everything else the user runs through uv.
    Shared,
    /// The uv Oyl installed: cache and Python downloads under
    /// `<data_dir>/uv`.
    Private,
}

/// The uv that builds the environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Uv {
    pub path: PathBuf,
    pub cache: UvCache,
}

/// The uv to use: the one detection found (the user's, unless it is the one
/// Oyl installed into `<data_dir>/bin`), else Oyl's own if it exists.
pub(crate) fn choose_uv(detection: &Detection, layout: &Layout) -> Option<Uv> {
    let own_bin = layout.bin();
    let canonical_bin = std::fs::canonicalize(&own_bin).ok();
    let is_oyls = |path: &Path| {
        path.starts_with(&own_bin)
            || canonical_bin
                .as_ref()
                .is_some_and(|bin| std::fs::canonicalize(path).is_ok_and(|p| p.starts_with(bin)))
    };
    if let Some(found) = &detection.uv {
        let cache = if is_oyls(&found.path) {
            UvCache::Private
        } else {
            UvCache::Shared
        };
        return Some(Uv {
            path: found.path.clone(),
            cache,
        });
    }
    let own = own_bin.join("uv");
    is_executable(&own).then_some(Uv {
        path: own,
        cache: UvCache::Private,
    })
}

/// The environment of Sorcar's Python: `base` (the child environment)
/// without a virtualenv or Python path the app may have been started with,
/// and with Sorcar's own `KISS_HOME`.
pub(crate) fn sorcar_env(
    base: Vec<(OsString, OsString)>,
    layout: &Layout,
) -> Vec<(OsString, OsString)> {
    with_vars(
        base,
        vec![
            ("KISS_HOME", layout.kiss_home().into_os_string()),
            ("PYTHONUNBUFFERED", "1".into()),
            ("PYTHONIOENCODING", "utf-8".into()),
        ],
    )
}

/// The environment of a uv command: as [`sorcar_env`], plus — for Oyl's
/// private uv only — uv's cache and Python downloads in the data folder. The
/// user's uv keeps its own defaults (and any `UV_CACHE_DIR` they set).
pub(crate) fn uv_env(
    base: Vec<(OsString, OsString)>,
    layout: &Layout,
    cache: UvCache,
) -> Vec<(OsString, OsString)> {
    let mut set = Vec::new();
    if cache == UvCache::Private {
        set.push(("UV_CACHE_DIR", layout.uv_cache().into_os_string()));
        set.push(("UV_PYTHON_INSTALL_DIR", layout.uv_python().into_os_string()));
    }
    with_vars(sorcar_env(base, layout), set)
}

fn with_vars(
    base: Vec<(OsString, OsString)>,
    set: Vec<(&str, OsString)>,
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = base
        .into_iter()
        .filter(|(name, _)| {
            let name = name.to_str().unwrap_or_default();
            !matches!(
                name,
                "VIRTUAL_ENV"
                    | "CONDA_PREFIX"
                    | "PYTHONPATH"
                    | "PYTHONHOME"
                    | "UV_PROJECT_ENVIRONMENT"
            ) && !set.iter().any(|(ours, _)| *ours == name)
        })
        .collect();
    env.extend(set.into_iter().map(|(name, value)| (name.into(), value)));
    env
}

/// One setup lock per data folder, shared by `prepare` and every turn.
pub(crate) fn setup_lock(data_dir: &Path) -> Arc<Mutex<()>> {
    static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> =
        LazyLock::new(Default::default);
    let key = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    Arc::clone(LOCKS.lock().entry(key).or_default())
}

/// The models Sorcar can use here and the one it uses by default:
/// `cc/opus` through the Claude subscription when `claude` exists, else
/// `codex/default` through Codex, else Sorcar's own default for the first
/// provider API key present.
pub fn sorcar_models(detection: &Detection) -> (Vec<ModelChoice>, Option<String>) {
    let mut models = Vec::new();
    let mut push = |id: &str, label: &str| {
        models.push(ModelChoice {
            id: id.into(),
            label: label.into(),
        })
    };
    if detection.claude.is_some() {
        push("cc/opus", "Claude Opus (your Claude subscription)");
        push("cc/sonnet", "Claude Sonnet (your Claude subscription)");
        push(
            "cc/haiku",
            "Claude Haiku (your Claude subscription, fastest)",
        );
    }
    if detection.codex.is_some() {
        push(
            "codex/default",
            "Codex default model (your ChatGPT subscription)",
        );
    }
    const KEYED: &[(&str, &str, &str)] = &[
        (
            "ANTHROPIC_API_KEY",
            "claude-opus-5-5",
            "Claude Opus 5.5 (Anthropic API key)",
        ),
        (
            "ANTHROPIC_API_KEY",
            "claude-sonnet-5-5",
            "Claude Sonnet 5.5 (Anthropic API key)",
        ),
        (
            "OPENAI_API_KEY",
            "gpt-6.1-sol-medium",
            "GPT-6.1 Sol (OpenAI API key)",
        ),
        (
            "OPENAI_API_KEY",
            "gpt-6-luna",
            "GPT-6 Luna (OpenAI API key)",
        ),
        (
            "GEMINI_API_KEY",
            "gemini-3.8-flash",
            "Gemini 3.8 Flash (Gemini API key)",
        ),
        (
            "OPENROUTER_API_KEY",
            "openrouter/anthropic/claude-opus-5.5",
            "Claude Opus 5.5 (OpenRouter)",
        ),
        (
            "TOGETHER_API_KEY",
            "moonshotai/Kimi-K3",
            "Kimi K3 (Together AI)",
        ),
        ("ZAI_API_KEY", "glm-4.7", "GLM-4.7 (Z.AI)"),
        ("MOONSHOT_API_KEY", "kimi-k3", "Kimi K3 (Moonshot)"),
    ];
    for (key, id, label) in KEYED {
        if detection.has_key(key) {
            push(id, label);
        }
    }
    let default = models.first().map(|model| model.id.clone());
    (models, default)
}

impl Harness for SorcarHarness {
    fn info(&self) -> HarnessInfo {
        let detection = self.setup.detection();
        let layout = self.layout();
        let (models, default_model) = sorcar_models(&detection);
        let route = if let Some(claude) = &detection.claude {
            Some(format!("through {}", claude.describe("claude")))
        } else if let Some(codex) = &detection.codex {
            Some(format!("through {}", codex.describe("codex")))
        } else {
            detection.api_keys.first().map(|key| format!("with {key}"))
        };
        let (readiness, status) = match route {
            None => (
                Readiness::Unavailable,
                "Install Claude Code or Codex, or set a provider API key, to use Sorcar".to_owned(),
            ),
            Some(route) if env_is_current(&layout) => (Readiness::Ready, format!("Runs {route}")),
            Some(route) if choose_uv(&detection, &layout).is_some() => (
                Readiness::Ready,
                format!("Runs {route}; the first turn downloads Sorcar (a few minutes)"),
            ),
            Some(route) => (
                Readiness::SetsUpOnFirstUse,
                format!("Sets itself up on first use (installs uv and Sorcar), then runs {route}"),
            ),
        };
        HarnessInfo {
            id: HarnessId::sorcar(),
            name: "Sorcar".into(),
            tagline: "KISS Sorcar: long-horizon agent that checks its own work".into(),
            readiness,
            status,
            models,
            default_model,
        }
    }

    fn start(&self, request: TurnRequest) -> anyhow::Result<TurnHandle> {
        let info = self.info();
        let detection = self.setup.detection();
        let layout = self.layout();
        process::begin("oyl-sorcar-turn", move |turn| {
            run_sorcar_turn(turn, &info, &detection, &layout, request)
        })
    }

    /// Install uv when missing, write the bridge and build Sorcar's
    /// environment, so the first turn starts at once. Blocking; holds the
    /// data folder's setup lock, which turns wait for. When the environment
    /// is already there it only imports Sorcar in it (well under a second).
    fn prepare(&self, progress: &dyn Fn(&str)) -> anyhow::Result<()> {
        let info = self.info();
        if info.readiness == Readiness::Unavailable {
            anyhow::bail!("{}", info.status);
        }
        let detection = self.setup.detection();
        let layout = self.layout();
        let lock = setup_lock(&layout.data_dir);
        let _guard = lock.lock();
        let (tx, rx) = mpsc::channel::<String>();
        let outcome = std::thread::scope(|scope| {
            let (detection, layout) = (&detection, &layout);
            let worker = scope.spawn(move || {
                let mut turn = Turn::detached(tx);
                ensure_ready(&mut turn, detection, layout, true)
            });
            for message in rx {
                progress(&message);
            }
            worker.join()
        });
        match outcome {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(message)) => anyhow::bail!("Sorcar could not be set up: {message}"),
            Err(_) => anyhow::bail!("Sorcar's setup crashed"),
        }
    }
}

/// Make Sorcar runnable and return its interpreter: the bridge on disk and
/// the environment built for the pinned release (with uv, installed into
/// `<data_dir>/bin` when there is none). The caller holds the setup lock.
/// With `verify`, a current environment is also checked by importing Sorcar
/// in it, and rebuilt when that fails; a turn relies on the file checks.
fn ensure_ready(
    turn: &mut Turn,
    detection: &Detection,
    layout: &Layout,
    verify: bool,
) -> Result<PathBuf, String> {
    write_bridge(layout).map_err(|err| {
        format!(
            "could not write the bridge in {}: {err}",
            layout.data_dir.display()
        )
    })?;
    if env_is_current(layout) {
        if !verify || check_env(turn, detection, layout).is_ok() {
            return Ok(layout.python());
        }
        log::info!("Sorcar's environment no longer imports; building it again");
    }
    if turn.is_cancelled() {
        return Err(STOPPED_SUMMARY.into());
    }
    let uv = match choose_uv(detection, layout) {
        Some(uv) => uv,
        None => Uv {
            path: install_uv(turn, layout, &detection.child_env())?,
            cache: UvCache::Private,
        },
    };
    turn.status("Setting up Sorcar… the first run downloads its Python environment, which takes a few minutes.");
    build_env(turn, detection, layout, &uv)?;
    turn.status("Sorcar is ready.");
    Ok(layout.python())
}

/// Build the environment from scratch: `uv venv`, `uv pip install`, an
/// import check, and the record last.
fn build_env(
    turn: &mut Turn,
    detection: &Detection,
    layout: &Layout,
    uv: &Uv,
) -> Result<(), String> {
    let venv = layout.venv();
    if venv.exists() {
        std::fs::remove_dir_all(&venv).map_err(|err| {
            format!(
                "could not remove the old environment {}: {err}",
                venv.display()
            )
        })?;
    }
    if let Some(parent) = venv.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }
    let env = uv_env(detection.child_env(), layout, uv.cache);
    let uv_command = |args: Vec<OsString>| Spawn {
        program: uv.path.clone(),
        args,
        cwd: layout.data_dir.clone(),
        env: env.clone(),
        stdin: Vec::new(),
    };

    turn.status("Creating Sorcar's Python environment…");
    let mut progress = SetupProgress::default();
    turn.run_helper(
        &uv_command(uv_venv_args(&venv)),
        VENV_TIMEOUT,
        &mut |line| progress.line(line),
    )
    .map_err(|err| format!("creating the Python {PYTHON_VERSION} environment failed: {err}"))?;
    turn.run_helper(
        &uv_command(uv_pip_args(&layout.python())),
        INSTALL_TIMEOUT,
        &mut |line| progress.line(line),
    )
    .map_err(|err| format!("installing {SORCAR_PACKAGE} failed: {err}"))?;
    let version = check_env(turn, detection, layout)?;
    let record = EnvRecord {
        package: SORCAR_PACKAGE.into(),
        version,
        python: PYTHON_VERSION.into(),
    };
    let json = serde_json::to_string_pretty(&record).map_err(|err| err.to_string())?;
    std::fs::write(layout.record(), json)
        .map_err(|err| format!("could not record the environment: {err}"))
}

/// Import Sorcar in the environment (`python -I -u <bridge> --check`) and
/// return the version it reports.
fn check_env(
    turn: &mut Turn,
    detection: &Detection,
    layout: &Layout,
) -> Result<Option<String>, String> {
    let spawn = Spawn {
        program: layout.python(),
        args: python_args(&layout.bridge(), true),
        cwd: layout.data_dir.clone(),
        env: sorcar_env(detection.child_env(), layout),
        stdin: Vec::new(),
    };
    let mut version = None;
    let mut ready = false;
    turn.run_helper(&spawn, CHECK_TIMEOUT, &mut |line| {
        if let Ok(BridgeEvent::Ready { version: reported }) =
            serde_json::from_str::<BridgeEvent>(line)
        {
            ready = true;
            version = reported;
        }
        None
    })
    .map_err(|err| format!("Sorcar does not start in its environment: {err}"))?;
    if ready {
        Ok(version)
    } else {
        Err("Sorcar's environment did not report ready".into())
    }
}

/// `uv venv` for Sorcar's environment.
pub(crate) fn uv_venv_args(venv: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["venv", "--no-project", "--python", PYTHON_VERSION]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.push(venv.into());
    args
}

/// `uv pip install` of the pinned Sorcar into the environment.
pub(crate) fn uv_pip_args(python: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["pip", "install", "--python"]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.push(python.into());
    args.push(SORCAR_PACKAGE.into());
    args
}

/// The interpreter's arguments for the bridge (`--check` only imports).
pub(crate) fn python_args(bridge: &Path, check: bool) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["-I".into(), "-u".into(), bridge.into()];
    if check {
        args.push("--check".into());
    }
    args
}

/// Turns uv's output while it builds Sorcar's environment into short human
/// lines.
#[derive(Debug, Default)]
pub(crate) struct SetupProgress {
    announced: bool,
    started: usize,
    finished: usize,
}

impl SetupProgress {
    /// The status line for one line of uv output, if it says anything.
    pub fn line(&mut self, line: &str) -> Option<String> {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Resolved ") {
            let count = rest.split_whitespace().next().unwrap_or_default();
            self.announced = true;
            return Some(format!("Downloading Sorcar ({count} packages)…"));
        }
        if let Some(rest) = line.strip_prefix("Downloading ") {
            if rest.starts_with("cpython") {
                return Some(format!("Downloading Python {PYTHON_VERSION}…"));
            }
            if rest.ends_with(')') {
                // "Downloading numpy (4.9MiB)": a large download begins.
                self.started += 1;
                if self.announced {
                    return None;
                }
                self.announced = true;
                return Some("Downloading Sorcar…".to_owned());
            }
            // " Downloading numpy" (older uv): that download is done.
            return Some(self.finished_one());
        }
        if line.starts_with("Downloaded ") {
            return Some(self.finished_one());
        }
        if let Some(rest) = line.strip_prefix("Installed ") {
            let count = rest.split_whitespace().next().unwrap_or_default();
            return Some(format!("Installed Sorcar ({count} packages)."));
        }
        if let Some(rest) = line.strip_prefix("Building ") {
            return Some(format!("Building {rest}…"));
        }
        None
    }

    fn finished_one(&mut self) -> String {
        self.finished += 1;
        let total = self.started.max(self.finished);
        format!(
            "Downloading Sorcar… {} of {total} large packages",
            self.finished
        )
    }
}

/// What the bridge reads on stdin.
#[derive(Debug, Serialize)]
pub(crate) struct BridgeRequest {
    /// Paper context, writing rules and the message.
    pub prompt: String,
    /// The message alone, as Sorcar's chat history records it.
    pub history_prompt: String,
    pub model: Option<String>,
    pub work_dir: PathBuf,
    pub active_file: Option<PathBuf>,
    pub session: Option<String>,
    pub attachments: Vec<PathBuf>,
}

impl BridgeRequest {
    pub fn new(request: &TurnRequest, model: Option<String>) -> Self {
        BridgeRequest {
            prompt: prompt::full_prompt(request),
            history_prompt: request.prompt.trim().to_owned(),
            model,
            work_dir: request.project_root.clone(),
            active_file: request.active_file.clone(),
            session: request.session.clone().filter(|s| !s.is_empty()),
            attachments: request.attachments.clone(),
        }
    }
}

fn run_sorcar_turn(
    turn: &mut Turn,
    info: &HarnessInfo,
    detection: &Detection,
    layout: &Layout,
    request: TurnRequest,
) {
    if info.readiness == Readiness::Unavailable {
        turn.fail(info.status.clone());
        return;
    }
    if !request.project_root.is_dir() {
        turn.fail(format!(
            "The paper folder {} does not exist.",
            request.project_root.display()
        ));
        return;
    }

    // Setup happens once per data folder: wait for a `prepare` (or another
    // turn) that is setting Sorcar up instead of building it twice.
    let lock = setup_lock(&layout.data_dir);
    let mut waiting = false;
    let guard = loop {
        if let Some(guard) = lock.try_lock_for(LOCK_POLL) {
            break guard;
        }
        if turn.is_cancelled() {
            turn.stopped(request.session.clone());
            return;
        }
        if !waiting {
            waiting = true;
            turn.status("Waiting for Sorcar's setup to finish…");
        }
    };
    let ready = ensure_ready(turn, detection, layout, false);
    drop(guard);
    let python = match ready {
        Ok(python) => python,
        Err(_) if turn.is_cancelled() => {
            turn.stopped(request.session.clone());
            return;
        }
        Err(err) => {
            turn.fail(format!("Sorcar could not be set up: {err}"));
            return;
        }
    };

    let model = request
        .model
        .clone()
        .filter(|m| !m.is_empty())
        .or_else(|| info.default_model.clone());
    let bridge_request = BridgeRequest::new(&request, model);
    let stdin = match serde_json::to_vec(&bridge_request) {
        Ok(bytes) => bytes,
        Err(err) => {
            turn.fail(format!("Could not encode the Sorcar request: {err}"));
            return;
        }
    };
    let spawn = Spawn {
        program: python,
        args: python_args(&layout.bridge(), false),
        cwd: request.project_root.clone(),
        env: sorcar_env(detection.child_env(), layout),
        stdin,
    };
    let mut parser = SorcarParser::new(request.project_root.clone(), request.session.clone());
    turn.run_agent(&spawn, &mut parser, "Sorcar");
}

/// Write the embedded bridge where the environment can run it, only when it
/// changed.
pub(crate) fn write_bridge(layout: &Layout) -> std::io::Result<()> {
    let path = layout.bridge();
    std::fs::create_dir_all(layout.kiss_home())?;
    if std::fs::read_to_string(&path).is_ok_and(|current| current == BRIDGE_SOURCE) {
        return Ok(());
    }
    let dir = path.parent().unwrap_or(&layout.data_dir);
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(format!(".{BRIDGE_FILE}.{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&temp, BRIDGE_SOURCE)?;
    std::fs::rename(&temp, &path)
}

/// The command that installs uv into `<data_dir>/bin` with the official
/// standalone installer, leaving the user's shell profiles alone.
pub(crate) fn uv_installer(layout: &Layout, env: &[(OsString, OsString)]) -> Spawn {
    let mut env = env.to_vec();
    env.retain(|(name, _)| {
        !matches!(
            name.to_str(),
            Some("UV_INSTALL_DIR" | "UV_NO_MODIFY_PATH" | "INSTALLER_NO_MODIFY_PATH")
        )
    });
    env.push(("UV_INSTALL_DIR".into(), layout.bin().into_os_string()));
    env.push(("UV_NO_MODIFY_PATH".into(), "1".into()));
    env.push(("INSTALLER_NO_MODIFY_PATH".into(), "1".into()));
    Spawn {
        program: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            format!("curl -LsSf {UV_INSTALLER_URL} | sh").into(),
        ],
        cwd: layout.data_dir.clone(),
        env,
        stdin: Vec::new(),
    }
}

fn install_uv(
    turn: &mut Turn,
    layout: &Layout,
    env: &[(OsString, OsString)],
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(layout.bin()).map_err(|err| err.to_string())?;
    turn.status("Installing uv, which sets up Sorcar…");
    turn.run_helper(
        &uv_installer(layout, env),
        UV_INSTALL_TIMEOUT,
        &mut |line| {
            let line = line.trim();
            (line.starts_with("downloading")
                || line.starts_with("installing")
                || line.starts_with("everything"))
            .then(|| line.to_owned())
        },
    )
    .map_err(|err| format!("installing uv failed: {err}"))?;
    let uv = layout.bin().join("uv");
    if is_executable(&uv) {
        turn.status("uv is installed.");
        Ok(uv)
    } else {
        Err(format!(
            "the uv installer finished but {} is missing",
            uv.display()
        ))
    }
}

/// One line of the bridge protocol (see `sorcar_bridge.py`).
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BridgeEvent {
    Ready {
        #[serde(default)]
        version: Option<String>,
    },
    Session {
        session: String,
    },
    Status {
        message: String,
    },
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    ToolStarted {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    ToolFinished {
        id: String,
        ok: bool,
        #[serde(default)]
        output: Option<String>,
    },
    Usage {
        #[serde(default)]
        cost_usd: Option<f64>,
        #[serde(default)]
        input_tokens: Option<u64>,
        #[serde(default)]
        output_tokens: Option<u64>,
    },
    Completed {
        success: bool,
        #[serde(default)]
        summary: String,
        #[serde(default)]
        session: Option<String>,
    },
    Failed {
        message: String,
    },
}

/// Maps the bridge's lines to agent events. Its stderr only feeds the
/// failure report.
pub(crate) struct SorcarParser {
    root: PathBuf,
    session: Option<String>,
    edits: HashMap<String, Vec<PathBuf>>,
}

impl SorcarParser {
    pub(crate) fn new(root: PathBuf, session: Option<String>) -> Self {
        Self {
            root,
            session: session.filter(|s| !s.is_empty()),
            edits: HashMap::new(),
        }
    }
}

impl StreamParser for SorcarParser {
    fn stdout_line(&mut self, line: &str, out: &mut Vec<AgentEvent>) {
        let Ok(event) = serde_json::from_str::<BridgeEvent>(line) else {
            if !line.trim().is_empty() {
                log::debug!("sorcar bridge: unexpected output: {line}");
            }
            return;
        };
        match event {
            BridgeEvent::Ready { .. } => {}
            BridgeEvent::Session { session } => {
                if !session.is_empty() {
                    self.session = Some(session);
                }
            }
            BridgeEvent::Status { message } => out.push(AgentEvent::Status { message }),
            BridgeEvent::TextDelta { text } => out.push(AgentEvent::TextDelta { text }),
            BridgeEvent::ThinkingDelta { text } => out.push(AgentEvent::ThinkingDelta { text }),
            BridgeEvent::ToolStarted { id, name, input } => {
                let view = tools::describe(&name, &input, &self.root);
                self.edits.insert(id.clone(), view.edits);
                out.push(AgentEvent::ToolStarted {
                    id,
                    name,
                    title: view.title,
                    detail: view.detail,
                });
            }
            BridgeEvent::ToolFinished { id, ok, output } => {
                let edits = self.edits.remove(&id).unwrap_or_default();
                out.push(AgentEvent::ToolFinished {
                    id,
                    ok,
                    output: output.filter(|o| !o.is_empty()),
                });
                if ok {
                    out.extend(
                        edits
                            .into_iter()
                            .map(|path| AgentEvent::FileChanged { path }),
                    );
                }
            }
            BridgeEvent::Usage {
                cost_usd,
                input_tokens,
                output_tokens,
            } => {
                out.push(AgentEvent::Usage {
                    cost_usd,
                    input_tokens,
                    output_tokens,
                });
            }
            BridgeEvent::Completed {
                success,
                summary,
                session,
            } => {
                if let Some(session) = session.filter(|s| !s.is_empty()) {
                    self.session = Some(session);
                }
                out.push(AgentEvent::Completed {
                    success,
                    summary,
                    session: self.session.clone(),
                });
            }
            BridgeEvent::Failed { message } => out.push(AgentEvent::Failed { message }),
        }
    }

    fn session(&self) -> Option<String> {
        self.session.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::detect::{FoundTool, FoundVia};
    use serde_json::json;

    fn tool(path: &str) -> Option<FoundTool> {
        Some(FoundTool {
            path: path.into(),
            version: Some("1.0".into()),
            via: FoundVia::Path,
        })
    }

    fn harness(detection: Detection, data: &Path) -> SorcarHarness {
        SorcarHarness::new(Setup::with_detection(data, detection))
    }

    fn env_value(env: &[(OsString, OsString)], name: &str) -> Option<OsString> {
        env.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())
    }

    fn executable(path: &Path, script: &str) {
        std::fs::write(path, script).expect("write script");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    /// A stand-in for uv: `venv` creates an environment whose `python` is a
    /// stand-in bridge (`--check` reports ready, anything else plays a short
    /// turn); `pip install` prints uv-like progress and installs the `kiss`
    /// package files. Every call (uv's and the environment's python's) is
    /// logged with the cache it was given.
    fn fake_uv(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).expect("dir");
        let log = dir.join("calls.log");
        let python = dir.join("fake-python");
        executable(
            &python,
            &format!(
                r#"#!/bin/sh
echo "python $* cache=${{UV_CACHE_DIR:-unset}} kiss=$KISS_HOME" >> "{log}"
case "$*" in
  *--check*) echo '{{"type":"ready","version":"2026.11.7"}}'; exit 0 ;;
esac
cat > /dev/null
echo '{{"type":"ready","version":"2026.11.7"}}'
echo '{{"type":"session","session":"chat-x"}}'
echo '{{"type":"completed","success":true,"summary":"done","session":"chat-x"}}'
"#,
                log = log.display()
            ),
        );
        let uv = dir.join("uv");
        executable(
            &uv,
            &format!(
                r#"#!/bin/sh
echo "uv $* cache=${{UV_CACHE_DIR:-unset}} kiss=$KISS_HOME" >> "{log}"
case "$1" in
  venv)
    for last in "$@"; do :; done
    mkdir -p "$last/bin" && cp "{python}" "$last/bin/python"
    echo "Using CPython 3.13.5" >&2 ;;
  pip)
    py="$4"; venv="$(dirname "$(dirname "$py")")"
    echo "Resolved 3 packages in 5ms" >&2
    echo "Downloading numpy (4.9MiB)" >&2
    echo " Downloaded numpy" >&2
    mkdir -p "$venv/lib/python3.13/site-packages/kiss" && touch "$venv/lib/python3.13/site-packages/kiss/__init__.py"
    echo "Installed 3 packages in 5ms" >&2
    echo " + numpy==2.0.0" >&2 ;;
esac
"#,
                log = log.display(),
                python = python.display()
            ),
        );
        uv
    }

    fn fake_setup(temp: &Path) -> (SorcarHarness, PathBuf, PathBuf) {
        let data = temp.join("data");
        let user_bin = temp.join("user-bin");
        let uv = fake_uv(&user_bin);
        let detection = Detection {
            claude: tool("/x/claude"),
            uv: Some(FoundTool {
                path: uv,
                version: Some("0.8.15".into()),
                via: FoundVia::Path,
            }),
            search_path: vec!["/usr/bin".into(), "/bin".into()],
            ..Detection::default()
        };
        (harness(detection, &data), data, user_bin.join("calls.log"))
    }

    fn calls(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn collect(handle: &TurnHandle) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        while let Ok(event) = handle.events.recv_blocking() {
            events.push(event);
        }
        events
    }

    #[test]
    fn default_model_follows_what_is_installed() {
        let claude = Detection {
            claude: tool("/x/claude"),
            codex: tool("/x/codex"),
            ..Detection::default()
        };
        let (models, default) = sorcar_models(&claude);
        assert_eq!(default.as_deref(), Some("cc/opus"));
        assert!(models.iter().any(|m| m.id == "cc/haiku"));
        assert!(models.iter().any(|m| m.id == "codex/default"));

        let codex = Detection {
            codex: tool("/x/codex"),
            api_keys: vec!["OPENAI_API_KEY".into()],
            ..Detection::default()
        };
        assert_eq!(sorcar_models(&codex).1.as_deref(), Some("codex/default"));

        let keys = Detection {
            api_keys: vec!["GEMINI_API_KEY".into(), "ANTHROPIC_API_KEY".into()],
            ..Detection::default()
        };
        assert_eq!(sorcar_models(&keys).1.as_deref(), Some("claude-opus-5-5"));

        assert_eq!(sorcar_models(&Detection::default()), (Vec::new(), None));
    }

    #[test]
    fn readiness_states() {
        let temp = tempfile::tempdir().expect("tempdir");
        let none = harness(Detection::default(), temp.path()).info();
        assert_eq!(none.readiness, Readiness::Unavailable);
        assert!(none.status.contains("Install Claude Code or Codex"));

        let no_uv = harness(
            Detection {
                claude: tool("/x/claude"),
                ..Detection::default()
            },
            temp.path(),
        )
        .info();
        assert_eq!(no_uv.readiness, Readiness::SetsUpOnFirstUse);
        assert_eq!(no_uv.default_model.as_deref(), Some("cc/opus"));

        let ready = harness(
            Detection {
                claude: tool("/x/claude"),
                uv: tool("/x/uv"),
                ..Detection::default()
            },
            temp.path(),
        )
        .info();
        assert_eq!(ready.readiness, Readiness::Ready);
        assert!(
            ready.status.contains("first turn downloads"),
            "{}",
            ready.status
        );
        assert!(ready.is_usable());
    }

    #[test]
    fn the_users_uv_shares_its_cache_oyls_own_keeps_it_private() {
        let temp = tempfile::tempdir().expect("tempdir");
        let layout = Layout {
            data_dir: temp.path().join("data"),
        };
        let base = vec![
            ("PATH".into(), "/usr/bin".into()),
            ("VIRTUAL_ENV".into(), "/some/venv".into()),
            ("KISS_HOME".into(), "/Users/me/.kiss".into()),
        ];

        // The user's uv: uv's own defaults, nothing overridden.
        let users = Detection {
            uv: tool("/Users/me/.local/bin/uv"),
            ..Detection::default()
        };
        let uv = choose_uv(&users, &layout).expect("uv");
        assert_eq!(
            uv,
            Uv {
                path: "/Users/me/.local/bin/uv".into(),
                cache: UvCache::Shared
            }
        );
        let env = uv_env(base.clone(), &layout, uv.cache);
        assert_eq!(env_value(&env, "UV_CACHE_DIR"), None);
        assert_eq!(env_value(&env, "UV_PYTHON_INSTALL_DIR"), None);
        assert_eq!(
            env_value(&env, "KISS_HOME"),
            Some(layout.kiss_home().into_os_string())
        );
        assert_eq!(env_value(&env, "VIRTUAL_ENV"), None);
        assert_eq!(env.iter().filter(|(n, _)| n == "KISS_HOME").count(), 1);
        // A cache the user configured for uv stays theirs.
        let mut configured = base.clone();
        configured.push(("UV_CACHE_DIR".into(), "/Volumes/big/uv".into()));
        let env = uv_env(configured, &layout, UvCache::Shared);
        assert_eq!(
            env_value(&env, "UV_CACHE_DIR"),
            Some("/Volumes/big/uv".into())
        );

        // Oyl's own uv, found by detection in <data_dir>/bin or only on disk.
        std::fs::create_dir_all(layout.bin()).expect("bin");
        let own = layout.bin().join("uv");
        executable(&own, "#!/bin/sh\n");
        let found_there = Detection {
            uv: Some(FoundTool {
                path: own.clone(),
                version: None,
                via: FoundVia::KnownLocation,
            }),
            ..Detection::default()
        };
        assert_eq!(
            choose_uv(&found_there, &layout).map(|uv| uv.cache),
            Some(UvCache::Private)
        );
        let uv = choose_uv(&Detection::default(), &layout).expect("Oyl's uv on disk");
        assert_eq!(
            uv,
            Uv {
                path: own,
                cache: UvCache::Private
            }
        );
        let env = uv_env(base.clone(), &layout, uv.cache);
        assert_eq!(
            env_value(&env, "UV_CACHE_DIR"),
            Some(layout.uv_cache().into_os_string())
        );
        assert_eq!(
            env_value(&env, "UV_PYTHON_INSTALL_DIR"),
            Some(layout.uv_python().into_os_string())
        );
        // Sorcar's own Python never gets uv's variables.
        let python_env = sorcar_env(base, &layout);
        assert_eq!(env_value(&python_env, "UV_CACHE_DIR"), None);
        assert_eq!(
            env_value(&python_env, "KISS_HOME"),
            Some(layout.kiss_home().into_os_string())
        );

        // Nothing at all: uv will be installed.
        let empty = Layout {
            data_dir: temp.path().join("empty"),
        };
        assert_eq!(choose_uv(&Detection::default(), &empty), None);
    }

    #[test]
    fn the_environment_record_decides_what_is_current() {
        let temp = tempfile::tempdir().expect("tempdir");
        let layout = Layout {
            data_dir: temp.path().to_path_buf(),
        };
        assert!(!env_is_current(&layout));
        std::fs::create_dir_all(layout.python().parent().expect("bin")).expect("bin");
        executable(&layout.python(), "#!/bin/sh\n");
        std::fs::create_dir_all(layout.package_init().parent().expect("kiss")).expect("kiss");
        std::fs::write(layout.package_init(), "").expect("init");
        let write = |package: &str, python: &str| {
            let record = EnvRecord {
                package: package.into(),
                version: Some("x".into()),
                python: python.into(),
            };
            std::fs::write(
                layout.record(),
                serde_json::to_string(&record).expect("json"),
            )
            .expect("record");
        };
        write(SORCAR_PACKAGE, PYTHON_VERSION);
        assert!(env_is_current(&layout));
        write("kiss-agent-framework==2026.1.1", PYTHON_VERSION);
        assert!(!env_is_current(&layout), "another pinned version");
        write(SORCAR_PACKAGE, "3.12");
        assert!(!env_is_current(&layout), "another Python");
        write(SORCAR_PACKAGE, PYTHON_VERSION);
        std::fs::remove_file(layout.package_init()).expect("remove");
        assert!(
            !env_is_current(&layout),
            "Sorcar missing from the environment"
        );
        std::fs::write(layout.record(), "not json").expect("garbage");
        assert!(!env_is_current(&layout));
    }

    #[test]
    fn one_setup_lock_per_data_folder() {
        let temp = tempfile::tempdir().expect("tempdir");
        let a = setup_lock(temp.path());
        let again = setup_lock(&temp.path().join("."));
        let b = setup_lock(&temp.path().join("other"));
        assert!(Arc::ptr_eq(&a, &again));
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn setup_progress_reads_uv() {
        let mut progress = SetupProgress::default();
        let lines: Vec<Option<String>> = [
            "Using CPython 3.13.5",
            "Downloading cpython-3.13.5-macos-aarch64-none (17.5MiB)",
            "Using Python 3.13.5 environment at: venv",
            "Resolved 163 packages in 1.2s",
            "Downloading numpy (4.9MiB)",
            "Downloading playwright (46.5MiB)",
            "   Building srt==3.5.3",
            " Downloaded numpy",
            "      Built srt==3.5.3",
            " Downloading playwright",
            "Prepared 163 packages in 30s",
            "Installed 163 packages in 344ms",
            " + numpy==2.4.0",
            "{\"type\":\"ready\"}",
        ]
        .into_iter()
        .map(|line| progress.line(line))
        .collect();
        assert_eq!(
            lines,
            vec![
                None,
                Some("Downloading Python 3.13…".into()),
                None,
                Some("Downloading Sorcar (163 packages)…".into()),
                None,
                None,
                Some("Building srt==3.5.3…".into()),
                Some("Downloading Sorcar… 1 of 2 large packages".into()),
                None,
                Some("Downloading Sorcar… 2 of 2 large packages".into()),
                None,
                Some("Installed Sorcar (163 packages).".into()),
                None,
                None,
            ]
        );
        let mut without_resolve = SetupProgress::default();
        assert_eq!(
            without_resolve.line("Downloading numpy (4.9MiB)"),
            Some("Downloading Sorcar…".into())
        );
    }

    #[test]
    fn prepare_builds_the_environment_once_and_turns_run_it_directly() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (sorcar, data, log) = fake_setup(temp.path());
        let layout = Layout {
            data_dir: data.clone(),
        };
        let paper = temp.path().join("paper");
        std::fs::create_dir_all(&paper).expect("paper");
        assert!(sorcar.info().status.contains("first turn downloads"));

        // First prepare builds the environment, with progress.
        let seen = std::cell::RefCell::new(Vec::new());
        sorcar
            .prepare(&|line| seen.borrow_mut().push(line.to_owned()))
            .expect("prepare");
        let seen = seen.into_inner();
        assert_eq!(
            seen,
            vec![
                "Setting up Sorcar… the first run downloads its Python environment, which takes a few minutes.",
                "Creating Sorcar's Python environment…",
                "Downloading Sorcar (3 packages)…",
                "Downloading Sorcar… 1 of 1 large packages",
                "Installed Sorcar (3 packages).",
                "Sorcar is ready.",
            ]
        );
        let first = calls(&log);
        assert_eq!(first.len(), 3, "{first:?}");
        assert!(
            first[0].starts_with("uv venv --no-project --python 3.13 ")
                && first[0].contains("sorcar/venv")
        );
        assert!(
            first[1].starts_with("uv pip install --python ") && first[1].contains(SORCAR_PACKAGE)
        );
        assert!(first[2].starts_with("python -I -u ") && first[2].contains("--check"));
        assert!(
            first.iter().all(|line| line.contains("cache=unset")),
            "the user's uv keeps its own cache"
        );
        assert!(
            first
                .iter()
                .all(|line| line.contains(&format!("kiss={}", data.join("kiss").display())))
        );
        let record: EnvRecord =
            serde_json::from_str(&std::fs::read_to_string(layout.record()).expect("record"))
                .expect("json");
        assert_eq!(record.version.as_deref(), Some("2026.11.7"));
        let info = sorcar.info();
        assert_eq!(info.readiness, Readiness::Ready);
        assert!(
            !info.status.contains("first turn downloads"),
            "{}",
            info.status
        );

        // Again: one import check, no progress, fast.
        let started = Instant::now();
        let quiet = std::cell::RefCell::new(Vec::new());
        sorcar
            .prepare(&|line| quiet.borrow_mut().push(line.to_owned()))
            .expect("prepare again");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(quiet.into_inner().is_empty());
        let second = calls(&log);
        assert_eq!(second.len(), 4);
        assert!(second[3].starts_with("python ") && second[3].contains("--check"));

        // A turn waits while setup holds the lock, and can be stopped there.
        let request = TurnRequest {
            project_root: paper.clone(),
            prompt: "hi".into(),
            ..TurnRequest::default()
        };
        let lock = setup_lock(&data);
        let guard = lock.lock();
        let stopped = sorcar.start(request.clone()).expect("start");
        assert_eq!(
            stopped.events.recv_blocking().expect("waiting"),
            AgentEvent::Status {
                message: "Waiting for Sorcar's setup to finish…".into()
            }
        );
        stopped.cancel();
        assert!(matches!(
            collect(&stopped).as_slice(),
            [AgentEvent::Completed { success: false, .. }]
        ));

        let waiting = sorcar.start(request).expect("start");
        assert!(matches!(
            waiting.events.recv_blocking(),
            Ok(AgentEvent::Status { .. })
        ));
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            waiting.events.try_recv().is_err(),
            "nothing happens while setup runs"
        );
        drop(guard);
        let events = collect(&waiting);
        assert_eq!(
            events,
            vec![AgentEvent::Completed {
                success: true,
                summary: "done".into(),
                session: Some("chat-x".into())
            }]
        );
        // The turn ran the environment's python directly; no uv involved.
        let third = calls(&log);
        assert_eq!(third.len(), 5);
        assert!(third[4].starts_with("python -I -u ") && !third[4].contains("--check"));
    }

    #[test]
    fn a_stale_or_broken_environment_is_rebuilt() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (sorcar, data, log) = fake_setup(temp.path());
        let layout = Layout { data_dir: data };
        sorcar.prepare(&|_| {}).expect("prepare");
        assert_eq!(calls(&log).len(), 3);

        // Another pinned release recorded: prepare rebuilds.
        let mut record: EnvRecord =
            serde_json::from_str(&std::fs::read_to_string(layout.record()).expect("record"))
                .expect("json");
        record.package = "kiss-agent-framework==2025.1.1".into();
        std::fs::write(
            layout.record(),
            serde_json::to_string(&record).expect("json"),
        )
        .expect("write");
        sorcar.prepare(&|_| {}).expect("rebuild");
        let rebuilt = calls(&log);
        assert_eq!(rebuilt.len(), 6);
        assert!(rebuilt[3].starts_with("uv venv"));
        assert!(env_is_current(&layout));

        // The interpreter vanished: the next turn rebuilds, saying so.
        std::fs::remove_file(layout.python()).expect("remove python");
        let paper = temp.path().join("paper");
        std::fs::create_dir_all(&paper).expect("paper");
        let events = collect(
            &sorcar
                .start(TurnRequest {
                    project_root: paper,
                    prompt: "hi".into(),
                    ..TurnRequest::default()
                })
                .expect("start"),
        );
        let statuses: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Status { message } => Some(message.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            statuses.first().copied(),
            Some(
                "Setting up Sorcar… the first run downloads its Python environment, which takes a few minutes."
            )
        );
        assert_eq!(statuses.last().copied(), Some("Sorcar is ready."));
        assert!(matches!(
            events.last(),
            Some(AgentEvent::Completed { success: true, .. })
        ));
        let after = calls(&log);
        assert!(after[6].starts_with("uv venv"));
        assert!(
            after
                .last()
                .is_some_and(|line| line.starts_with("python -I -u ") && !line.contains("--check"))
        );
    }

    #[test]
    fn prepare_refuses_when_nothing_can_run() {
        let temp = tempfile::tempdir().expect("tempdir");
        let err = harness(Detection::default(), temp.path())
            .prepare(&|_| {})
            .expect_err("unavailable");
        assert!(err.to_string().contains("Install Claude Code or Codex"));
    }

    #[test]
    fn unavailable_turn_fails_with_the_status() {
        let temp = tempfile::tempdir().expect("tempdir");
        let handle = harness(Detection::default(), temp.path())
            .start(TurnRequest {
                project_root: temp.path().into(),
                prompt: "hi".into(),
                ..TurnRequest::default()
            })
            .expect("start");
        let event = handle.events.recv_blocking().expect("event");
        assert!(
            matches!(event, AgentEvent::Failed { message } if message.contains("Install Claude Code"))
        );
        assert!(
            handle.events.recv_blocking().is_err(),
            "closed after the terminal event"
        );
    }

    #[test]
    fn bridge_is_written_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let layout = Layout {
            data_dir: temp.path().join("data"),
        };
        write_bridge(&layout).expect("write");
        assert_eq!(
            std::fs::read_to_string(layout.bridge()).expect("read"),
            BRIDGE_SOURCE
        );
        assert!(layout.kiss_home().is_dir());
        let modified = std::fs::metadata(layout.bridge())
            .and_then(|m| m.modified())
            .expect("mtime");
        write_bridge(&layout).expect("write again");
        assert_eq!(
            std::fs::metadata(layout.bridge())
                .and_then(|m| m.modified())
                .expect("mtime"),
            modified
        );
    }

    #[test]
    fn commands() {
        let layout = Layout {
            data_dir: PathBuf::from("/data"),
        };
        let text = |args: Vec<OsString>| {
            args.iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(
            text(uv_venv_args(&layout.venv())),
            "venv --no-project --python 3.13 /data/sorcar/venv"
        );
        assert_eq!(
            text(uv_pip_args(&layout.python())),
            format!("pip install --python /data/sorcar/venv/bin/python {SORCAR_PACKAGE}")
        );
        assert_eq!(
            text(python_args(&layout.bridge(), false)),
            "-I -u /data/bridge/sorcar_bridge.py"
        );
        assert_eq!(
            text(python_args(&layout.bridge(), true)),
            "-I -u /data/bridge/sorcar_bridge.py --check"
        );
        let installer = uv_installer(&layout, &[("UV_INSTALL_DIR".into(), "/elsewhere".into())]);
        let get = |name: &str| {
            installer
                .env
                .iter()
                .filter(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(get("UV_INSTALL_DIR"), vec![OsString::from("/data/bin")]);
        assert_eq!(get("UV_NO_MODIFY_PATH"), vec![OsString::from("1")]);
        assert!(
            installer.args[1]
                .to_string_lossy()
                .contains("https://astral.sh/uv/install.sh")
        );
    }

    #[test]
    fn bridge_request_keeps_the_raw_message_for_history() {
        let request = TurnRequest {
            project_root: "/papers/owls".into(),
            prompt: " add owls ".into(),
            session: Some(String::new()),
            ..TurnRequest::default()
        };
        let bridge = BridgeRequest::new(&request, Some("cc/haiku".into()));
        assert_eq!(bridge.history_prompt, "add owls");
        assert!(
            bridge.prompt.contains("# Oyl's rules")
                && bridge.prompt.ends_with("# Request\nadd owls\n")
        );
        assert_eq!(bridge.session, None);
        let json = serde_json::to_value(&bridge).expect("json");
        assert_eq!(json["model"], "cc/haiku");
        assert_eq!(json["work_dir"], "/papers/owls");
    }

    #[test]
    fn bridge_lines_map_to_events() {
        let mut parser = SorcarParser::new("/papers/owls".into(), None);
        let mut out = Vec::new();
        parser.stderr_line("Traceback (most recent call last):", &mut out);
        for line in [
            json!({"type":"ready","version":"2026.11.7"}),
            json!({"type":"session","session":"chat-1"}),
            json!({"type":"text_delta","text":"Reading."}),
            json!({"type":"tool_started","id":"sorcar-1","name":"Edit","input":{"file_path":"/papers/owls/main.typ","old_string":"a","new_string":"b"}}),
            json!({"type":"tool_finished","id":"sorcar-1","ok":true,"output":"ok"}),
            json!({"type":"usage","cost_usd":0.01,"input_tokens":100,"output_tokens":null}),
            json!({"type":"some_future_event","x":1}),
            json!({"type":"completed","success":true,"summary":"Added owls.","session":null}),
        ] {
            parser.stdout_line(&line.to_string(), &mut out);
        }
        assert_eq!(
            out,
            vec![
                AgentEvent::TextDelta { text: "Reading.".into() },
                AgentEvent::ToolStarted {
                    id: "sorcar-1".into(),
                    name: "Edit".into(),
                    title: "Edit main.typ".into(),
                    detail: Some(
                        json!({"file_path":"/papers/owls/main.typ","old_string":"a","new_string":"b"}).to_string()
                    ),
                },
                AgentEvent::ToolFinished {
                    id: "sorcar-1".into(),
                    ok: true,
                    output: Some("ok".into())
                },
                AgentEvent::FileChanged {
                    path: "/papers/owls/main.typ".into()
                },
                AgentEvent::Usage {
                    cost_usd: Some(0.01),
                    input_tokens: Some(100),
                    output_tokens: None
                },
                AgentEvent::Completed {
                    success: true,
                    summary: "Added owls.".into(),
                    session: Some("chat-1".into())
                },
            ]
        );
    }

    #[test]
    fn bridge_source_disables_worktrees() {
        assert!(BRIDGE_SOURCE.contains("ALLOWED_TOOLS"));
        assert!(!BRIDGE_SOURCE.contains("\"create_worktree\","));
        assert!(BRIDGE_SOURCE.contains("web_tools=False"));
        assert!(BRIDGE_SOURCE.contains("is_parallel=False"));
        assert!(BRIDGE_SOURCE.contains("system_prompt_hook=_strip_worktree_rules"));
    }
}
