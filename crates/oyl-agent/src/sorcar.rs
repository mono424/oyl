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
//! Models: the newest Claude models through the subscription (`cc/…`), Codex
//! models, then the API-key models present. An effort is Sorcar's level
//! suffix on the catalog id (`cc/claude-opus-5-5-xhigh`). Claude Fable 5.2,
//! newer than the pinned release, is registered in `KISS_HOME/MY_MODELS.json`.
//!
//! Capabilities run one of Sorcar's agents (SEAs) on the request instead of
//! a plain turn: `discover` (the AI discovery loop, in the git repository of
//! the user's experiments, with Progress read from its state files),
//! `write_paper`, `review` (the report goes to `.oyl/reviews/`) and
//! `revise_and_review` (rounds of writing and fresh reviews, run by the
//! bridge, with Progress per draft).
//!
//! Plain turns and the paper capabilities edit the paper folder in place:
//! the bridge gives them no `create_worktree` / `commit_and_merge` tools
//! (which also switches off Sorcar's worktree write guard) and removes the
//! worktree rules from the system prompt. A discovery run keeps Sorcar's
//! worktrees and commits: they are how it keeps its frontier.

use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use oyl_model::agent::{
    AgentEvent, Capability, Harness, HarnessId, HarnessInfo, ModelChoice, Progress, Readiness,
    TurnHandle, TurnRequest,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    detect::{Detection, Setup, is_executable},
    discovery::{DiscoverWatcher, round_limit},
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

/// The reasoning efforts Sorcar's Claude models come in, lowest first: its
/// catalog lists each as an alias of the model (`cc/claude-opus-5-5-xhigh`).
pub const SORCAR_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// The effort Sorcar's catalog runs a model at when no level is named.
pub const SORCAR_DEFAULT_EFFORT: &str = "high";
/// The model Sorcar uses when the user picked none, with `claude` installed.
pub const SORCAR_DEFAULT_MODEL: &str = "cc/claude-opus-5-5";
/// Claude Fable 5.2 through the Claude subscription. The pinned Sorcar
/// release predates it, so Oyl registers it (and its effort levels) in
/// Sorcar's model registry, `MY_MODELS.json` under `KISS_HOME`.
pub const FABLE_5_2: &str = "cc/claude-fable-5-2";

/// Short names Sorcar or older Oyl versions used, and the full ids they
/// stand for now.
const SORCAR_ALIASES: [(&str, &str); 4] = [
    ("cc/opus", "cc/claude-opus-5-5"),
    ("cc/sonnet", "cc/claude-sonnet-5-5"),
    ("cc/haiku", "cc/claude-haiku-5-5"),
    ("cc/fable", FABLE_5_2),
];

/// The effort levels a model has as catalog aliases (`<model>-<level>`).
fn effort_levels(model: &str) -> &'static [&'static str] {
    match model {
        FABLE_5_2
        | "cc/claude-fable-5-1"
        | "cc/claude-fable-5"
        | "cc/claude-opus-5-5"
        | "cc/claude-sonnet-5-5"
        | "cc/claude-haiku-5-5"
        | "claude-opus-5-5"
        | "claude-sonnet-5-5" => &SORCAR_EFFORTS,
        "gpt-6.1-sol" => &["low", "medium", "high", "xhigh"],
        "gemini-3.8-flash" => &["low", "medium", "high"],
        _ => &[],
    }
}

/// The model id a Sorcar run uses for a picked model and effort.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelPick {
    /// What the bridge runs.
    pub id: String,
    /// The model without the effort, run instead when `id` turns out not
    /// to be in Sorcar's catalog.
    pub fallback: Option<String>,
    /// Why the effort could not be applied, for a status line.
    pub note: Option<String>,
}

/// Map a model and effort to Sorcar's catalog id: aliases resolve to the
/// current full id first (`cc/opus` → `cc/claude-opus-5-5`), an effort
/// becomes the level suffix (`cc/claude-opus-5-5-xhigh`), replacing one the
/// id already has; a model without that level keeps its default, with a
/// note.
pub(crate) fn resolve_model(model: &str, effort: Option<&str>) -> ModelPick {
    let model = SORCAR_ALIASES
        .iter()
        .find(|(alias, _)| *alias == model)
        .map_or(model, |(_, full)| full);
    // An id that already carries a level: its base and that level.
    let (base, given) = SORCAR_EFFORTS
        .iter()
        .find_map(|level| {
            model
                .strip_suffix(&format!("-{level}"))
                .filter(|base| effort_levels(base).contains(level))
                .map(|base| (base, Some(*level)))
        })
        .unwrap_or((model, None));
    match effort.filter(|e| !e.is_empty()) {
        None => ModelPick {
            id: model.to_owned(),
            fallback: given.map(|_| base.to_owned()),
            note: None,
        },
        Some(effort) if effort_levels(base).contains(&effort) => ModelPick {
            id: format!("{base}-{effort}"),
            fallback: Some(base.to_owned()),
            note: None,
        },
        Some(effort) => ModelPick {
            id: model.to_owned(),
            fallback: None,
            note: Some(format!(
                "{base} has no \"{effort}\" effort in Sorcar; it runs at its default."
            )),
        },
    }
}

/// The catalog entries Oyl adds to Sorcar's model registry: Fable 5.2 and
/// one alias per effort level, like the catalog's own Claude models.
pub(crate) fn registered_models() -> serde_json::Map<String, Value> {
    let entry = |thinking: &str, alias_of: Option<&str>| {
        let mut entry = serde_json::json!({
            "context_length": 500_000,
            "input_price_per_1M": 0.0,
            "output_price_per_1M": 0.0,
            "fc": true,
            "emb": false,
            "gen": true,
            "thinking": thinking,
            "comment": "Added by Oyl: Claude Fable 5.2 through the Claude Code CLI",
        });
        if let (Some(base), Some(map)) = (alias_of, entry.as_object_mut()) {
            map.insert("alias_of".into(), Value::String(base.into()));
        }
        entry
    };
    let mut models = serde_json::Map::new();
    models.insert(FABLE_5_2.into(), entry(SORCAR_DEFAULT_EFFORT, None));
    for level in SORCAR_EFFORTS {
        models.insert(
            format!("{FABLE_5_2}-{level}"),
            entry(level, Some(FABLE_5_2)),
        );
    }
    models
}

/// Merge [`registered_models`] into `<KISS_HOME>/MY_MODELS.json`, keeping
/// every other entry; written only when something changes.
pub(crate) fn register_models(layout: &Layout) -> std::io::Result<()> {
    let path = layout.kiss_home().join("MY_MODELS.json");
    let current: serde_json::Map<String, Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let mut merged = current.clone();
    for (name, entry) in registered_models() {
        merged.insert(name, entry);
    }
    if merged == current && path.is_file() {
        return Ok(());
    }
    std::fs::create_dir_all(layout.kiss_home())?;
    let text =
        serde_json::to_string_pretty(&Value::Object(merged)).map_err(std::io::Error::other)?;
    let temp = path.with_extension(format!("json.{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&temp, text + "\n")?;
    std::fs::rename(&temp, &path)
}

/// The models Sorcar can use here, newest first, and the one it uses by
/// default: Opus 5.5 through the Claude subscription when `claude` exists,
/// else `codex/default` through Codex, else Sorcar's own default for the
/// first provider API key present.
pub fn sorcar_models(detection: &Detection) -> (Vec<ModelChoice>, Option<String>) {
    let mut models = Vec::new();
    let mut push = |id: &str, label: &str| {
        models.push(ModelChoice {
            id: id.into(),
            label: label.into(),
        })
    };
    let mut default = None;
    if detection.claude.is_some() {
        push(FABLE_5_2, "Fable 5.2 (your Claude subscription)");
        push("cc/claude-opus-5-5", "Opus 5.5 (your Claude subscription)");
        push(
            "cc/claude-sonnet-5-5",
            "Sonnet 5.5 (your Claude subscription)",
        );
        push(
            "cc/claude-haiku-5-5",
            "Haiku 5.5 (your Claude subscription, fastest)",
        );
        push(
            "cc/claude-fable-5-1",
            "Fable 5.1 (your Claude subscription)",
        );
        default = Some(SORCAR_DEFAULT_MODEL.to_owned());
    }
    if detection.codex.is_some() {
        push("codex/default", "Codex default (your ChatGPT subscription)");
        push(
            "codex/gpt-6.1-sol",
            "GPT-6.1 Sol (your ChatGPT subscription)",
        );
        push(
            "codex/gpt-6.1-terra",
            "GPT-6.1 Terra (your ChatGPT subscription)",
        );
        push(
            "codex/gpt-6-luna",
            "GPT-6 Luna (your ChatGPT subscription, fastest)",
        );
        default = default.or_else(|| Some("codex/default".to_owned()));
    }
    const KEYED: &[(&str, &str, &str)] = &[
        (
            "ANTHROPIC_API_KEY",
            "claude-opus-5-5",
            "Opus 5.5 (Anthropic API key)",
        ),
        (
            "ANTHROPIC_API_KEY",
            "claude-sonnet-5-5",
            "Sonnet 5.5 (Anthropic API key)",
        ),
        (
            "OPENAI_API_KEY",
            "gpt-6.1-sol",
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
            "Opus 5.5 (OpenRouter)",
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
            default = default.or_else(|| Some((*id).to_owned()));
        }
    }
    (models, default)
}

/// Sorcar's specialised agents, offered as modes besides a plain turn.
pub fn sorcar_capabilities() -> Vec<Capability> {
    let capability =
        |id: &str, name: &str, description: &str, long_running: bool, placeholder: &str| {
            Capability {
                id: id.into(),
                name: name.into(),
                description: description.into(),
                long_running,
                placeholder: placeholder.into(),
            }
        };
    vec![
        capability(
            CAPABILITY_DISCOVER,
            "Discover",
            "AI discovery on your experiments: a baseline, the state of the art, then one idea per round, \
             each measured and committed, keeping a Pareto frontier of the winners",
            true,
            "What to improve, the metric and the goal, and the folder of your experiments…",
        ),
        capability(
            CAPABILITY_WRITE,
            "Write paper",
            "Writes or revises the paper with Sorcar's paper-writing agent, checking structure, \
             citations and AI-sounding prose",
            false,
            "What to write or revise, for which venue…",
        ),
        capability(
            CAPABILITY_REVIEW,
            "Review",
            "Reviews the paper like a careful reviewer of the venue, with the related work, and \
             writes the report to .oyl/reviews/",
            false,
            "The venue and anything the review should focus on…",
        ),
        capability(
            CAPABILITY_REVISE,
            "Revise & review",
            "Writes, reviews with a fresh reviewer and revises in rounds until the review says strong \
             accept or the paper stops improving",
            true,
            "Writing: what to write. Review: the venue and how to review…",
        ),
    ]
}

/// [`Capability::id`]s of [`sorcar_capabilities`].
pub const CAPABILITY_DISCOVER: &str = "discover";
/// See [`CAPABILITY_DISCOVER`].
pub const CAPABILITY_WRITE: &str = "write_paper";
/// See [`CAPABILITY_DISCOVER`].
pub const CAPABILITY_REVIEW: &str = "review";
/// See [`CAPABILITY_DISCOVER`].
pub const CAPABILITY_REVISE: &str = "revise_and_review";

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
            efforts: SORCAR_EFFORTS.iter().map(|e| (*e).to_owned()).collect(),
            default_effort: Some(SORCAR_DEFAULT_EFFORT.into()),
            capabilities: sorcar_capabilities(),
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
    if let Err(err) = register_models(layout) {
        log::warn!("could not register Fable 5.2 with Sorcar: {err}");
    }
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
    /// A plain turn: paper context, writing rules and the message. A
    /// capability: the task for its agent.
    pub prompt: String,
    /// The message alone, as Sorcar's chat history records it.
    pub history_prompt: String,
    pub model: Option<String>,
    /// Run instead of `model` when that is not in Sorcar's catalog.
    pub model_fallback: Option<String>,
    pub work_dir: PathBuf,
    pub active_file: Option<PathBuf>,
    pub main_file: Option<PathBuf>,
    pub session: Option<String>,
    pub attachments: Vec<PathBuf>,
    /// A [`Capability::id`], or `None` for a plain turn.
    pub capability: Option<String>,
    /// Review: where the report goes.
    pub review_output: Option<PathBuf>,
    /// Revise & review: where each round's review goes.
    pub reports_dir: Option<PathBuf>,
    /// Revise & review: the round cap the task names.
    pub max_rounds: Option<u32>,
}

impl BridgeRequest {
    pub fn new(
        request: &TurnRequest,
        model: Option<ModelPick>,
        plan: Option<&CapabilityPlan>,
    ) -> Self {
        let (model, model_fallback) = match model {
            Some(pick) => (Some(pick.id), pick.fallback),
            None => (None, None),
        };
        BridgeRequest {
            prompt: plan.map_or_else(|| prompt::full_prompt(request), |plan| plan.task.clone()),
            history_prompt: request.prompt.trim().to_owned(),
            model,
            model_fallback,
            work_dir: plan.map_or_else(
                || request.project_root.clone(),
                |plan| plan.work_dir.clone(),
            ),
            active_file: request.active_file.clone(),
            main_file: request.main_file.clone(),
            session: request.session.clone().filter(|s| !s.is_empty()),
            attachments: request.attachments.clone(),
            capability: plan.map(|plan| plan.capability.clone()),
            review_output: plan.and_then(|plan| plan.review_output.clone()),
            reports_dir: plan.and_then(|plan| plan.reports_dir.clone()),
            max_rounds: plan.and_then(|plan| plan.max_rounds),
        }
    }
}

/// How a capability runs: where, on what task, and where its reports go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CapabilityPlan {
    pub capability: String,
    pub work_dir: PathBuf,
    pub task: String,
    pub review_output: Option<PathBuf>,
    pub reports_dir: Option<PathBuf>,
    pub max_rounds: Option<u32>,
    /// Discover: the round limit, for Progress.
    pub round_limit: Option<u32>,
}

/// Plan a capability run, or say why it cannot run.
pub(crate) fn plan_capability(
    request: &TurnRequest,
    capability: &str,
    now: SystemTime,
) -> Result<CapabilityPlan, String> {
    let root = &request.project_root;
    let instructions = request.prompt.trim();
    let main = request.main_file.clone().unwrap_or_else(|| {
        root.join(
            prompt::engine_of(request)
                .unwrap_or(oyl_model::Engine::Typst)
                .default_main(),
        )
    });
    let stamp = utc_stamp(now);
    let mut plan = CapabilityPlan {
        capability: capability.to_owned(),
        work_dir: root.clone(),
        task: String::new(),
        review_output: None,
        reports_dir: None,
        max_rounds: None,
        round_limit: None,
    };
    match capability {
        CAPABILITY_DISCOVER => {
            let work_dir = discover_work_dir(request)?;
            let place = if work_dir == *root {
                format!("Work in {}, the folder of the paper.", work_dir.display())
            } else {
                format!(
                    "Work in {}: the experiments behind the paper at {}.",
                    work_dir.display(),
                    root.display()
                )
            };
            plan.round_limit = Some(round_limit(instructions));
            plan.task = format!(
                "{instructions}\n\n{place}\nThere is no browser in this run: search the state of the art with curl \
                 on the arXiv API (http://export.arxiv.org/api/query), Semantic Scholar \
                 (https://api.semanticscholar.org/graph/v1/paper/search) and GitHub search; this overrides the \
                 note that web tools are off. Run unattended: nobody can answer questions."
            );
            plan.work_dir = work_dir;
        }
        CAPABILITY_WRITE => {
            plan.task = format!(
                "{}\n# What to do\n{instructions}\n\nThe paper is {}; edit its files in place in {}. Run unattended: \
                 nobody can answer questions.",
                prompt::paper_context(request),
                main.display(),
                root.display()
            );
        }
        CAPABILITY_REVIEW => {
            let output = root
                .join(".oyl")
                .join("reviews")
                .join(format!("{stamp}.md"));
            let focus = if instructions.is_empty() {
                "No venue was named: review it for the venue it targets, else for a top venue of its field.".to_owned()
            } else {
                instructions.to_owned()
            };
            plan.task = format!(
                "Review the paper at {} (its sources are in {}).\n{focus}\nWrite the review to {}. Run unattended: \
                 when something is unclear, decide and say so in the review.",
                main.display(),
                root.display(),
                output.display()
            );
            plan.review_output = Some(output);
        }
        CAPABILITY_REVISE => {
            plan.reports_dir = Some(
                root.join(".oyl")
                    .join("reviews")
                    .join(format!("revise-{stamp}")),
            );
            plan.max_rounds = crate::discovery::named_round_limit(instructions);
            plan.task = format!(
                "{instructions}\n\nThe paper is {} (its sources are in {}).",
                main.display(),
                root.display()
            );
        }
        other => return Err(format!("Sorcar has no \"{other}\" mode.")),
    }
    Ok(plan)
}

/// The folder a discovery run works in: a folder outside the paper that
/// the request names (`~/exp/fib`, `"../my experiments"`), else the paper
/// folder. It must be in a git repository — the loop commits every idea.
pub(crate) fn discover_work_dir(request: &TurnRequest) -> Result<PathBuf, String> {
    let root = &request.project_root;
    let named = path_candidates(&request.prompt)
        .into_iter()
        .filter_map(|candidate| {
            let path = if let Some(rest) = candidate.strip_prefix("~/") {
                dirs::home_dir()?.join(rest)
            } else {
                root.join(&candidate)
            };
            let path = std::fs::canonicalize(&path).ok()?;
            path.is_dir().then_some(path)
        })
        .find(|path| {
            let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
            !path.starts_with(&canonical_root)
        });
    let work_dir = named.unwrap_or_else(|| root.clone());
    if !work_dir.ancestors().any(|dir| dir.join(".git").exists()) {
        return Err(format!(
            "Discover needs a git repository: it commits every idea it tries. {} is not in one; run `git init` \
             there, or name the folder of your experiments in the request.",
            work_dir.display()
        ));
    }
    Ok(work_dir)
}

/// Path-looking words of a request: quoted or backticked text, and words
/// starting with `/`, `~/`, `./` or `../`.
fn path_candidates(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    for quote in ['`', '"', '\''] {
        let parts: Vec<&str> = text.split(quote).collect();
        candidates.extend(
            parts
                .iter()
                .skip(1)
                .step_by(2)
                .map(|part| part.trim().to_owned()),
        );
    }
    candidates.extend(
        text.split_whitespace()
            .map(|word| {
                word.trim_matches(|c: char| {
                    matches!(c, '`' | '"' | '\'' | '(' | ')' | ',' | ';' | ':')
                })
            })
            .map(|word| word.trim_end_matches('.'))
            .filter(|word| {
                ["/", "~/", "./", "../"]
                    .iter()
                    .any(|start| word.starts_with(start))
            })
            .map(str::to_owned),
    );
    candidates.retain(|c| !c.is_empty() && !c.contains('\n'));
    candidates
}

/// `2026-11-09-171203`, in UTC.
pub(crate) fn utc_stamp(now: SystemTime) -> String {
    let secs = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
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
    let plan = match request.capability.as_deref().filter(|c| !c.is_empty()) {
        Some(capability) => match plan_capability(&request, capability, SystemTime::now()) {
            Ok(plan) => Some(plan),
            Err(message) => {
                turn.fail(message);
                return;
            }
        },
        None => None,
    };

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

    let pick = request
        .model
        .clone()
        .filter(|m| !m.is_empty())
        .or_else(|| info.default_model.clone())
        .map(|model| resolve_model(&model, request.effort.as_deref()));
    if let Some(note) = pick.as_ref().and_then(|pick| pick.note.clone()) {
        turn.status(note);
    }
    let bridge_request = BridgeRequest::new(&request, pick, plan.as_ref());
    let stdin = match serde_json::to_vec(&bridge_request) {
        Ok(bytes) => bytes,
        Err(err) => {
            turn.fail(format!("Could not encode the Sorcar request: {err}"));
            return;
        }
    };
    let work_dir = bridge_request.work_dir.clone();
    let spawn = Spawn {
        program: python,
        args: python_args(&layout.bridge(), false),
        cwd: work_dir.clone(),
        env: sorcar_env(detection.child_env(), layout),
        stdin,
    };
    let watcher = plan
        .as_ref()
        .and_then(|plan| plan.round_limit)
        .map(DiscoverWatcher::new);
    let mut parser = SorcarParser::new(work_dir, request.session.clone()).with_watcher(watcher);
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
    /// Where a discovery run keeps its state files now.
    StateDir {
        path: PathBuf,
    },
    /// Progress the bridge knows itself (the revise-and-review rounds).
    Progress {
        progress: Progress,
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
    /// A discovery run's state files, read for Progress.
    watcher: Option<DiscoverWatcher>,
}

impl SorcarParser {
    pub(crate) fn new(root: PathBuf, session: Option<String>) -> Self {
        Self {
            root,
            session: session.filter(|s| !s.is_empty()),
            edits: HashMap::new(),
            watcher: None,
        }
    }

    /// Watch a discovery run's state files.
    pub(crate) fn with_watcher(mut self, watcher: Option<DiscoverWatcher>) -> Self {
        self.watcher = watcher;
        self
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
            BridgeEvent::StateDir { path } => {
                if let Some(watcher) = &mut self.watcher {
                    watcher.set_state_dir(path);
                }
            }
            BridgeEvent::Progress { progress } => out.push(AgentEvent::Progress(progress)),
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
                if let Some(watcher) = &mut self.watcher {
                    watcher.tool(&name, Some(&input.to_string()));
                }
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
                // The last word on a discovery run's state, before it ends.
                if let Some(progress) = self.watcher.as_mut().and_then(DiscoverWatcher::flush) {
                    out.push(AgentEvent::Progress(progress));
                }
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

    fn tick(&mut self, out: &mut Vec<AgentEvent>) {
        if let Some(progress) = self.watcher.as_mut().and_then(|w| w.tick(Instant::now())) {
            out.push(AgentEvent::Progress(progress));
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
        assert_eq!(default.as_deref(), Some(SORCAR_DEFAULT_MODEL));
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            &ids[..5],
            [
                FABLE_5_2,
                "cc/claude-opus-5-5",
                "cc/claude-sonnet-5-5",
                "cc/claude-haiku-5-5",
                "cc/claude-fable-5-1"
            ]
        );
        assert_eq!(models[0].label, "Fable 5.2 (your Claude subscription)");
        assert!(ids.contains(&"codex/default"));

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
        assert_eq!(no_uv.default_model.as_deref(), Some(SORCAR_DEFAULT_MODEL));
        assert_eq!(no_uv.default_effort.as_deref(), Some("high"));
        assert_eq!(no_uv.efforts, ["low", "medium", "high", "xhigh", "max"]);

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
        let bridge =
            BridgeRequest::new(&request, Some(resolve_model("cc/haiku", Some("low"))), None);
        assert_eq!(bridge.history_prompt, "add owls");
        assert!(
            bridge.prompt.contains("# Oyl's rules")
                && bridge.prompt.ends_with("# Request\nadd owls\n")
        );
        assert_eq!(bridge.session, None);
        let json = serde_json::to_value(&bridge).expect("json");
        assert_eq!(json["model"], "cc/claude-haiku-5-5-low");
        assert_eq!(json["model_fallback"], "cc/claude-haiku-5-5");
        assert_eq!(json["work_dir"], "/papers/owls");
        assert_eq!(json["capability"], Value::Null);
    }

    #[test]
    fn models_and_efforts_resolve_to_catalog_ids() {
        let pick = |model: &str, effort: Option<&str>| resolve_model(model, effort);
        assert_eq!(
            pick("cc/opus", None),
            ModelPick {
                id: "cc/claude-opus-5-5".into(),
                fallback: None,
                note: None
            }
        );
        assert_eq!(
            pick("cc/opus", Some("xhigh")).id,
            "cc/claude-opus-5-5-xhigh"
        );
        assert_eq!(pick(FABLE_5_2, Some("low")).id, "cc/claude-fable-5-2-low");
        assert_eq!(pick("cc/fable", Some("max")).id, "cc/claude-fable-5-2-max");
        // An id that already has a level gets the new one instead.
        let replaced = pick("cc/claude-sonnet-5-5-max", Some("medium"));
        assert_eq!(replaced.id, "cc/claude-sonnet-5-5-medium");
        assert_eq!(replaced.fallback.as_deref(), Some("cc/claude-sonnet-5-5"));
        assert_eq!(
            pick("cc/claude-sonnet-5-5-max", None).id,
            "cc/claude-sonnet-5-5-max"
        );
        assert_eq!(pick("gpt-6.1-sol", Some("xhigh")).id, "gpt-6.1-sol-xhigh");
        // No such level: the model at its default, with a note.
        let codex = pick("codex/default", Some("high"));
        assert_eq!(codex.id, "codex/default");
        assert!(codex.note.is_some_and(|n| n.contains("no \"high\" effort")));
        let gemini = pick("gemini-3.8-flash", Some("max"));
        assert_eq!(
            (gemini.id.as_str(), gemini.fallback),
            ("gemini-3.8-flash", None)
        );
        // A real model whose name ends like a level is left alone.
        assert_eq!(
            pick("openrouter/openai/o3-mini-high", None).id,
            "openrouter/openai/o3-mini-high"
        );
    }

    #[test]
    fn fable_5_2_is_registered_without_losing_other_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let layout = Layout {
            data_dir: temp.path().into(),
        };
        std::fs::create_dir_all(layout.kiss_home()).expect("kiss home");
        let path = layout.kiss_home().join("MY_MODELS.json");
        std::fs::write(&path, r#"{"_documentation": ["x"], "my/model": {"context_length": 1000, "input_price_per_1M": 0, "output_price_per_1M": 0}}"#)
            .expect("seed");
        register_models(&layout).expect("register");
        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        assert!(saved.get("my/model").is_some() && saved.get("_documentation").is_some());
        assert_eq!(saved[FABLE_5_2]["thinking"], "high");
        assert_eq!(saved[FABLE_5_2]["context_length"], 500_000);
        for level in SORCAR_EFFORTS {
            let alias = &saved[format!("{FABLE_5_2}-{level}")];
            assert_eq!(alias["thinking"], level);
            assert_eq!(alias["alias_of"], FABLE_5_2);
        }
        let modified = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .expect("mtime");
        register_models(&layout).expect("again");
        assert_eq!(
            std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .expect("mtime"),
            modified,
            "unchanged"
        );
    }

    #[test]
    fn capabilities_are_listed() {
        let caps = sorcar_capabilities();
        let ids: Vec<(&str, bool)> = caps
            .iter()
            .map(|c| (c.id.as_str(), c.long_running))
            .collect();
        assert_eq!(
            ids,
            [
                (CAPABILITY_DISCOVER, true),
                (CAPABILITY_WRITE, false),
                (CAPABILITY_REVIEW, false),
                (CAPABILITY_REVISE, true)
            ]
        );
        assert!(
            caps.iter().all(|c| !c.name.is_empty()
                && !c.description.is_empty()
                && !c.placeholder.is_empty())
        );
        let temp = tempfile::tempdir().expect("tempdir");
        let info = harness(
            Detection {
                claude: tool("/x/claude"),
                ..Detection::default()
            },
            temp.path(),
        )
        .info();
        assert_eq!(info.capabilities, caps);
    }

    #[test]
    fn capabilities_are_planned() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paper = temp.path().join("paper");
        std::fs::create_dir_all(&paper).expect("paper");
        let experiments = temp.path().join("my experiments");
        std::fs::create_dir_all(experiments.join(".git")).expect("repo");
        let not_git = temp.path().join("plain");
        std::fs::create_dir_all(&not_git).expect("plain");
        let request = |prompt: String| TurnRequest {
            project_root: paper.clone(),
            main_file: Some(paper.join("main.typ")),
            prompt,
            ..TurnRequest::default()
        };
        let now = UNIX_EPOCH + Duration::from_secs(1_794_244_323);

        let discover = plan_capability(
            &request(format!(
                "Make fib faster in \"{}\", round limit 2",
                experiments.display()
            )),
            CAPABILITY_DISCOVER,
            now,
        )
        .expect("discover");
        assert_eq!(
            discover.work_dir,
            std::fs::canonicalize(&experiments).expect("canonical")
        );
        assert_eq!(discover.round_limit, Some(2));
        assert!(discover.task.contains("round limit 2") && discover.task.contains("curl"));
        let err = plan_capability(
            &request(format!("speed up {}", not_git.display())),
            CAPABILITY_DISCOVER,
            now,
        )
        .expect_err("not a repository");
        assert!(err.contains("git init"), "{err}");
        // Nothing named and the paper is no repository either.
        assert!(plan_capability(&request("faster".into()), CAPABILITY_DISCOVER, now).is_err());

        let review = plan_capability(&request("for CHI 2027".into()), CAPABILITY_REVIEW, now)
            .expect("review");
        let output = paper.join(".oyl/reviews/2026-11-09-171203.md");
        assert_eq!(review.review_output.as_deref(), Some(output.as_path()));
        assert!(
            review.task.contains(&output.display().to_string())
                && review.task.contains("for CHI 2027")
        );
        assert!(
            review
                .task
                .contains(&paper.join("main.typ").display().to_string())
        );

        let write = plan_capability(
            &request("Tighten the abstract".into()),
            CAPABILITY_WRITE,
            now,
        )
        .expect("write");
        assert!(write.task.contains("# Paper") && write.task.contains("Tighten the abstract"));
        assert_eq!(write.work_dir, paper);

        let revise = plan_capability(
            &request("Writing: tighten. Review: CHI. 3 rounds".into()),
            CAPABILITY_REVISE,
            now,
        )
        .expect("revise");
        assert_eq!(revise.max_rounds, Some(3));
        assert_eq!(
            revise.reports_dir,
            Some(paper.join(".oyl/reviews/revise-2026-11-09-171203"))
        );

        assert!(plan_capability(&request("x".into()), "teleport", now).is_err());
    }

    #[test]
    fn stamps_are_utc() {
        assert_eq!(utc_stamp(UNIX_EPOCH), "1970-01-01-000000");
        assert_eq!(
            utc_stamp(UNIX_EPOCH + Duration::from_secs(1_709_251_199)),
            "2024-02-29-235959"
        );
    }

    #[test]
    fn discovery_runs_report_progress_from_their_state_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("tmp").join("task-1");
        std::fs::create_dir_all(&state).expect("state");
        std::fs::write(state.join("split.md"), "eval: python bench.py\n").expect("split");
        std::fs::write(
            state.join("pareto.md"),
            "| id | runtime_ms |\n|---|---|\n| B | 812 |\n| I1 | 3.1 |\n",
        )
        .expect("pareto");
        std::fs::write(
            state.join("explored-ideas.md"),
            "| id | dev metrics | status |\n|---|---|---|\n| B | runtime_ms=812 | KEPT |\n| I1 | runtime_ms=3.1 | KEPT |\n",
        )
        .expect("explored");
        let mut parser =
            SorcarParser::new(temp.path().into(), None).with_watcher(Some(DiscoverWatcher::new(2)));
        let mut out = Vec::new();
        parser.tick(&mut out);
        assert!(out.is_empty(), "no state dir yet");
        parser.stdout_line(
            &json!({"type": "state_dir", "path": state}).to_string(),
            &mut out,
        );
        parser.stdout_line(
            &json!({"type":"tool_started","id":"s1","name":"Bash","input":{"command":"python bench.py --dev","description":"eval"}}).to_string(),
            &mut out,
        );
        parser.tick(&mut out);
        let progress = match out.iter().find(|e| matches!(e, AgentEvent::Progress(_))) {
            Some(AgentEvent::Progress(progress)) => progress.clone(),
            _ => panic!("no progress in {out:?}"),
        };
        assert_eq!(progress.phase, "Evaluating");
        assert_eq!((progress.step, progress.total), (Some(1), Some(2)));
        assert_eq!(
            progress.metrics[0],
            ("runtime_ms".to_owned(), "3.1".to_owned())
        );
        assert_eq!(progress.state_dir.as_deref(), Some(state.as_path()));
        // The bridge's own progress passes through.
        let mut more = Vec::new();
        parser.stdout_line(
            &json!({"type":"progress","progress":{"phase":"Reviewing","phases":["Writing","Reviewing"],"step":2,"total":6,"fraction":0.33,"metrics":[["verdict","weak accept"]],"state_dir":null}}).to_string(),
            &mut more,
        );
        assert!(
            matches!(&more[..], [AgentEvent::Progress(p)] if p.phase == "Reviewing" && p.metrics[0].1 == "weak accept")
        );
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
        // A plain turn gets an allow-list without the worktree tools...
        let allowed = BRIDGE_SOURCE
            .split_once("ALLOWED_TOOLS = frozenset({")
            .and_then(|(_, rest)| rest.split_once("})"))
            .map(|(list, _)| list)
            .expect("ALLOWED_TOOLS");
        assert!(allowed.contains("\"Edit\"") && !allowed.contains("create_worktree"));
        // ...and so do capabilities that edit the paper; discover keeps them.
        assert!(BRIDGE_SOURCE.contains("WORKTREE_TOOLS if in_place"));
        assert!(BRIDGE_SOURCE.contains("in_place=capability != \"discover\""));
        assert!(BRIDGE_SOURCE.contains("web_tools=False"));
        assert!(BRIDGE_SOURCE.contains("is_parallel=False"));
        assert!(BRIDGE_SOURCE.contains("system_prompt_hook=_strip_worktree_rules"));
    }

    #[test]
    fn bridge_source_stops_turns_stuck_on_broken_tool_calls() {
        // Haiku 5.5 at low effort can loop on native call tags the CLI
        // replaces with a placeholder; the bridge ends such a turn.
        assert!(BRIDGE_SOURCE.contains("BROKEN_CALL_MARK = \"<invoke_tool_placeholder>\""));
        assert!(BRIDGE_SOURCE.contains("token = self._count_broken_calls(token)"));
        let guard = BRIDGE_SOURCE
            .split_once("def _count_broken_calls")
            .and_then(|(_, rest)| rest.split_once("def token_callback"))
            .map(|(body, _)| body)
            .expect("guard");
        assert!(guard.contains("\"failed\"") && guard.contains("_exit(1)"));
    }
}
