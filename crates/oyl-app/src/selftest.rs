//! `oyl --self-test`: drives the real window through Oyl's main paths
//! and exits 0 when every check passes, 1 otherwise.
//!
//! Everything happens in a fresh temporary folder: the data folder (as
//! `OYL_DATA_DIR` would set it) and the papers. The window opens without
//! taking focus. The agent is a scripted harness that edits `main.typ` on
//! disk in the middle of its turn, the way a real agent would. Oyl Cloud is
//! an in-process mock of its API (`oyl_sync::mock`), with the key kept in
//! memory: the self-test never touches the network or the Keychain.

use std::fmt::Display;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::{
    AnyWindowHandle, App, AsyncApp, Bounds, Entity, Focusable as _, Keystroke, Modifiers,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, PlatformInput, Window,
    point, px,
};
use oyl_agent::{ScriptStep, ScriptedHarness};
use oyl_core::LayoutMode;
use oyl_editor::EditorEvent;
use oyl_model::agent::{
    AgentEvent, Capability, FactCheckEvent, FactCheckHandle, FactCheckRequest, FactChecker,
    FactFinding, FindingLevel, Harness, HarnessId, HarnessInfo, ModelChoice, RewriteRequest,
    Rewriter, TurnHandle, TurnRequest,
};
use oyl_model::typeset::RenderedDocument;
use oyl_model::{Author, Engine, SourceLocation};

use crate::actions;
use crate::persist::{AppPaths, WindowFrame};
use crate::shell::{Agents, CloudOptions, Launch, Shell, ShellOptions, ToastKind};

/// What the scripted agent appends to `main.typ`.
const AGENT_SENTENCE: &str = "Owls hunt by sound, the self-test agent says.";
/// The prompt of the scripted turn.
const AGENT_PROMPT: &str = "Add a sentence about owls to the paper.";
/// What the test types into the Typst paper.
const TYPED: &str = "Oyl self-test typing";
/// A name the Typst paper does not define: an error.
const MISSING: &str = "oyl_selftest_missing";

/// Runs the self-test and exits.
pub fn run() {
    let temp = match tempfile::Builder::new().prefix("oyl-self-test-").tempdir() {
        Ok(temp) => temp,
        Err(err) => {
            eprintln!("self-test: no temporary folder: {err}");
            std::process::exit(1);
        }
    };
    let base = temp.path().to_path_buf();
    let paths = AppPaths::in_dir(base.join("data"));
    let papers = base.join("papers");
    println!("Oyl self-test in {}", base.display());
    let mock = match oyl_sync::mock::MockCloud::start() {
        Ok(mock) => Arc::new(mock),
        Err(err) => {
            eprintln!("self-test: no mock Oyl Cloud: {err}");
            std::process::exit(1);
        }
    };
    mock.set_poll_interval(1);
    let tokens: Arc<oyl_sync::MemoryTokens> = Arc::default();

    gpui_kit::application()
        .with_assets(crate::assets::OylAssets)
        .run(move |cx| {
            crate::init_app(cx);
            let harness: Arc<dyn Harness> = Arc::new(EditingHarness::new());
            let quick: Arc<dyn Harness> = Arc::new(QuickHarness::new(base.join("discovery")));
            let opened = open_shell(
                &paths,
                Agents::Given(vec![harness.clone(), quick]),
                cloud_options(&mock, &tokens),
                cx,
            );
            let (window, shell) = match opened {
                Ok(opened) => opened,
                Err(err) => {
                    eprintln!("self-test: no window: {err:#}");
                    std::process::exit(1);
                }
            };
            let paths = paths.clone();
            let papers = papers.clone();
            let (mock, tokens) = (mock.clone(), tokens.clone());
            cx.spawn(async move |cx| {
                let mut report = Report::new();
                let test = SelfTest {
                    paths: paths.clone(),
                    papers,
                    window,
                    shell,
                    previewed: std::cell::Cell::new(false),
                    cloud: mock,
                    tokens,
                };
                test.all(&mut report, harness, cx).await;
                let failed = report.finish();
                // The papers and data were ours; leave nothing behind.
                drop(temp);
                std::process::exit(if failed { 1 } else { 0 });
            })
            .detach();
        });
}

/// Oyl Cloud for the self-test: the mock, keys in memory, quick timing.
fn cloud_options(
    mock: &oyl_sync::mock::MockCloud,
    tokens: &Arc<oyl_sync::MemoryTokens>,
) -> CloudOptions {
    CloudOptions {
        url: Some(mock.url()),
        tokens: tokens.clone(),
        timing: oyl_sync::Timing {
            push_delay: Duration::from_millis(300),
            pull_every: Duration::from_secs(1),
            backoff_min: Duration::from_millis(200),
            backoff_max: Duration::from_secs(2),
        },
    }
}

fn open_shell(
    paths: &AppPaths,
    agents: Agents,
    cloud: CloudOptions,
    cx: &mut App,
) -> anyhow::Result<(AnyWindowHandle, Entity<Shell>)> {
    let options = crate::window_options(WindowFrame::load(paths), false, cx);
    let paths = paths.clone();
    gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| {
            Shell::new(
                ShellOptions {
                    paths,
                    launch: Launch::Restore,
                    agents,
                    cloud,
                },
                window,
                cx,
            )
        })
    })
}

/// The printed report.
struct Report {
    started: Instant,
    failures: Vec<String>,
    passed: usize,
}

impl Report {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            failures: Vec::new(),
            passed: 0,
        }
    }

    fn step(&self, title: &str) {
        println!("\n[{:>6.1}s] {title}", self.started.elapsed().as_secs_f32());
    }

    fn check(&mut self, ok: bool, what: impl Display) {
        if ok {
            self.passed += 1;
            println!("  ok    {what}");
        } else {
            println!("  FAIL  {what}");
            self.failures.push(what.to_string());
        }
    }

    /// Prints the summary; whether anything failed.
    fn finish(&self) -> bool {
        println!(
            "\n{} passed, {} failed in {:.1}s",
            self.passed,
            self.failures.len(),
            self.started.elapsed().as_secs_f32()
        );
        for failure in &self.failures {
            println!("  FAILED: {failure}");
        }
        !self.failures.is_empty()
    }
}

struct SelfTest {
    paths: AppPaths,
    papers: PathBuf,
    window: AnyWindowHandle,
    shell: Entity<Shell>,
    /// Whether the last branch turn's preview showed its branch.
    previewed: std::cell::Cell<bool>,
    /// The mock Oyl Cloud.
    cloud: Arc<oyl_sync::mock::MockCloud>,
    /// Where the app keeps its Oyl Cloud key in the self-test.
    tokens: Arc<oyl_sync::MemoryTokens>,
}

/// What a check reads off the shell.
#[derive(Clone, Debug, Default)]
struct Snapshot {
    root: Option<PathBuf>,
    name: Option<String>,
    compiles: u64,
    errors: usize,
    pages: usize,
    fingerprints: Vec<u64>,
    last_failed: bool,
    overlays: Vec<PathBuf>,
    mode: Option<LayoutMode>,
    citations: usize,
}

impl SelfTest {
    async fn all(&self, report: &mut Report, harness: Arc<dyn Harness>, cx: &mut AsyncApp) {
        report.step("0. The process can wait for its children");
        #[cfg(unix)]
        report.check(
            !crate::sigchld_ignored(),
            "SIGCHLD is not ignored (a Finder launch would otherwise auto-reap children)",
        );
        self.wait(cx, Duration::from_millis(300), |_| false).await;
        let typst_article = self.create_papers(report, cx).await;
        let Some(article) = typst_article else {
            report.check(false, "a Typst paper to continue with");
            return;
        };
        self.typing(report, &article, cx).await;
        self.modes(report, cx).await;
        self.agent_tab(report, cx).await;
        self.agent_turn(report, harness, &article, cx).await;
        self.media(report, &article, cx).await;
        self.exports(report, cx).await;
        self.palette(report, &article, cx).await;
        self.panels(report, &article, cx).await;
        self.threads(report, cx).await;
        self.background_agents(report, cx).await;
        self.modes_of_turns(report, cx).await;
        self.editor_ai(report, &article, cx).await;
        self.cloud_sync(report, &article, cx).await;
        let pending = self.branches(report, &article, cx).await;
        self.reopen(report, &article, pending, cx).await;
    }

    // -- Plumbing -----------------------------------------------------------

    fn snapshot(&self, cx: &mut AsyncApp) -> Snapshot {
        let shell = self.shell.clone();
        cx.update(|cx| {
            let shell = shell.read(cx);
            let document = shell.compile.document.clone();
            Snapshot {
                root: shell.paper.as_ref().map(|p| p.project.root().to_path_buf()),
                name: shell.paper.as_ref().map(|p| p.project.name().to_string()),
                compiles: shell.compile.finished,
                errors: shell.compile.errors,
                pages: document.as_ref().map(|d| d.page_count()).unwrap_or(0),
                fingerprints: document
                    .as_ref()
                    .map(|d| fingerprints(d.as_ref()))
                    .unwrap_or_default(),
                last_failed: shell.compile.last_failed,
                overlays: shell.compile.last_overlays.clone(),
                mode: Some(shell.mode),
                citations: shell.paper.as_ref().map(|p| p.citations.len()).unwrap_or(0),
            }
        })
    }

    /// Runs `f` on the shell with its window.
    fn with_shell<R>(
        &self,
        cx: &mut AsyncApp,
        f: impl FnOnce(&mut Shell, &mut Window, &mut gpui_kit::Context<Shell>) -> R,
    ) -> Option<R> {
        let shell = self.shell.clone();
        self.window
            .update(cx, |_, window, cx| {
                shell.update(cx, |shell, cx| f(shell, window, cx))
            })
            .ok()
    }

    /// Draws a frame, so layout-dependent work (key dispatch, scrolling)
    /// sees the current tree even when the screen does not ask for frames.
    fn frame(&self, cx: &mut AsyncApp) {
        let _ = self.window.update(cx, |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }

    /// Waits (drawing frames) until `done` holds or `timeout` passes.
    async fn wait(
        &self,
        cx: &mut AsyncApp,
        timeout: Duration,
        mut done: impl FnMut(&Snapshot) -> bool,
    ) -> bool {
        let start = Instant::now();
        loop {
            self.frame(cx);
            if done(&self.snapshot(cx)) {
                return true;
            }
            if start.elapsed() >= timeout {
                return false;
            }
            cx.background_executor()
                .timer(Duration::from_millis(30))
                .await;
        }
    }

    /// Waits for an arbitrary condition read from the app.
    async fn wait_app(
        &self,
        cx: &mut AsyncApp,
        timeout: Duration,
        mut done: impl FnMut(&App) -> bool,
    ) -> bool {
        let start = Instant::now();
        loop {
            self.frame(cx);
            if cx.update(|cx| done(cx)) {
                return true;
            }
            if start.elapsed() >= timeout {
                return false;
            }
            cx.background_executor()
                .timer(Duration::from_millis(30))
                .await;
        }
    }

    fn editor_text(&self, path: &Path, cx: &mut AsyncApp) -> String {
        let shell = self.shell.clone();
        let path = path.to_path_buf();
        cx.update(|cx| {
            let editor = shell.read(cx).editor.clone();
            editor.read(cx).text(&path, cx).unwrap_or_default()
        })
    }

    fn type_text(&self, text: &str, cx: &mut AsyncApp) {
        self.frame(cx);
        let text = text.to_string();
        let _ = self.window.update(cx, move |_, window, cx| {
            for c in text.chars() {
                let key = if c == ' ' {
                    "space".to_string()
                } else {
                    c.to_string()
                };
                window.dispatch_keystroke(
                    Keystroke {
                        modifiers: Modifiers::default(),
                        key,
                        key_char: Some(c.to_string()),
                    },
                    cx,
                );
            }
        });
    }

    /// A real click at the middle of `bounds`, through the window's event
    /// dispatch (hit testing and all).
    fn click(&self, bounds: Bounds<Pixels>, cx: &mut AsyncApp) {
        self.frame(cx);
        let at = point(
            bounds.origin.x + bounds.size.width / 2.,
            bounds.origin.y + bounds.size.height / 2.,
        );
        let _ = self.window.update(cx, move |_, window, cx| {
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: at,
                    pressed_button: None,
                    modifiers: Modifiers::default(),
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: at,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: at,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                }),
                cx,
            );
        });
        self.frame(cx);
    }

    /// Presses a key, as typed.
    fn press(&self, key: &'static str, cx: &mut AsyncApp) {
        self.frame(cx);
        if let Ok(keystroke) = Keystroke::parse(key) {
            let _ = self.window.update(cx, move |_, window, cx| {
                window.dispatch_keystroke(keystroke, cx);
            });
        }
        self.frame(cx);
    }

    fn dispatch(&self, action: Box<dyn gpui_kit::Action>, cx: &mut AsyncApp) {
        self.frame(cx);
        let _ = self
            .window
            .update(cx, move |_, window, cx| window.dispatch_action(action, cx));
        self.frame(cx);
    }

    // -- 1. Papers from every template ----------------------------------------

    async fn create_papers(&self, report: &mut Report, cx: &mut AsyncApp) -> Option<PathBuf> {
        report.step("1. New papers from every template open and compile");
        let mut plan: Vec<(Engine, &'static str, &'static str)> =
            oyl_core::templates(Engine::Typst)
                .iter()
                .map(|template| (Engine::Typst, template.id, template.name))
                .collect();
        plan.push((Engine::Latex, "article", "Article"));
        let mut typst_article = None;
        for (engine, template, template_name) in plan {
            let name = format!("Self-test {} {template_name}", engine.label());
            let parent = self.papers.clone();
            let wanted = name.clone();
            self.with_shell(cx, move |shell, window, cx| {
                shell.create_paper(wanted, engine, template, parent, window, cx)
            });
            let timeout = match engine {
                Engine::Typst => Duration::from_secs(60),
                Engine::Latex => Duration::from_secs(240),
            };
            let started = Instant::now();
            let compiled = self
                .wait(cx, timeout, |s| {
                    s.name.as_deref() == Some(name.as_str()) && s.compiles >= 1
                })
                .await;
            let s = self.snapshot(cx);
            report.check(
                compiled,
                format!(
                    "{name}: created, opened, compiled in {:.1}s",
                    started.elapsed().as_secs_f32()
                ),
            );
            report.check(s.pages >= 1, format!("{name}: {} page(s)", s.pages));
            let errors = cx.update(|cx| {
                self.shell
                    .read(cx)
                    .compile
                    .diagnostics
                    .iter()
                    .filter(|d| d.is_error())
                    .map(|d| d.message.clone())
                    .collect::<Vec<_>>()
            });
            report.check(
                errors.is_empty(),
                format!("{name}: no errors{}", describe(&errors)),
            );
            if engine == Engine::Typst && template == "article" {
                typst_article = s.root.clone();
            }
        }
        typst_article
    }

    // -- 2. Typing, an error, its fix ------------------------------------------

    async fn typing(&self, report: &mut Report, article: &Path, cx: &mut AsyncApp) {
        report.step("2. Typing reaches the preview without a save; errors keep the pages");
        let root = article.to_path_buf();
        self.with_shell(cx, move |shell, window, cx| {
            shell.open_paper(root, window, cx)
        });
        let opened = self
            .wait(cx, Duration::from_secs(60), |s| {
                s.root.as_deref() == Some(article) && s.compiles >= 1
            })
            .await;
        report.check(opened, "the Typst article is open again");
        let main = article.join("main.typ");
        let before = self.snapshot(cx);
        let text = self.editor_text(&main, cx);
        let last_line = text.split('\n').count() as u32;
        let location = SourceLocation::new(main.clone(), last_line, 1);
        self.with_shell(cx, move |shell, window, cx| {
            shell
                .editor
                .update(cx, |editor, cx| editor.goto(&location, window, cx))
        });
        self.type_text(TYPED, cx);
        let typed = self.editor_text(&main, cx).contains(TYPED);
        report.check(typed, "keystrokes reached the editor");
        let changed = self
            .wait(cx, Duration::from_secs(20), |s| {
                s.compiles > before.compiles && s.fingerprints != before.fingerprints
            })
            .await;
        let after = self.snapshot(cx);
        let disk = fs::read_to_string(&main).unwrap_or_default();
        report.check(
            changed,
            "the preview's pages changed (page fingerprints differ)",
        );
        report.check(
            after.overlays.iter().any(|p| p == &main),
            "that compile read the unsaved buffer",
        );
        report.check(
            !disk.contains(TYPED) || after.overlays.contains(&main),
            if disk.contains(TYPED) {
                "the file was saved by then (autosave), but the compile used the buffer"
            } else {
                "the file on disk did not have the text yet"
            },
        );

        // An error.
        let pages = after.pages;
        let compiles = after.compiles;
        let error_line = format!("\n#{MISSING}\n");
        self.with_shell(cx, move |shell, window, cx| {
            shell.editor.update(cx, |editor, cx| {
                editor.insert_at_cursor(&error_line, window, cx)
            })
        });
        let failed = self
            .wait(cx, Duration::from_secs(20), |s| {
                s.compiles > compiles && s.errors > 0
            })
            .await;
        report.check(failed, "an unknown name is reported as an error");
        let in_editor = cx.update(|cx| {
            self.shell.read(cx).editor_diagnostics.iter().any(|d| {
                d.is_error() && d.file.as_deref() == Some(main.as_path()) && d.line.is_some()
            })
        });
        report.check(
            in_editor,
            "the error reached the editor, at a line of main.typ",
        );
        let preview_pages = cx.update(|cx| {
            let preview = self.shell.read(cx).preview.clone();
            preview
                .read(cx)
                .document()
                .map(|d| d.page_count())
                .unwrap_or(0)
        });
        let s = self.snapshot(cx);
        report.check(
            s.last_failed && preview_pages == pages,
            format!("the last pages stay on screen ({preview_pages} of {pages})"),
        );
        let status = cx.update(|cx| self.shell.read(cx).status());
        report.check(
            status.text.contains("error"),
            format!("the header says “{}”", status.text),
        );

        // The fix: define the name above its use.
        let text = self.editor_text(&main, cx);
        let line = text
            .split('\n')
            .position(|line| line.trim() == format!("#{MISSING}"))
            .map(|index| index as u32 + 1)
            .unwrap_or(1);
        let location = SourceLocation::new(main.clone(), line, 1);
        let fix = format!("#let {MISSING} = []\n");
        let compiles = s.compiles;
        self.with_shell(cx, move |shell, window, cx| {
            shell.editor.update(cx, |editor, cx| {
                editor.goto(&location, window, cx);
                editor.insert_at_cursor(&fix, window, cx);
            })
        });
        let fixed = self
            .wait(cx, Duration::from_secs(20), |s| {
                s.compiles > compiles && s.errors == 0 && !s.last_failed
            })
            .await;
        report.check(fixed, "the fix compiles cleanly again");
        // Let autosave write it before the next steps read the disk.
        let saved = self
            .wait_app(cx, Duration::from_secs(5), |cx| {
                let editor = self.shell.read(cx).editor.clone();
                editor.read(cx).unsaved(cx).is_empty()
            })
            .await;
        report.check(saved, "autosave wrote the buffer");
    }

    // -- 3. Modes and appearance ----------------------------------------------

    async fn modes(&self, report: &mut Report, cx: &mut AsyncApp) {
        report.step("3. The two tabs, Write and Agent; dark and light");
        let tabs = cx.update(|cx| {
            let shell = self.shell.read(cx);
            crate::layout::MODES.map(crate::layout::mode_label) == ["Write", "Agent"]
                && !shell.editor.read(cx).is_zen()
        });
        report.check(
            tabs,
            "the header offers exactly Write and Agent; the editor is never in zen",
        );
        let checks: [(Box<dyn gpui_kit::Action>, LayoutMode); 3] = [
            (Box::new(actions::AgentMode), LayoutMode::Agent),
            (Box::new(actions::WriteMode), LayoutMode::Write),
            (Box::new(actions::AgentMode), LayoutMode::Agent),
        ];
        for (action, mode) in checks {
            self.dispatch(action, cx);
            let (now, saved) = cx.update(|cx| {
                let shell = self.shell.read(cx);
                (shell.mode, shell.settings.layout_mode)
            });
            report.check(
                now == mode && saved == mode,
                format!("{}: on screen, remembered", crate::layout::mode_label(mode)),
            );
        }
        self.press("cmd-1", cx);
        let mode = cx.update(|cx| self.shell.read(cx).mode);
        report.check(mode == LayoutMode::Write, "⌘1 is Write");
        // In Write, the paper gets exactly the right column below the header.
        self.frame(cx);
        let geometry = self.with_shell(cx, |shell, window, cx| {
            let viewport = window.viewport_size();
            let preview = shell.handle.preview_bounds.get();
            let content = shell.content_bounds.get();
            let zoom = shell.preview.read(cx).zoom();
            let page = shell
                .compile
                .document
                .as_ref()
                .map(|d| d.page_size_pt(0).0)
                .unwrap_or(0.);
            (
                viewport,
                preview,
                content,
                zoom,
                page,
                shell.settings.split_ratio,
            )
        });
        if let Some((viewport, preview, content, zoom, page, ratio)) = geometry {
            let close = |a: f32, b: f32| (a - b).abs() < 1.5;
            let top = f32::from(preview.origin.y);
            let left = f32::from(preview.origin.x);
            let right = left + f32::from(preview.size.width);
            let bottom = top + f32::from(preview.size.height);
            report.check(
                close(top, crate::layout::HEADER_HEIGHT)
                    && close(right, f32::from(viewport.width))
                    && close(bottom, f32::from(viewport.height))
                    && close(
                        left,
                        f32::from(content.origin.x)
                            + crate::layout::clamp_split(ratio, f32::from(content.size.width))
                                * f32::from(content.size.width),
                    ),
                format!(
                    "the preview fills the right column below the header ({left:.0},{top:.0})–({right:.0},{bottom:.0}) in a {:.0}×{:.0} window",
                    f32::from(viewport.width),
                    f32::from(viewport.height)
                ),
            );
            let sheet = zoom * page;
            let margin = (f32::from(preview.size.width) - sheet) / 2.;
            report.check(
                sheet > 0. && margin >= oyl_preview::layout::MARGIN_X - 0.5,
                format!("the sheet fits its width with equal margins of {margin:.0} px"),
            );
        }
        let dark = cx.update(|cx| {
            use oyl_ui::prelude::*;
            cx.tokens().dark
        });
        self.dispatch(Box::new(actions::ToggleAppearance), cx);
        let flipped = cx.update(|cx| {
            use oyl_ui::prelude::*;
            cx.tokens().dark
        });
        report.check(
            flipped != dark,
            if flipped {
                "dark appearance"
            } else {
                "light appearance"
            },
        );
        self.dispatch(Box::new(actions::ToggleAppearance), cx);
        let back = cx.update(|cx| {
            use oyl_ui::prelude::*;
            cx.tokens().dark
        });
        report.check(back == dark, "and back");
    }

    // -- 3b. The Agent tab, the cards, the picker --------------------------------

    /// The tab on screen, and whether the composer or the tab's content has
    /// focus.
    fn tab_state(&self, cx: &mut AsyncApp) -> (LayoutMode, bool, bool) {
        self.with_shell(cx, |shell, window, cx| {
            use gpui_kit::Focusable as _;
            let chat = shell.chat_has_focus(window, cx);
            let content = match shell.mode {
                LayoutMode::Agent => chat,
                LayoutMode::Write => shell.editor.focus_handle(cx).is_focused(window),
            };
            (shell.mode, chat, content)
        })
        .unwrap_or((LayoutMode::Write, false, false))
    }

    async fn settle(&self, cx: &mut AsyncApp) {
        self.wait(cx, Duration::from_millis(250), |_| false).await;
    }

    /// The handle dragged `dx` points, as a user would.
    fn drag_handle(&self, dx: f32, cx: &mut AsyncApp) {
        self.frame(cx);
        let handle = cx.update(|cx| self.shell.read(cx).handle.hit_bounds.get());
        let from = point(
            handle.origin.x + handle.size.width / 2.,
            handle.origin.y + handle.size.height / 2.,
        );
        let to = point(from.x + gpui_kit::px(dx), from.y);
        let _ = self.window.update(cx, move |_, window, cx| {
            for event in [
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: from,
                    pressed_button: None,
                    modifiers: Modifiers::default(),
                }),
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: from,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: to,
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers::default(),
                }),
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: to,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                }),
            ] {
                window.dispatch_event(event, cx);
            }
        });
        self.frame(cx);
    }

    async fn agent_tab(&self, report: &mut Report, cx: &mut AsyncApp) {
        report.step("3b. The Agent tab, the cards, the picker");
        let origins: [(Box<dyn gpui_kit::Action>, LayoutMode); 1] =
            [(Box::new(actions::WriteMode), LayoutMode::Write)];
        for (action, origin) in origins {
            let name = crate::layout::mode_label(origin);
            self.dispatch(action, cx);
            self.settle(cx).await;
            self.dispatch(Box::new(actions::FocusAgent), cx);
            self.settle(cx).await;
            let (mode, chat, _) = self.tab_state(cx);
            report.check(
                mode == LayoutMode::Agent && chat,
                format!("from {name}: ⌘L goes to the Agent tab, caret in the composer"),
            );
            self.dispatch(Box::new(actions::FocusAgent), cx);
            self.settle(cx).await;
            let (mode, _, content) = self.tab_state(cx);
            report.check(
                mode == origin && content,
                format!("⌘L again goes back to {name}, focus on its content"),
            );
            self.dispatch(Box::new(actions::FocusAgent), cx);
            self.press("escape", cx);
            self.settle(cx).await;
            let (mode, _, content) = self.tab_state(cx);
            report.check(
                mode == origin && content,
                format!("Esc in the composer goes back to {name}"),
            );
        }

        // The editor's scroll viewport ends inside the card in Write; the
        // cards keep their gaps.
        for (action, mode) in [
            (
                Box::new(actions::WriteMode) as Box<dyn gpui_kit::Action>,
                LayoutMode::Write,
            ),
            (Box::new(actions::AgentMode), LayoutMode::Agent),
        ] {
            self.dispatch(action, cx);
            self.settle(cx).await;
            let measured = self.with_shell(cx, |shell, window, cx| {
                let card = shell.card_bounds.get();
                let text = shell.editor.read(cx).text_bounds(cx);
                (
                    card,
                    text,
                    window.viewport_size(),
                    shell.content_bounds.get(),
                    shell.settings.split_ratio,
                )
            });
            let Some((card, text, viewport, content, ratio)) = measured else {
                continue;
            };
            let edges = |b: Bounds<Pixels>| {
                (
                    f32::from(b.origin.x),
                    f32::from(b.origin.y),
                    f32::from(b.origin.x + b.size.width),
                    f32::from(b.origin.y + b.size.height),
                )
            };
            let (cl, ct, cr, cb) = edges(card);
            if mode != LayoutMode::Agent {
                let Some(text) = text else {
                    report.check(false, "the editor's text area was laid out");
                    continue;
                };
                let (tl, tt, tr, tb) = edges(text);
                // The editor's own padding (oyl-editor): top, right, bottom,
                // left; the scroll viewport (and its clip) starts inside it.
                let (pt, pr, pb, pl) = (16., 20., 16., 12.);
                let inside = tl >= cl + pl - 0.5
                    && tt >= ct + pt - 0.5
                    && tr <= cr - pr + 0.5
                    && tb <= cb - pb + 0.5;
                report.check(
                    inside && tr > tl && tb > tt,
                    format!(
                        "{}: the editor's scroll viewport ({tl:.0},{tt:.0})–({tr:.0},{tb:.0}) is inside the card's inner rect ({cl:.0},{ct:.0})–({cr:.0},{cb:.0}) by its padding",
                        crate::layout::mode_label(mode)
                    ),
                );
            }
            let gap = crate::layout::CARD_GAP;
            let width = f32::from(content.size.width);
            let boundary = f32::from(content.origin.x)
                + crate::layout::clamp_columns(mode, ratio, width) * width;
            let (left, right) = match mode {
                LayoutMode::Agent => (gap + crate::layout::SIDEBAR_WIDTH + gap, boundary - gap),
                LayoutMode::Write => (gap, boundary - gap),
            };
            let near = |a: f32, b: f32| (a - b).abs() < 1.5;
            report.check(
                near(cl, left)
                    && near(ct, crate::layout::HEADER_HEIGHT + gap)
                    && near(cb, f32::from(viewport.height) - gap)
                    && near(cr, right),
                format!(
                    "{}: the card ({cl:.0},{ct:.0})–({cr:.0},{cb:.0}) keeps {gap:.0}px from the edges, the header{}",
                    crate::layout::mode_label(mode),
                    match mode {
                        LayoutMode::Agent => ", the sidebar and the column boundary",
                        LayoutMode::Write => " and the column boundary",
                    }
                ),
            );
        }

        // In the Agent tab the handle sizes the agent against the paper.
        let (before, card_before) = cx.update(|cx| {
            let shell = self.shell.read(cx);
            (shell.settings.split_ratio, shell.card_bounds.get())
        });
        self.drag_handle(120., cx);
        self.settle(cx).await;
        let (after, card_after) = cx.update(|cx| {
            let shell = self.shell.read(cx);
            (shell.settings.split_ratio, shell.card_bounds.get())
        });
        let grew = f32::from(card_after.size.width) - f32::from(card_before.size.width);
        report.check(
            after > before + 0.05 && (grew - 120.).abs() < 2.,
            format!(
                "Agent tab: dragging the handle resizes the agent vs the paper ({before:.2} → {after:.2}, card {grew:+.0} px)"
            ),
        );
        self.with_shell(cx, move |shell, _, cx| shell.set_split_ratio(before, cx));
        self.settle(cx).await;

        // The picker opens on a click; a model picked there is used.
        let chat = cx.update(|cx| self.shell.read(cx).chat.clone());
        let chip = cx.update(|cx| chat.read(cx).picker_bounds());
        self.click(chip, cx);
        let opened = cx.update(|cx| chat.read(cx).open_popover());
        report.check(
            opened == Some("agent"),
            "a click on the composer's chip opens the picker",
        );
        let before = cx.update(|cx| chat.read(cx).selection());
        self.dispatch(Box::new(oyl_chat::PopoverDown), cx);
        self.dispatch(Box::new(oyl_chat::PopoverConfirm), cx);
        let (after, open) =
            cx.update(|cx| (chat.read(cx).selection(), chat.read(cx).open_popover()));
        let model = after.as_ref().and_then(|(_, model)| model.clone());
        report.check(
            open.is_none() && model.as_deref() == Some("script-careful") && before != after,
            format!(
                "↓ Enter picked the model “{}” and closed the picker",
                model.as_deref().unwrap_or("none")
            ),
        );
        let saved = cx.update(|cx| {
            self.shell
                .read(cx)
                .settings
                .model_for("self-test")
                .map(str::to_owned)
        });
        report.check(
            saved.as_deref() == Some("script-careful"),
            "the model choice is remembered",
        );

        // One list holds every model of every harness; the search narrows
        // it; one pick switches the harness and the model.
        self.click(chip, cx);
        let expected: Vec<(HarnessId, Option<String>)> = cx.update(|cx| {
            self.shell
                .read(cx)
                .agent
                .harnesses
                .iter()
                .flat_map(|harness| {
                    let info = harness.info();
                    if info.models.is_empty() {
                        vec![(info.id, None)]
                    } else {
                        info.models
                            .iter()
                            .map(|model| (info.id.clone(), Some(model.id.clone())))
                            .collect()
                    }
                })
                .collect()
        });
        let (listed, search) = cx.update(|cx| {
            let chat = chat.read(cx);
            (chat.picker_models(), chat.picker_has_search())
        });
        let listed: Vec<(HarnessId, Option<String>)> = listed
            .into_iter()
            .map(|model| (model.harness, model.model))
            .collect();
        report.check(
            listed == expected && expected.len() > 6 && search,
            format!(
                "the picker lists all {} models of every harness ({} listed), with a search field",
                expected.len(),
                listed.len()
            ),
        );
        self.type_text("rapid", cx);
        let narrowed: Vec<String> = cx.update(|cx| {
            chat.read(cx)
                .picker_models()
                .into_iter()
                .map(|model| model.name)
                .collect()
        });
        report.check(
            narrowed == ["Rapid"],
            format!("typing “rapid” narrows the list to {narrowed:?}"),
        );
        self.press("enter", cx);
        let (selection, open) =
            cx.update(|cx| (chat.read(cx).selection(), chat.read(cx).open_popover()));
        report.check(
            open.is_none()
                && selection == Some((HarnessId::new(QUICK_ID), Some("quick-rapid".into()))),
            format!("Enter picks another harness's model: harness and model switch together ({selection:?})"),
        );
        self.click(chip, cx);
        self.type_text("careful", cx);
        self.press("enter", cx);
        let selection = cx.update(|cx| chat.read(cx).selection());
        report.check(
            selection == Some((HarnessId::new("self-test"), Some("script-careful".into()))),
            "and back to the self-test agent's careful model the same way",
        );
        self.click(chip, cx);
        self.press("escape", cx);
        let open = cx.update(|cx| chat.read(cx).open_popover());
        let (mode, ..) = self.tab_state(cx);
        report.check(
            open.is_none() && mode == LayoutMode::Agent,
            "Esc closes the picker first, the Agent tab stays",
        );
        self.press("escape", cx);
        self.settle(cx).await;
        let (mode, ..) = self.tab_state(cx);
        report.check(
            mode == LayoutMode::Write,
            "a second Esc goes back to the tab it came from (Write)",
        );
    }

    // -- 4. An agent turn ------------------------------------------------------

    async fn agent_turn(
        &self,
        report: &mut Report,
        _harness: Arc<dyn Harness>,
        article: &Path,
        cx: &mut AsyncApp,
    ) {
        report.step("4. An agent turn edits the folder; history; undo");
        let main = article.join("main.typ");
        let before = self.snapshot(cx);
        let chat = cx.update(|cx| self.shell.read(cx).chat.clone());
        cx.update(|cx| chat.update(cx, |chat, cx| chat.send(AGENT_PROMPT, cx)));
        let picked_up = self
            .wait_app(cx, Duration::from_secs(10), |cx| {
                let editor = self.shell.read(cx).editor.clone();
                editor
                    .read(cx)
                    .text(&main, cx)
                    .is_some_and(|text| text.contains(AGENT_SENTENCE))
            })
            .await;
        report.check(
            picked_up,
            "the agent's edit on disk reached the buffer (folder watcher)",
        );
        let recompiled = self
            .wait(cx, Duration::from_secs(20), |s| {
                s.compiles > before.compiles
            })
            .await;
        report.check(recompiled, "the paper recompiled after the agent's edit");
        let finished = self
            .wait_app(cx, Duration::from_secs(20), |cx| {
                let shell = self.shell.read(cx);
                !shell.chat.read(cx).is_running()
                    && shell
                        .paper
                        .as_ref()
                        .is_some_and(|p| !p.turns.turns.is_empty())
            })
            .await;
        report.check(finished, "the turn finished and its version was recorded");
        let history = cx.update(|cx| {
            self.shell
                .read(cx)
                .paper
                .as_ref()
                .and_then(|p| p.history.clone())
        });
        let versions = match history {
            Some(history) => cx
                .background_spawn(async move { history.versions() })
                .await
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let agent_version = versions
            .iter()
            .find(|v| v.author == Author::Agent && v.label.as_deref() == Some(AGENT_PROMPT));
        report.check(
            agent_version.is_some(),
            format!(
                "history has an Agent version labelled with the prompt ({} versions)",
                versions.len()
            ),
        );
        let before_turn = versions
            .iter()
            .skip_while(|v| v.author != Author::Agent)
            .nth(1)
            .map(|v| format!("version {} by {}", v.number, v.author.label()));
        report.check(
            before_turn.is_some(),
            format!(
                "and the user's version before it ({})",
                before_turn.as_deref().unwrap_or("missing")
            ),
        );

        let turn = cx.update(|cx| {
            self.shell
                .read(cx)
                .paper
                .as_ref()
                .and_then(|p| p.turns.turns.keys().next().cloned())
        });
        if let Some(turn) = turn {
            self.with_shell(cx, move |shell, window, cx| {
                shell.undo_turn(turn, window, cx)
            });
        }
        let undone = self
            .wait_app(cx, Duration::from_secs(10), |cx| {
                let editor = self.shell.read(cx).editor.clone();
                editor
                    .read(cx)
                    .text(&main, cx)
                    .is_some_and(|text| !text.contains(AGENT_SENTENCE))
            })
            .await;
        let disk = fs::read_to_string(&main).unwrap_or_default();
        report.check(
            undone && !disk.contains(AGENT_SENTENCE) && disk.contains(TYPED),
            "Undo restored the text (buffer and disk), keeping the user's own typing",
        );
    }

    // -- 5. A pasted image, a dropped bibliography -----------------------------

    async fn media(&self, report: &mut Report, article: &Path, cx: &mut AsyncApp) {
        report.step("5. A pasted PNG becomes a figure; a dropped .bib adds sources");
        let main = article.join("main.typ");
        let png = match test_png() {
            Ok(png) => png,
            Err(err) => {
                report.check(false, format!("a PNG to paste: {err}"));
                return;
            }
        };
        let figures_before = count_files(&article.join("figures"), "png");
        self.with_shell(cx, move |shell, window, cx| {
            shell.handle_editor_event(
                EditorEvent::PastedImage {
                    bytes: png,
                    extension: "png".into(),
                },
                window,
                cx,
            )
        });
        let inserted = self
            .wait_app(cx, Duration::from_secs(10), |cx| {
                let editor = self.shell.read(cx).editor.clone();
                editor
                    .read(cx)
                    .text(&main, cx)
                    .is_some_and(|text| text.contains("#figure(image(\"figures/"))
            })
            .await;
        report.check(
            count_files(&article.join("figures"), "png") > figures_before,
            "the image is in figures/",
        );
        report.check(inserted, "a figure block was inserted at the cursor");

        let bib = article
            .parent()
            .unwrap_or(article)
            .join("dropped-sources.bib");
        let written = fs::write(
            &bib,
            "@article{lovelace1843,\n  title = {Sketch of the Analytical Engine},\n  author = {Lovelace, Ada},\n  journal = {Scientific Memoirs},\n  year = {1843},\n}\n\n@book{turing1950,\n  title = {Computing Machinery and Intelligence},\n  author = {Turing, Alan},\n  year = {1950},\n}\n",
        );
        report.check(written.is_ok(), "a .bib to drop");
        let before = self.snapshot(cx).citations;
        let dropped = bib.clone();
        self.with_shell(cx, move |shell, window, cx| {
            shell.handle_editor_event(
                EditorEvent::DroppedFiles {
                    paths: vec![dropped],
                },
                window,
                cx,
            )
        });
        let grew = self
            .wait(cx, Duration::from_secs(10), |s| s.citations >= before + 2)
            .await;
        let after = self.snapshot(cx).citations;
        report.check(
            grew,
            format!("citation candidates grew from {before} to {after}"),
        );
    }

    // -- 6. Exports ---------------------------------------------------------------

    async fn exports(&self, report: &mut Report, cx: &mut AsyncApp) {
        report.step("6. Export the PDF and the sources");
        let out = self
            .paths
            .data_dir
            .parent()
            .unwrap_or(&self.paths.data_dir)
            .join("exports");
        let pdf = out.join("paper.pdf");
        let zip = out.join("paper.zip");
        let bib = out.join("paper.bib");
        for (what, dest) in [
            ("PDF", pdf.clone()),
            ("source", zip.clone()),
            ("bibliography", bib.clone()),
        ] {
            let task = self.with_shell(cx, move |shell, _, cx| shell.export_to(what, dest, cx));
            let result = match task {
                Some(task) => task.await,
                None => Err(anyhow::anyhow!("the window is gone")),
            };
            report.check(
                result.is_ok(),
                format!(
                    "{what} exported{}",
                    match &result {
                        Ok(_) => String::new(),
                        Err(err) => format!(": {err:#}"),
                    }
                ),
            );
        }
        let pdf_bytes = fs::read(&pdf).unwrap_or_default();
        report.check(
            pdf_bytes.starts_with(b"%PDF-") && pdf_bytes.len() > 1000,
            format!("the PDF is a PDF ({} bytes)", pdf_bytes.len()),
        );
        let zip_bytes = fs::read(&zip).unwrap_or_default();
        let has = |name: &[u8]| zip_bytes.windows(name.len()).any(|w| w == name);
        report.check(
            zip_bytes.starts_with(b"PK\x03\x04") && has(b"main.typ") && has(b"refs.bib"),
            format!(
                "the zip is a zip with main.typ and refs.bib ({} bytes)",
                zip_bytes.len()
            ),
        );
        let bib_text = fs::read_to_string(&bib).unwrap_or_default();
        report.check(
            bib_text.contains("lovelace1843"),
            "the exported bibliography has the dropped sources",
        );
    }

    // -- 7. The palette ----------------------------------------------------------

    fn showing(&self, id: &str, cx: &mut AsyncApp) -> bool {
        let id = id.to_string();
        cx.update(|cx| self.shell.read(cx).overlays.read(cx).is_showing(&id))
    }

    fn dismiss_overlay(&self, cx: &mut AsyncApp) {
        self.with_shell(cx, |shell, window, cx| {
            shell
                .overlays
                .update(cx, |host, cx| host.dismiss(window, cx))
        });
        self.frame(cx);
    }

    fn count_in(&self, path: &Path, needle: &str, cx: &mut AsyncApp) -> usize {
        self.editor_text(path, cx).matches(needle).count()
    }

    async fn palette(&self, report: &mut Report, article: &Path, cx: &mut AsyncApp) {
        report.step("7. The command palette ranks and runs");
        let main = article.join("main.typ");
        self.dispatch(Box::new(actions::CommandPalette), cx);
        let palette = cx.update(|cx| self.shell.read(cx).palette.clone());
        let entries = cx.update(|cx| palette.read(cx).entries().len());
        report.check(
            self.showing("palette", cx) && entries > 20,
            format!("⌘K shows the palette ({entries} entries)"),
        );
        let first = |query: &'static str, this: &Self, cx: &mut AsyncApp| -> String {
            let view = palette.clone();
            this.with_shell(cx, move |_, window, cx| {
                view.update(cx, |palette, cx| palette.set_query(query, window, cx))
            });
            this.frame(cx);
            cx.update(|cx| {
                palette
                    .read(cx)
                    .shown()
                    .first()
                    .map(|entry| entry.label.clone())
                    .unwrap_or_default()
            })
        };
        let export = first("expdf", self, cx);
        report.check(
            export == "Export PDF…",
            format!("“expdf” ranks “{export}” first"),
        );
        let section = first("intro", self, cx);
        report.check(
            section.trim() == "Introduction",
            format!("“intro” ranks the section “{}” first", section.trim()),
        );
        let cite = first("lovelace", self, cx);
        report.check(
            cite == "Cite Lovelace 1843",
            format!("“lovelace” ranks “{cite}” first"),
        );
        let query = cx.update(|cx| palette.read(cx).query().to_string());
        report.check(query == "lovelace", "the palette keeps the query typed");
        let before = self.count_in(&main, "@lovelace1843", cx);
        self.dispatch(
            Box::new(gpui_kit::base::actions::Confirm { secondary: false }),
            cx,
        );
        let cited = self
            .wait_app(cx, Duration::from_secs(5), |cx| {
                let editor = self.shell.read(cx).editor.clone();
                editor
                    .read(cx)
                    .text(&main, cx)
                    .is_some_and(|text| text.matches("@lovelace1843").count() > before)
            })
            .await;
        report.check(
            cited && !self.showing("palette", cx),
            "Enter inserted the citation and closed the palette",
        );
        self.dispatch(Box::new(actions::CommandPalette), cx);
        first("agent tab", self, cx);
        self.dispatch(
            Box::new(gpui_kit::base::actions::Confirm { secondary: false }),
            cx,
        );
        let mode = self.snapshot(cx).mode;
        report.check(
            mode == Some(LayoutMode::Agent) && !self.showing("palette", cx),
            "a command picked in the palette runs (Agent tab)",
        );
        self.dispatch(Box::new(actions::WriteMode), cx);
    }

    // -- 8. The panels -------------------------------------------------------------

    async fn panels(&self, report: &mut Report, article: &Path, cx: &mut AsyncApp) {
        report.step("8. The five panels and the source PDF");
        let main = article.join("main.typ");
        let Some(panels) = cx.update(|cx| {
            self.shell.read(cx).panels.as_ref().map(|p| {
                (
                    p.sources.clone(),
                    p.history.clone(),
                    p.outline.clone(),
                    p.figures.clone(),
                    p.problems.clone(),
                )
            })
        }) else {
            report.check(false, "the paper has its panels");
            return;
        };
        let (sources, history, outline, figures, problems) = panels;

        // Sources: every source, and a citation through the panel.
        let expected = self.snapshot(cx).citations;
        self.dispatch(Box::new(actions::ShowSources), cx);
        let loaded = self
            .wait_app(cx, Duration::from_secs(10), |cx| {
                sources.read(cx).source_count() == expected
            })
            .await;
        let count = cx.update(|cx| sources.read(cx).source_count());
        report.check(
            self.showing("sources", cx) && loaded,
            format!("⇧⌘R shows Sources with all {count} sources"),
        );
        self.type_text("lovelace", cx);
        let filtered = self
            .wait_app(cx, Duration::from_secs(5), |cx| {
                sources.read(cx).visible_keys() == vec!["lovelace1843".to_string()]
            })
            .await;
        report.check(filtered, "typing filters the sources");
        let before = self.count_in(&main, "@lovelace1843", cx);
        self.dispatch(Box::new(oyl_panels::Confirm), cx);
        let cited = self
            .wait_app(cx, Duration::from_secs(5), |cx| {
                let editor = self.shell.read(cx).editor.clone();
                editor
                    .read(cx)
                    .text(&main, cx)
                    .is_some_and(|text| text.matches("@lovelace1843").count() > before)
            })
            .await;
        report.check(
            cited && !self.showing("sources", cx),
            "InsertCitation reached the buffer and the panel closed",
        );

        // The source's PDF, read inside the app.
        let pdf = self
            .paths
            .data_dir
            .parent()
            .unwrap_or(&self.paths.data_dir)
            .join("exports/paper.pdf");
        cx.update(|cx| {
            sources.update(cx, |_, cx| {
                cx.emit(oyl_panels::SourcesEvent::OpenPdf {
                    key: "lovelace1843".into(),
                    path: pdf,
                })
            })
        });
        let viewer = cx.update(|cx| {
            self.shell
                .read(cx)
                .panels
                .as_ref()
                .and_then(|p| p.pdf.clone())
        });
        let read = match &viewer {
            Some(viewer) => {
                self.wait_app(cx, Duration::from_secs(10), |cx| {
                    viewer.read(cx).is_loaded() && viewer.read(cx).page_count(cx) >= 1
                })
                .await
            }
            None => false,
        };
        let key = viewer
            .as_ref()
            .map(|viewer| cx.update(|cx| viewer.read(cx).key().to_string()))
            .unwrap_or_default();
        report.check(
            read && self.showing("pdf", cx) && key == "lovelace1843",
            "OpenPdf shows the PDF in a card, titled with its key",
        );
        if let Some(viewer) = viewer {
            cx.update(|cx| {
                viewer.update(cx, |viewer, cx| {
                    let page = viewer.page(cx);
                    let key = viewer.key().to_string();
                    cx.emit(crate::pdf_viewer::PdfViewerEvent::CitePage { key, page })
                })
            });
        }
        let paged = self
            .wait_app(cx, Duration::from_secs(5), |cx| {
                let editor = self.shell.read(cx).editor.clone();
                editor
                    .read(cx)
                    .text(&main, cx)
                    .is_some_and(|text| text.contains("@lovelace1843[p. 1]"))
            })
            .await;
        report.check(paged, "“Cite page 1” inserted @lovelace1843[p. 1]");

        // Outline: the template's headings; a jump lands on the line.
        let headings = cx.update(|cx| {
            self.shell
                .read(cx)
                .paper
                .as_ref()
                .map(|p| p.outline.clone())
                .unwrap_or_default()
        });
        self.dispatch(Box::new(actions::ShowOutline), cx);
        let shown = cx.update(|cx| outline.read(cx).shown().len());
        report.check(
            self.showing("outline", cx) && shown == headings.len() && shown > 0,
            format!("⇧⌘O shows Outline with the {shown} headings of the template"),
        );
        let target = cx.update(|cx| {
            let panel = outline.read(cx);
            let shown = panel.shown();
            panel
                .selected()
                .filter(|ix| shown.contains(ix))
                .or_else(|| shown.first().copied())
        });
        self.dispatch(Box::new(oyl_panels::Confirm), cx);
        let wanted = target
            .and_then(|ix| headings.get(ix))
            .map(|h| h.location.line);
        let landed = self
            .wait_app(cx, Duration::from_secs(5), |cx| {
                let editor = self.shell.read(cx).editor.clone();
                editor.read(cx).cursor(cx).map(|c| c.line) == wanted
            })
            .await;
        report.check(
            landed && wanted.is_some() && self.showing("outline", cx),
            format!(
                "Jump put the cursor on line {} and the outline stayed",
                wanted.unwrap_or(0)
            ),
        );
        self.dismiss_overlay(cx);

        // Figures: the pasted image.
        self.dispatch(Box::new(actions::ShowFigures), cx);
        let listed = self
            .wait_app(cx, Duration::from_secs(10), |cx| {
                !figures.read(cx).figures().is_empty()
            })
            .await;
        let count = cx.update(|cx| figures.read(cx).figures().len());
        report.check(
            self.showing("figures", cx) && listed,
            format!("⇧⌘F shows Figures with {count} image(s)"),
        );
        self.dispatch(Box::new(actions::ShowFigures), cx);
        report.check(!self.showing("figures", cx), "the same key closes it");

        // Problems: none, then the new error; "Fix with agent".
        self.dispatch(Box::new(actions::ShowProblems), cx);
        let errors = |cx: &App| {
            problems
                .read(cx)
                .diagnostics()
                .iter()
                .filter(|d| d.is_error())
                .count()
        };
        let clean = cx.update(|cx| errors(cx));
        report.check(
            self.showing("problems", cx) && clean == 0,
            "⇧⌘M shows Problems: no errors",
        );
        let compiles = self.snapshot(cx).compiles;
        let broken = format!("\n#{MISSING}_again\n");
        self.with_shell(cx, move |shell, window, cx| {
            shell.editor.update(cx, |editor, cx| {
                editor.insert_at_cursor(&broken, window, cx)
            })
        });
        let reported = self
            .wait(cx, Duration::from_secs(20), |s| {
                s.compiles > compiles && s.errors > 0
            })
            .await;
        let now = cx.update(|cx| errors(cx));
        report.check(
            reported && now >= 1,
            format!("after an error, Problems lists {now} error(s)"),
        );
        let prompt = cx.update(|cx| problems.read(cx).fix_prompt());
        let turns = cx.update(|cx| self.shell.read(cx).chat.read(cx).thread().turns.len());
        if let Some(prompt) = prompt.clone() {
            cx.update(|cx| {
                problems.update(cx, |_, cx| {
                    cx.emit(oyl_panels::ProblemsEvent::FixWithAgent { prompt })
                })
            });
        }
        let started = self
            .wait_app(cx, Duration::from_secs(5), |cx| {
                let chat = self.shell.read(cx).chat.read(cx);
                chat.thread().turns.len() > turns
                    && chat.thread().turns.last().map(|t| Some(t.prompt.clone()))
                        == Some(prompt.clone())
            })
            .await;
        report.check(
            prompt.is_some() && started && !self.showing("problems", cx),
            "Fix with agent started a turn with the errors",
        );
        let (mode, ..) = self.tab_state(cx);
        report.check(mode == LayoutMode::Agent, "and switched to the Agent tab");
        let done = self
            .wait_app(cx, Duration::from_secs(20), |cx| {
                !self.shell.read(cx).chat.read(cx).is_running()
            })
            .await;
        report.check(done, "the turn finished");

        // History: versions, and a restore that reloads the buffer.
        self.dispatch(Box::new(actions::ShowHistory), cx);
        let versions = self
            .wait_app(cx, Duration::from_secs(10), |cx| {
                history.read(cx).versions().len() >= 2
            })
            .await;
        let count = cx.update(|cx| history.read(cx).versions().len());
        report.check(
            self.showing("history", cx) && versions,
            format!("⇧⌘H shows History with {count} versions"),
        );
        cx.update(|cx| history.update(cx, |panel, cx| panel.select_number(1, cx)));
        self.frame(cx);
        let wide = cx.update(|cx| f32::from(history.read(cx).preferred_width()));
        report.check(
            wide > 420.,
            format!("a selected version widens History to {wide:.0} px"),
        );
        self.dispatch(Box::new(oyl_panels::Confirm), cx);
        self.dispatch(Box::new(oyl_panels::Confirm), cx);
        let restored = self
            .wait_app(cx, Duration::from_secs(10), |cx| {
                let editor = self.shell.read(cx).editor.clone();
                editor
                    .read(cx)
                    .text(&main, cx)
                    .is_some_and(|text| !text.contains(TYPED) && !text.contains("lovelace1843"))
            })
            .await;
        let toast = cx.update(|cx| {
            self.shell
                .read(cx)
                .toasts
                .iter()
                .any(|(_, text)| text == "Restored version 1")
        });
        report.check(
            restored && toast,
            "restoring version 1 reloaded the buffer (“Restored version 1”)",
        );
        self.dismiss_overlay(cx);
    }

    // -- 8b. Two threads at once ----------------------------------------------------

    async fn threads(&self, report: &mut Report, cx: &mut AsyncApp) {
        report.step("8b. Two threads run at once; the sidebar lists them; delete");
        let chat = cx.update(|cx| self.shell.read(cx).chat.clone());
        let sidebar = cx.update(|cx| self.shell.read(cx).sidebar.clone());
        const FIRST: &str = "First thread: tighten the abstract.";
        const SECOND: &str = "Second thread: shorten the introduction.";
        self.dispatch(Box::new(actions::AgentMode), cx);
        self.settle(cx).await;
        // ⌘N inside the Agent tab starts a thread.
        self.press("cmd-n", cx);
        let fresh = cx.update(|cx| chat.read(cx).thread().is_empty());
        report.check(
            fresh && !self.showing("library", cx),
            "⌘N in the Agent tab starts a new thread (not a paper)",
        );
        cx.update(|cx| chat.update(cx, |chat, cx| chat.send(FIRST, cx)));
        self.settle(cx).await;
        let first = cx.update(|cx| chat.read(cx).thread().id.clone());
        self.press("cmd-n", cx);
        cx.update(|cx| chat.update(cx, |chat, cx| chat.send(SECOND, cx)));
        self.settle(cx).await;
        let second = cx.update(|cx| chat.read(cx).thread().id.clone());
        let overview = cx.update(|cx| sidebar.read(cx).overview(cx));
        let titles: Vec<String> = overview.running.iter().map(|r| r.title.clone()).collect();
        let on_top: Vec<&str> = overview
            .recent
            .iter()
            .take(2)
            .map(|summary| summary.id.as_str())
            .collect();
        report.check(
            cx.update(|cx| chat.read(cx).running_count()) == 2
                && titles.iter().any(|t| t.starts_with("First thread"))
                && titles.iter().any(|t| t.starts_with("Second thread"))
                && on_top.contains(&first.as_str())
                && on_top.contains(&second.as_str()),
            format!(
                "two turns run at once, both on top of the list with a running dot: {titles:?}"
            ),
        );
        let activity = overview
            .running
            .iter()
            .map(|r| r.activity.clone())
            .collect::<Vec<_>>();
        report.check(
            activity.iter().all(|a| !a.is_empty()),
            format!("each with its live activity: {activity:?}"),
        );
        // Back to the first while both stream.
        let id = first.clone();
        self.with_shell(cx, move |shell, window, cx| {
            shell
                .chat
                .update(cx, |chat, cx| chat.show_thread(&id, window, cx))
        });
        let (shown, live) = cx.update(|cx| {
            let chat = chat.read(cx);
            (chat.thread().id.clone(), chat.current_is_running())
        });
        report.check(
            shown == first && live,
            "switching to the first thread while it streams shows it live",
        );
        let header_dot = cx.update(|cx| self.shell.read(cx).agent.running);
        report.check(header_dot, "the Agent tab's dot shows that turns run");
        let done = self
            .wait_app(cx, Duration::from_secs(20), |cx| {
                !chat.read(cx).is_running()
            })
            .await;
        report.check(done, "both turns finished");
        let overview = cx.update(|cx| sidebar.read(cx).overview(cx));
        let recent: Vec<&str> = overview.recent.iter().map(|r| r.id.as_str()).collect();
        report.check(
            overview.running.is_empty()
                && recent.contains(&first.as_str())
                && recent.contains(&second.as_str()),
            format!("both stay listed once done ({} threads)", recent.len()),
        );
        let turns = cx.update(|cx| {
            self.shell
                .read(cx)
                .paper
                .as_ref()
                .map(|p| p.turns.turns.len())
                .unwrap_or(0)
        });
        report.check(
            turns >= 2,
            format!("each turn recorded its own version ({turns} turns)"),
        );
        // Rename and delete the second.
        let id = second.clone();
        cx.update(|cx| chat.update(cx, |chat, cx| chat.rename_thread(&id, "Intro pass", cx)));
        let renamed = cx.update(|cx| {
            sidebar
                .read(cx)
                .overview(cx)
                .recent
                .iter()
                .any(|r| r.id == second && r.title == "Intro pass")
        });
        report.check(renamed, "a thread can be renamed");
        let id = second.clone();
        cx.update(|cx| chat.update(cx, |chat, cx| chat.delete_thread(&id, cx)));
        let gone = cx.update(|cx| {
            !sidebar
                .read(cx)
                .overview(cx)
                .recent
                .iter()
                .any(|r| r.id == second)
        });
        let file = self
            .snapshot(cx)
            .root
            .map(|root| root.join(".oyl/chat").join(format!("{second}.json")));
        report.check(
            gone && file.is_some_and(|file| !file.exists()),
            "deleting a thread removes it from the list and from disk",
        );
        self.dispatch(Box::new(actions::WriteMode), cx);
    }

    // -- 8c. Background agents --------------------------------------------------

    async fn background_agents(&self, report: &mut Report, cx: &mut AsyncApp) {
        report.step("8c. A long task runs in the background; a quick question meanwhile");
        const LONG: &str = "Long task: look up how owls hear.";
        const QUICK: &str = "Quick question: what is an owl?";
        let chat = cx.update(|cx| self.shell.read(cx).chat.clone());
        let sidebar = cx.update(|cx| self.shell.read(cx).sidebar.clone());
        let before = cx.update(|cx| chat.read(cx).selection());
        self.dispatch(Box::new(actions::AgentMode), cx);
        self.settle(cx).await;
        let quick_agent = HarnessId::new(QUICK_ID);
        let agent = quick_agent.clone();
        cx.update(|cx| {
            chat.update(cx, |chat, cx| {
                chat.select(&agent, Some(QuickHarness::LONG.into()), cx)
            })
        });
        self.press("cmd-n", cx);
        cx.update(|cx| chat.update(cx, |chat, cx| chat.send(LONG, cx)));
        self.settle(cx).await;
        let long = cx.update(|cx| chat.read(cx).thread().id.clone());

        // ⌘N: a fresh thread at once, the caret in its composer.
        self.press("cmd-n", cx);
        let (fresh, focused, running) = self
            .window
            .update(cx, |_, window, cx| {
                let chat = chat.read(cx);
                (
                    chat.thread().is_empty(),
                    chat.focus_handle(cx).is_focused(window),
                    chat.running_count(),
                )
            })
            .unwrap_or((false, false, 0));
        report.check(
            fresh && focused && running == 1,
            "⌘N while a long task runs opens a fresh thread with the composer focused; the task keeps going",
        );
        let agent = quick_agent.clone();
        cx.update(|cx| chat.update(cx, |chat, cx| chat.select(&agent, Some("quick".into()), cx)));

        // The picker offers the harness's efforts; one click picks one.
        let chip = cx.update(|cx| chat.read(cx).picker_bounds());
        self.click(chip, cx);
        let (offered, pill) = cx.update(|cx| {
            let chat = chat.read(cx);
            (chat.efforts_offered(), chat.effort_pill_bounds("high"))
        });
        if let Some(pill) = pill {
            self.click(pill, cx);
        }
        self.press("escape", cx);
        let (effort, saved) = cx.update(|cx| {
            (
                chat.read(cx).effort(),
                self.shell
                    .read(cx)
                    .settings
                    .model_for(&crate::shell::effort_key(QUICK_ID))
                    .map(str::to_owned),
            )
        });
        report.check(
            offered == ["low", "medium", "high", "xhigh", "max"]
                && effort.as_deref() == Some("high")
                && saved.as_deref() == Some("high"),
            format!("the picker's effort pills ({offered:?}) pick “high”, remembered per harness"),
        );
        cx.update(|cx| chat.update(cx, |chat, cx| chat.send(QUICK, cx)));
        let answered = self
            .wait_app(cx, Duration::from_secs(5), |cx| {
                let chat = chat.read(cx);
                !chat.thread().is_empty() && !chat.current_is_running()
            })
            .await;
        let still = cx.update(|cx| chat.read(cx).running_count());
        report.check(
            answered && still == 1,
            "the quick question is answered while the long task still runs",
        );
        let sent = QUICK_EFFORT
            .lock()
            .map(|effort| effort.clone())
            .unwrap_or_default();
        report.check(
            sent.as_deref() == Some("high"),
            format!("the turn went out at that effort ({sent:?})"),
        );
        self.settle(cx).await;
        let label = cx.update(|cx| sidebar.read(cx).tray_label(cx));
        report.check(
            label.as_deref() == Some("1 agent running"),
            format!("the tray button reads “1 agent running” ({label:?})"),
        );

        // The tray lists the long task with its live activity.
        let button = cx.update(|cx| sidebar.read(cx).tray_button_bounds());
        self.click(button, cx);
        self.settle(cx).await;
        let (open, items, item) = cx.update(|cx| {
            let sidebar = sidebar.read(cx);
            (
                sidebar.is_tray_open(),
                sidebar.tray_items(cx),
                sidebar.tray_item_bounds(&long),
            )
        });
        let lines: Vec<String> = items
            .iter()
            .map(|item| format!("{} — {}", item.title, item.line))
            .collect();
        report.check(
            open && items.len() == 1
                && items[0].id == long
                && items[0].running
                && items[0].line == "Searching: owl hearing",
            format!("a click opens the tray: {lines:?}"),
        );
        report.check(
            item.is_some_and(|item| {
                item.size.height > px(0.)
                    && item.bottom() <= button.top()
                    && item.left() >= button.left()
            }),
            "the tray grows upward from the button, inside the sidebar",
        );
        self.press("escape", cx);
        let open = cx.update(|cx| sidebar.read(cx).is_tray_open());
        report.check(!open, "Esc closes the tray");

        // It ends off screen: "1 done", and a toast.
        let done = self
            .wait_app(cx, Duration::from_secs(10), |cx| {
                chat.read(cx).running_count() == 0
            })
            .await;
        self.settle(cx).await;
        let (label, overview) = cx.update(|cx| {
            let sidebar = sidebar.read(cx);
            (sidebar.tray_label(cx), sidebar.overview(cx))
        });
        report.check(
            done && label.as_deref() == Some("1 done")
                && overview.finished.len() == 1
                && overview.finished[0].id == long
                && overview.finished[0].outcome == "Done",
            format!(
                "ended off screen: {label:?}, {:?}",
                overview
                    .finished
                    .iter()
                    .map(|finished| finished.outcome.clone())
                    .collect::<Vec<_>>()
            ),
        );
        let toast = cx.update(|cx| {
            self.shell
                .read(cx)
                .toasts
                .iter()
                .rev()
                .find(|(_, message)| message.contains("Long task"))
                .map(|(kind, message)| (*kind, message.clone()))
        });
        report.check(
            toast.as_ref().is_some_and(|(kind, message)| {
                *kind == ToastKind::Success
                    && message.starts_with("“Long task")
                    && message.ends_with("” finished")
            }),
            format!("a toast says so: {:?}", toast.map(|(_, message)| message)),
        );

        // Opening it from the tray shows it and clears the state.
        self.click(button, cx);
        self.settle(cx).await;
        let item = cx.update(|cx| sidebar.read(cx).tray_item_bounds(&long));
        if let Some(item) = item {
            self.click(item, cx);
        }
        self.settle(cx).await;
        let (shown, label, open, finished) = cx.update(|cx| {
            let sidebar = sidebar.read(cx);
            (
                chat.read(cx).thread().id.clone(),
                sidebar.tray_label(cx),
                sidebar.is_tray_open(),
                sidebar.overview(cx).finished.len(),
            )
        });
        report.check(
            item.is_some() && shown == long && label.is_none() && !open && finished == 0,
            "clicking it in the tray shows the thread and clears “finished”; the button hides",
        );
        if let Some((harness, model)) = before {
            cx.update(|cx| chat.update(cx, |chat, cx| chat.select(&harness, model, cx)));
        }
        self.dispatch(Box::new(actions::WriteMode), cx);
    }

    // -- 8d. Modes -----------------------------------------------------------------

    async fn modes_of_turns(&self, report: &mut Report, cx: &mut AsyncApp) {
        report.step("8d. Modes: Ask, Discover (long-running), New discovery");
        const GOAL: &str = "Find a better owl call detector.";
        let chat = cx.update(|cx| self.shell.read(cx).chat.clone());
        let sidebar = cx.update(|cx| self.shell.read(cx).sidebar.clone());
        let before = cx.update(|cx| chat.read(cx).selection());
        self.dispatch(Box::new(actions::AgentMode), cx);
        let agent = HarnessId::new(QUICK_ID);
        cx.update(|cx| chat.update(cx, |chat, cx| chat.select(&agent, Some("quick".into()), cx)));
        self.press("cmd-n", cx);
        self.settle(cx).await;

        // The mode pill opens a menu: Ask, then the harness's capabilities.
        let pill = cx.update(|cx| chat.read(cx).mode_pill_bounds());
        self.click(pill, cx);
        let (open, names) = cx.update(|cx| {
            let chat = chat.read(cx);
            (
                chat.open_popover(),
                chat.modes()
                    .into_iter()
                    .map(|mode| mode.name)
                    .collect::<Vec<_>>(),
            )
        });
        report.check(
            open == Some("mode") && names == ["Ask", "Discover", "Review"],
            format!("the mode pill lists {names:?}"),
        );
        self.press("down", cx);
        self.press("enter", cx);
        let (mode, placeholder, open) = cx.update(|cx| {
            let chat = chat.read(cx);
            (chat.mode(), chat.placeholder(), chat.open_popover())
        });
        report.check(
            open.is_none()
                && mode.as_deref() == Some(oyl_chat::DISCOVER)
                && placeholder.as_ref() == DISCOVER_PLACEHOLDER,
            format!("↓ Enter picks Discover; the placeholder follows (“{placeholder}”)"),
        );

        // A long-running mode goes straight to the background.
        let running_before = cx.update(|cx| chat.read(cx).running_count());
        let discovery = cx.update(|cx| chat.read(cx).thread().id.clone());
        cx.update(|cx| chat.update(cx, |chat, cx| chat.send(GOAL, cx)));
        self.settle(cx).await;
        let (fresh, mode, running) = cx.update(|cx| {
            let chat = chat.read(cx);
            (chat.thread().is_empty(), chat.mode(), chat.running_count())
        });
        let sent = QUICK_CAPABILITY
            .lock()
            .map(|capability| capability.clone())
            .unwrap_or_default();
        report.check(
            sent.as_deref() == Some(oyl_chat::DISCOVER)
                && fresh
                && mode.is_none()
                && running == running_before + 1,
            format!("a Discover turn goes out as “discover” ({sent:?}) and runs in the background; a fresh Ask thread is on screen"),
        );
        let toast = cx.update(|cx| {
            self.shell
                .read(cx)
                .toasts
                .iter()
                .rev()
                .any(|(_, message)| message == &format!("“{GOAL}” runs in the background"))
        });
        let tray = cx.update(|cx| sidebar.read(cx).tray_label(cx));
        report.check(
            toast && tray.as_deref() == Some("1 agent running"),
            format!("a toast says it runs in the background; the tray counts it ({tray:?})"),
        );

        // "New discovery" opens a fresh thread in Discover mode with the
        // starter lines.
        let row = cx.update(|cx| sidebar.read(cx).discovery_row_bounds());
        self.click(row, cx);
        self.settle(cx).await;
        let (fresh, mode, text) = cx.update(|cx| {
            let chat = chat.read(cx);
            (
                chat.thread().is_empty(),
                chat.mode(),
                chat.composer_text(cx),
            )
        });
        report.check(
            fresh
                && mode.as_deref() == Some(oyl_chat::DISCOVER)
                && text == oyl_chat::DISCOVERY_STARTER,
            format!(
                "“New discovery” opens a Discover thread with the starter ({:?})",
                text.lines().next()
            ),
        );
        cx.update(|cx| chat.update(cx, |chat, cx| chat.set_mode(None, cx)));

        // Its progress: the tray row, the card, the board.
        let id = discovery.clone();
        let progressed = self
            .wait_app(cx, Duration::from_secs(5), |cx| {
                chat.read(cx)
                    .task(&id)
                    .and_then(|task| task.progress.as_ref())
                    .is_some_and(|progress| progress.step == Some(2))
            })
            .await;
        let button = cx.update(|cx| sidebar.read(cx).tray_button_bounds());
        self.click(button, cx);
        self.settle(cx).await;
        let (row, ring) = cx.update(|cx| {
            (
                sidebar.read(cx).tray_item_bounds(&discovery),
                chat.read(cx)
                    .task(&discovery)
                    .and_then(|task| task.fraction()),
            )
        });
        report.check(
            progressed
                && row.is_some()
                && ring.is_some_and(|fraction| (fraction - 0.4).abs() < 1e-3),
            format!("the tray lists the discovery as a task row with a ring at {ring:?}"),
        );
        self.press("escape", cx);
        let id = discovery.clone();
        self.with_shell(cx, move |shell, window, cx| {
            shell
                .chat
                .update(cx, |chat, cx| chat.show_thread(&id, window, cx))
        });
        self.settle(cx).await;
        let (card, phase, view) = cx.update(|cx| {
            let chat = chat.read(cx);
            (
                chat.shows_task_card(),
                chat.task(&discovery)
                    .and_then(|task| task.progress.as_ref())
                    .map(|progress| (progress.phase.clone(), progress.step, progress.total)),
                chat.thread_view(),
            )
        });
        report.check(
            card && phase == Some((DISCOVERY_PHASES[1].to_string(), Some(2), Some(5))),
            format!("its thread pins a task card: {phase:?}"),
        );
        let id = discovery.clone();
        let loaded = self
            .wait_app(cx, Duration::from_secs(5), |cx| {
                chat.read(cx).board(&id).is_some_and(|board| {
                    let board = board.read(cx);
                    board.is_loaded()
                        && board
                            .state()
                            .frontier
                            .iter()
                            .any(|row| row.id == FRONTIER_BEST)
                })
            })
            .await;
        report.check(
            view == Some(oyl_chat::ThreadView::Board) && loaded,
            "under it the Board (the default while it runs) loads the frontier from its state folder",
        );
        // Stop, as the card's button does.
        let id = discovery.clone();
        cx.update(|cx| chat.update(cx, |chat, cx| chat.stop_thread(&id, cx)));
        let id = discovery.clone();
        let stopped = self
            .wait_app(cx, Duration::from_secs(10), |cx| {
                let chat = chat.read(cx);
                chat.running_count() == 0
                    && chat
                        .task(&id)
                        .is_some_and(|task| task.state == oyl_tasks::TaskState::Stopped)
            })
            .await;
        report.check(
            stopped,
            "Stop on the card stops the run; its task reads Stopped",
        );
        if let Some((harness, model)) = before {
            cx.update(|cx| chat.update(cx, |chat, cx| chat.select(&harness, model, cx)));
        }
        self.dispatch(Box::new(actions::WriteMode), cx);
    }

    // -- 8e. AI in the editor -------------------------------------------------------

    async fn editor_ai(&self, report: &mut Report, article: &Path, cx: &mut AsyncApp) {
        report.step("8e. AI in the editor: polish, fact check, no global ⌘B/⌘I");
        let main = article.join("main.typ");
        // Nothing outside the editor binds its keys.
        let global: Vec<String> = cx.update(|cx| {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            keymap
                .bindings()
                .filter(|binding| {
                    let [keystroke] = binding.keystrokes() else {
                        return false;
                    };
                    let modifiers = keystroke.modifiers();
                    let ours = modifiers.platform
                        && !modifiers.control
                        && !modifiers.alt
                        && matches!(
                            (keystroke.key(), modifiers.shift),
                            ("b", false) | ("b", true) | ("i", false)
                        );
                    ours && binding.predicate().is_none_or(|predicate| {
                        !predicate.to_string().contains(oyl_editor::KEY_CONTEXT)
                    })
                })
                .map(|binding| format!("{:?}", binding.predicate().map(|p| p.to_string())))
                .collect()
        });
        report.check(
            global.is_empty(),
            format!("⌘B, ⌘I and ⇧⌘B are bound only in the editor ({global:?})"),
        );

        self.with_shell(cx, |shell, _, cx| {
            shell.set_editor_assist(
                Some(Arc::new(FakeRewriter)),
                Some(Arc::new(FakeChecker)),
                cx,
            )
        });
        self.dispatch(Box::new(actions::WriteMode), cx);
        self.settle(cx).await;
        let text = self.editor_text(&main, cx);
        let line = text
            .lines()
            .position(|line| line.starts_with(PROSE_LINE))
            .unwrap_or(0);
        // In the middle of a sentence of prose.
        let location = SourceLocation::new(main.clone(), line as u32 + 1, 10);
        self.with_shell(cx, move |shell, window, cx| {
            shell
                .editor
                .update(cx, |editor, cx| editor.goto(&location, window, cx));
            shell.editor.focus_handle(cx).focus(window, cx);
        });
        self.frame(cx);
        // From the palette's command (the same action as the menu item).
        self.dispatch(Box::new(actions::PolishSelection), cx);
        let changed = self
            .wait_app(cx, Duration::from_secs(5), |cx| {
                self.shell
                    .read(cx)
                    .editor
                    .read(cx)
                    .text(&main, cx)
                    .is_some_and(|text| text.contains(POLISHED_MARK))
            })
            .await;
        report.check(
            changed,
            "“Polish selection” rewrites the sentence at the cursor through the rewriter",
        );
        self.dispatch(Box::new(actions::FactCheckSelection), cx);
        let toast = self
            .wait_app(cx, Duration::from_secs(5), |cx| {
                self.shell
                    .read(cx)
                    .toasts
                    .iter()
                    .any(|(_, message)| message == "Fact check: 1 problem · 7 claims")
            })
            .await;
        report.check(
            toast,
            "“Fact-check selection” runs the checker; a toast says “Fact check: 1 problem · 7 claims”",
        );
        let bib = cx.update(|cx| self.shell.read(cx).bib_text.borrow().clone());
        let asked = FACT_BIB.lock().map(|bib| bib.clone()).unwrap_or_default();
        report.check(
            asked == bib,
            format!("the check got the paper's refs.bib ({} bytes)", asked.len()),
        );
        self.with_shell(cx, |shell, _, cx| shell.set_editor_assist(None, None, cx));
    }

    // -- 8f. Branches ----------------------------------------------------------------

    /// Starts a turn of the self-test agent in a fresh thread of the Agent
    /// tab and waits for its branch; runs `during` once it exists, then
    /// waits for the turn and its branch to settle. The thread's id.
    async fn branch_turn(
        &self,
        prompt: &str,
        during: impl FnOnce(&Path, &Path),
        article: &Path,
        cx: &mut AsyncApp,
    ) -> Option<String> {
        let chat = cx.update(|cx| self.shell.read(cx).chat.clone());
        self.press("cmd-n", cx);
        let agent = HarnessId::new("self-test");
        cx.update(|cx| {
            chat.update(cx, |chat, cx| {
                chat.select(&agent, Some("script".into()), cx)
            })
        });
        let prompt = prompt.to_string();
        cx.update(|cx| chat.update(cx, |chat, cx| chat.send(prompt, cx)));
        let thread = cx.update(|cx| chat.read(cx).thread().id.clone());
        let id = thread.clone();
        let made = self
            .wait_app(cx, Duration::from_secs(10), |cx| {
                self.shell
                    .read(cx)
                    .paper
                    .as_ref()
                    .is_some_and(|paper| paper.branches.contains_key(&id))
            })
            .await;
        if !made {
            return None;
        }
        let id = thread.clone();
        let work = cx.update(|cx| {
            self.shell
                .read(cx)
                .paper
                .as_ref()
                .and_then(|paper| paper.branches.get(&id))
                .map(|branch| branch.branch.path().to_path_buf())
        })?;
        // The Agent tab's preview shows this thread's branch meanwhile.
        self.frame(cx);
        let shown = cx.update(|cx| self.shell.read(cx).preview_branch(cx).map(|(path, _)| path));
        self.previewed.set(shown.as_deref() == Some(work.as_path()));
        during(article, &work);
        let id = thread.clone();
        self.wait_app(cx, Duration::from_secs(20), |cx| {
            let shell = self.shell.read(cx);
            let running = shell.chat.read(cx).is_running();
            let settling = shell.paper.as_ref().is_some_and(|paper| {
                paper.branches.get(&id).is_some_and(|branch| {
                    matches!(
                        branch.stage,
                        crate::shell::Stage::Working | crate::shell::Stage::Finishing
                    )
                })
            });
            !running && !settling
        })
        .await;
        self.settle(cx).await;
        Some(thread)
    }

    // -- 8g. Oyl Cloud --------------------------------------------------------------

    /// The header glyph's status, if the paper syncs.
    fn cloud_status(&self, cx: &mut AsyncApp) -> Option<oyl_sync::Status> {
        cx.update(|cx| {
            self.shell
                .read(cx)
                .paper
                .as_ref()
                .and_then(|paper| paper.cloud.as_ref())
                .and_then(|cloud| cloud.status.clone())
        })
    }

    fn toasted(&self, text: &str, cx: &mut AsyncApp) -> bool {
        cx.update(|cx| {
            self.shell
                .read(cx)
                .toasts
                .iter()
                .any(|(_, message)| message.contains(text))
        })
    }

    async fn cloud_sync(&self, report: &mut Report, article: &Path, cx: &mut AsyncApp) {
        use oyl_sync::Status;
        report.step(
            "8g. Oyl Cloud: sign in, sync, changes from elsewhere, a review, open from the cloud",
        );
        let long = Duration::from_secs(30);
        let labels = |this: &Self, cx: &mut AsyncApp| -> Vec<(String, bool)> {
            cx.update(|cx| {
                this.shell
                    .read(cx)
                    .palette_entries(cx)
                    .into_iter()
                    .map(|entry| (entry.label, entry.checked))
                    .collect()
            })
        };
        let has = |entries: &[(String, bool)], label: &str| entries.iter().any(|(l, _)| l == label);
        let entries = labels(self, cx);
        report.check(
            has(&entries, "Sign in to Oyl Cloud…")
                && !has(&entries, "Sign out of Oyl Cloud")
                && has(&entries, "Sync to Oyl Cloud"),
            "signed out, the palette offers “Sign in to Oyl Cloud…”",
        );
        report.check(
            self.cloud_status(cx).is_none() && !oyl_sync::SyncState::is_enabled(article),
            "sync is off: nothing leaves the Mac, and the header shows no cloud glyph",
        );

        // Signing in: the card shows a code, the browser approves it.
        self.dispatch(Box::new(actions::SignInToCloud), cx);
        let card = cx.update(|cx| self.shell.read(cx).cloud.card.clone());
        let mut code = None;
        let shown = self
            .wait_app(cx, Duration::from_secs(10), |cx| {
                match card.read(cx).stage() {
                    crate::cloud_view::SignInStage::Waiting { code: shown, .. } => {
                        code = Some(shown.clone());
                        true
                    }
                    _ => false,
                }
            })
            .await;
        report.check(
            shown && self.showing(crate::shell::cloud_sign_in_id(), cx),
            format!(
                "the sign-in card shows the code {}",
                code.clone().unwrap_or_default()
            ),
        );
        let account = oyl_sync::Account {
            id: "user:selftest".into(),
            name: "Self Test".into(),
            email: "self.test@example.test".into(),
        };
        let approved = code
            .as_deref()
            .is_some_and(|code| self.cloud.approve(code, &account));
        let shell = self.shell.clone();
        let signed_in = self
            .wait_app(cx, long, |cx| shell.read(cx).cloud_account().is_some())
            .await;
        report.check(
            approved
                && signed_in
                && self.toasted("Signed in to Oyl Cloud as Self Test", cx)
                && !self.showing(crate::shell::cloud_sign_in_id(), cx),
            "approved in the browser: “Signed in to Oyl Cloud as Self Test”, the card closes",
        );
        use oyl_sync::TokenStore as _;
        report.check(
            self.tokens
                .load(&oyl_sync::token_key(&self.cloud.url()))
                .is_some_and(|token| token.starts_with("oyl_sync_")),
            "the key is kept by the key store (the Keychain outside the self-test)",
        );

        // Sync on for the article: the cloud paper is made and filled.
        self.dispatch(Box::new(actions::ToggleCloudSync), cx);
        let mut synced = false;
        let start = Instant::now();
        while start.elapsed() < long {
            self.frame(cx);
            if self.cloud_status(cx) == Some(Status::Synced) {
                synced = true;
                break;
            }
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
        }
        let paper = oyl_sync::SyncState::load(article)
            .paper_id
            .unwrap_or_default();
        let head = self.cloud.head_files(&paper);
        report.check(
            synced && head.contains_key("main.typ") && head.contains_key("refs.bib"),
            format!(
                "“Sync to Oyl Cloud” made the paper and sent its {} files",
                head.len()
            ),
        );
        let entries = labels(self, cx);
        report.check(
            entries.contains(&("Sync to Oyl Cloud".to_string(), true)) && has(&entries, "Sync now"),
            "the palette shows “Sync to Oyl Cloud” checked, and “Sync now”",
        );
        let glyph = cx.update(|cx| self.shell.read(cx).cloud_glyph(cx).map(|g| g.tooltip));
        report.check(
            glyph.as_deref() == Some("Synced with Oyl Cloud"),
            format!(
                "the header's cloud glyph says “{}”",
                glyph.unwrap_or_default()
            ),
        );

        // Typing goes up a moment after the save.
        self.dispatch(Box::new(actions::WriteMode), cx);
        let main = article.join("main.typ");
        let lines = self.editor_text(&main, cx).split('\n').count() as u32;
        let location = SourceLocation::new(main.clone(), lines, 1);
        self.with_shell(cx, move |shell, window, cx| {
            shell
                .editor
                .update(cx, |editor, cx| editor.goto(&location, window, cx))
        });
        const SYNCED: &str = "Typed for the cloud";
        self.type_text(SYNCED, cx);
        let mut pushed = false;
        let start = Instant::now();
        while start.elapsed() < long {
            self.frame(cx);
            let head = self.cloud.head_files(&paper);
            if head
                .get("main.typ")
                .is_some_and(|bytes| String::from_utf8_lossy(bytes).contains(SYNCED))
            {
                pushed = true;
                break;
            }
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
        }
        report.check(
            pushed,
            "typing was saved and pushed: the cloud's main.typ has it",
        );

        // A change made on the web comes in by itself.
        self.cloud.web_commit(
            &paper,
            &[("from-web.md", Some(b"Written on the web.\n".as_slice()))],
        );
        self.dispatch(Box::new(actions::SyncNow), cx);
        let shell = self.shell.clone();
        let toasted = self
            .wait_app(cx, long, |cx| {
                shell
                    .read(cx)
                    .toasts
                    .iter()
                    .any(|(_, message)| message == "From the web · 1 file updated")
            })
            .await;
        let arrived = toasted && article.join("from-web.md").exists();
        let history = cx.update(|cx| {
            self.shell
                .read(cx)
                .paper
                .as_ref()
                .and_then(|paper| paper.history.clone())
        });
        let by_cloud = history
            .and_then(|history| history.versions().ok())
            .is_some_and(|versions| {
                versions.iter().any(|v| {
                    v.author == Author::Cloud && v.label.as_deref() == Some("From the web")
                })
            });
        report.check(
            arrived && by_cloud,
            "a file added on the web arrived (“From the web · 1 file updated”, a history version by Cloud)",
        );

        // Overlapping edits wait for a review in the Agent tab.
        let notes = article.join("cloud-notes.md");
        let _ = fs::write(&notes, "Notes, first draft.\n");
        let up = self
            .wait(cx, long, |_| {
                self.cloud.head_files(&paper).contains_key("cloud-notes.md")
            })
            .await;
        report.check(up, "a file written by another tool went up too");
        self.cloud.web_commit(
            &paper,
            &[(
                "cloud-notes.md",
                Some(b"Notes, as the web has them.\n".as_slice()),
            )],
        );
        let _ = fs::write(&notes, "Notes, as this Mac has them.\n");
        self.dispatch(Box::new(actions::SyncNow), cx);
        let chat = cx.update(|cx| self.shell.read(cx).chat.clone());
        let reviewed = self
            .wait_app(cx, long, |cx| {
                chat.read(cx)
                    .cloud_review()
                    .is_some_and(|review| review.conflicts.len() == 1)
            })
            .await;
        report.check(
            reviewed
                && self.toasted("Changes from Oyl Cloud need review (From the web)", cx)
                && self.cloud_status(cx) == Some(Status::NeedsReview),
            "the same lines changed on both sides: “Changes from Oyl Cloud” waits in the Agent tab, the glyph says so",
        );
        report.check(
            fs::read_to_string(&notes).is_ok_and(|text| text.contains("this Mac")),
            "nothing was written before the review",
        );
        self.dispatch(Box::new(actions::AgentMode), cx);
        self.settle(cx).await;
        let id = cx.update(|cx| {
            chat.read(cx)
                .cloud_review()
                .and_then(|review| review.conflicts.first().map(|c| c.id.clone()))
        });
        if let Some(id) = id {
            let chat = chat.clone();
            let _ = self.window.update(cx, move |_, _, cx| {
                chat.update(cx, |chat, cx| {
                    chat.set_review_choice(
                        oyl_chat::CLOUD_REVIEW,
                        &id,
                        oyl_chat::ReviewChoice::Yours,
                        cx,
                    );
                    chat.apply_review(oyl_chat::CLOUD_REVIEW, cx);
                })
            });
        }
        let settled = self
            .wait_app(cx, long, |cx| chat.read(cx).cloud_review().is_none())
            .await;
        let mut kept = false;
        let start = Instant::now();
        while start.elapsed() < long {
            self.frame(cx);
            if self
                .cloud
                .head_files(&paper)
                .get("cloud-notes.md")
                .is_some_and(|bytes| bytes == b"Notes, as this Mac has them.\n")
                && self.cloud_status(cx) == Some(Status::Synced)
            {
                kept = true;
                break;
            }
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
        }
        report.check(
            settled && kept,
            "Apply with “Yours”: the review goes away and this Mac's notes become the cloud's",
        );

        // Open from Oyl Cloud: another paper of the account, downloaded.
        let token = self
            .tokens
            .load(&oyl_sync::token_key(&self.cloud.url()))
            .unwrap_or_default();
        let client = oyl_sync::Client::new(&self.cloud.url(), Some(token));
        let other = format!("paper_sync:k{}", "5e1f".repeat(8));
        let made = client
            .create_paper(&other, "Made on another Mac", "typst", "main.typ")
            .and_then(|_| {
                let text = b"= Made on another Mac\n\nHello from the cloud.\n";
                let sha = oyl_history::hash_bytes(text);
                client.put_blob(&sha, text)?;
                let files = std::collections::BTreeMap::from([(
                    "main.typ".to_string(),
                    oyl_sync::FileEntry {
                        sha256: sha,
                        size: text.len() as u64,
                    },
                )]);
                client.commit(&other, None, &files, None)
            });
        report.check(made.is_ok(), "the account has a second paper in the cloud");
        self.dispatch(Box::new(actions::OpenFromCloud), cx);
        let shell = self.shell.clone();
        let listed = self
            .wait_app(cx, long, |cx| {
                shell
                    .read(cx)
                    .library_cloud_data()
                    .papers
                    .iter()
                    .any(|p| p.name == "Made on another Mac")
            })
            .await;
        let only_other = cx.update(|cx| {
            self.shell
                .read(cx)
                .library_cloud_data()
                .papers
                .iter()
                .all(|p| p.id != paper)
        });
        report.check(
            listed && only_other && self.showing("library", cx),
            "the library's “Oyl Cloud” section lists it (and not the paper already here)",
        );
        let parent = self.papers.join("from-cloud");
        let _ = fs::create_dir_all(&parent);
        let (id, into) = (other.clone(), parent.clone());
        self.with_shell(cx, move |shell, window, cx| {
            shell.download_cloud_paper(id, into, window, cx)
        });
        // An empty folder picked (as made with the picker's "New Folder")
        // takes the paper itself.
        let downloaded = parent.clone();
        let opened = self
            .wait(cx, Duration::from_secs(60), |s| {
                s.root.as_ref().is_some_and(|root| {
                    fs::canonicalize(root).ok() == fs::canonicalize(&downloaded).ok()
                }) && s.compiles >= 1
            })
            .await;
        report.check(
            opened
                && fs::read_to_string(downloaded.join("main.typ"))
                    .is_ok_and(|t| t.contains("Hello from the cloud"))
                && self.toasted("Downloaded “Made on another Mac” from Oyl Cloud", cx),
            "“Open” downloaded it into the empty folder picked and opened it",
        );
        let synced = self
            .wait(cx, long, |_| self.cloud.revision_count(&other) >= 2)
            .await;
        report.check(
            synced,
            "it syncs from the start (setting it up as a paper went up as revision 2)",
        );

        // Back to the article; sync off; sign out.
        let root = article.to_path_buf();
        self.with_shell(cx, move |shell, window, cx| {
            shell.open_paper(root, window, cx)
        });
        let back = self
            .wait(cx, Duration::from_secs(60), |s| {
                s.root.as_deref() == Some(article) && s.compiles >= 1
            })
            .await;
        report.check(back, "the article is open again (and syncs again)");
        let resumed = {
            let start = Instant::now();
            let mut ok = false;
            while start.elapsed() < long {
                self.frame(cx);
                if self.cloud_status(cx) == Some(Status::Synced) {
                    ok = true;
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
            }
            ok
        };
        report.check(resumed, "reopened, the paper pulls and is synced");
        self.dispatch(Box::new(actions::ToggleCloudSync), cx);
        self.frame(cx);
        report.check(
            self.cloud_status(cx).is_none() && !oyl_sync::SyncState::is_enabled(article),
            "“Sync to Oyl Cloud” off: the glyph goes away",
        );
        self.dispatch(Box::new(actions::SignOutOfCloud), cx);
        self.frame(cx);
        let entries = labels(self, cx);
        report.check(
            cx.update(|cx| self.shell.read(cx).cloud_account().is_none())
                && self
                    .tokens
                    .load(&oyl_sync::token_key(&self.cloud.url()))
                    .is_none()
                && has(&entries, "Sign in to Oyl Cloud…"),
            "signed out: the key is forgotten, the palette offers to sign in again",
        );
        self.dispatch(Box::new(actions::WriteMode), cx);
        self.settle(cx).await;
    }

    async fn branches(
        &self,
        report: &mut Report,
        article: &Path,
        cx: &mut AsyncApp,
    ) -> Option<String> {
        report.step("8f. Agent turns on branches: apply, review, re-review, discard");
        let main = article.join("main.typ");
        let chat = cx.update(|cx| self.shell.read(cx).chat.clone());
        let sentences = |text: &str| text.matches(AGENT_SENTENCE).count();
        self.dispatch(Box::new(actions::AgentMode), cx);
        self.settle(cx).await;

        // 1. The paper changes elsewhere meanwhile: applied by itself.
        let before = fs::read_to_string(&main).unwrap_or_default();
        let extra = article.join("extra.typ");
        let mut seen: Option<String> = None;
        let thread = {
            let (main, extra, seen) = (main.clone(), extra.clone(), &mut seen);
            self.branch_turn(
                "Branch turn one.",
                move |_, work| {
                    let _ = fs::write(&extra, "= Your own notes\n");
                    // The agent edits its copy; the paper stays as it was.
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while Instant::now() < deadline {
                        let edited = fs::read_to_string(work.join("main.typ"))
                            .is_ok_and(|text| text.contains(AGENT_SENTENCE));
                        if edited {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    *seen = fs::read_to_string(&main).ok();
                },
                article,
                cx,
            )
            .await
        };
        let untouched = seen.as_deref().map(sentences) == Some(sentences(&before));
        report.check(
            thread.is_some() && untouched,
            "the agent works on its own copy: the paper is untouched while it runs",
        );
        report.check(
            self.previewed.get(),
            "meanwhile the Agent tab's preview compiles and shows that copy (“Showing …'s branch”)",
        );
        let after = fs::read_to_string(&main).unwrap_or_default();
        let applied_toast = cx.update(|cx| {
            self.shell
                .read(cx)
                .toasts
                .iter()
                .any(|(_, message)| message == "Applied · 1 file")
        });
        let view = thread
            .as_ref()
            .and_then(|thread| cx.update(|cx| chat.read(cx).branch_view(thread).cloned()));
        report.check(
            sentences(&after) == sentences(&before) + 1
                && fs::read_to_string(&extra).is_ok_and(|text| text.contains("Your own notes"))
                && applied_toast
                && matches!(view, Some(oyl_chat::BranchView::Applied { files: 1, .. })),
            "a change of another file meanwhile: applied by itself (“Applied · 1 file”, Undo in the thread)",
        );

        // 2. The paper changes the same lines: a review, settled three ways.
        let mut pending = None;
        for (choice, user_line) in [
            (oyl_chat::ReviewChoice::Yours, "Your line, kept."),
            (oyl_chat::ReviewChoice::Agents, "Your line, replaced."),
            (
                oyl_chat::ReviewChoice::Both,
                "Your line, next to the agent's.",
            ),
        ] {
            let before = fs::read_to_string(&main).unwrap_or_default();
            let line = user_line.to_string();
            let thread = self
                .branch_turn(
                    "Branch turn with a conflict.",
                    move |article, _| append_line(&article.join("main.typ"), &line),
                    article,
                    cx,
                )
                .await;
            let Some(thread) = thread else {
                report.check(false, format!("a turn for “{user_line}” got its branch"));
                continue;
            };
            let review = cx.update(|cx| chat.read(cx).branch_view(&thread).cloned());
            let conflict = match &review {
                Some(oyl_chat::BranchView::Review(review)) => review.conflicts.first().cloned(),
                _ => None,
            };
            let Some(conflict) = conflict else {
                report.check(false, format!("a review for “{user_line}”: {review:?}"));
                continue;
            };
            if choice == oyl_chat::ReviewChoice::Yours {
                report.check(
                    conflict.lines
                        && conflict.main.contains(user_line)
                        && conflict.agent.contains(AGENT_SENTENCE),
                    format!(
                        "the same lines changed: a review with a Lines conflict at {}",
                        conflict.place
                    ),
                );
            }
            let id = thread.clone();
            let conflict_id = conflict.id.clone();
            cx.update(|cx| {
                chat.update(cx, |chat, cx| {
                    chat.set_review_choice(&id, &conflict_id, choice, cx);
                    chat.apply_review(&id, cx);
                })
            });
            let id = thread.clone();
            self.wait_app(cx, Duration::from_secs(10), |cx| {
                !matches!(
                    chat.read(cx).branch_view(&id),
                    Some(oyl_chat::BranchView::Review(_))
                )
            })
            .await;
            self.settle(cx).await;
            let text = fs::read_to_string(&main).unwrap_or_default();
            let (yours, agents) = (
                text.contains(user_line),
                sentences(&text) > sentences(&before),
            );
            let expected = match choice {
                oyl_chat::ReviewChoice::Yours => yours && !agents,
                oyl_chat::ReviewChoice::Agents => !yours && agents,
                oyl_chat::ReviewChoice::Both => {
                    yours && agents && text.find(user_line) < text.rfind(AGENT_SENTENCE)
                }
            };
            report.check(
                expected,
                format!("“{choice:?}” settles the conflict as chosen (yours: {yours}, agent's: {agents})"),
            );
        }

        // 3. The paper changes again before Apply: the review is redone.
        let before = fs::read_to_string(&main).unwrap_or_default();
        let thread = self
            .branch_turn(
                "Branch turn to discard.",
                |article, _| append_line(&article.join("main.typ"), "A first try."),
                article,
                cx,
            )
            .await;
        if let Some(thread) = thread.clone() {
            let text = fs::read_to_string(&main).unwrap_or_default();
            let _ =
                oyl_core::write_file_atomic(&main, text.replace("A first try.", "A second try."));
            self.settle(cx).await;
            let id = thread.clone();
            cx.update(|cx| chat.update(cx, |chat, cx| chat.apply_review(&id, cx)));
            let id = thread.clone();
            let updated = self
                .wait_app(cx, Duration::from_secs(10), |cx| {
                    match chat.read(cx).branch_view(&id) {
                        Some(oyl_chat::BranchView::Review(review)) => {
                            review.note.as_deref() == Some(crate::shell::REVIEW_UPDATED)
                        }
                        _ => false,
                    }
                })
                .await;
            report.check(
                updated,
                "the paper changed again before Apply: “review updated”, nothing written",
            );
            let id = thread.clone();
            cx.update(|cx| chat.update(cx, |chat, cx| chat.discard_branch(&id, cx)));
            let id = thread.clone();
            let gone = self
                .wait_app(cx, Duration::from_secs(10), |cx| {
                    chat.read(cx).branch_view(&id).is_none()
                })
                .await;
            let text = fs::read_to_string(&main).unwrap_or_default();
            report.check(
                gone && text.contains("A second try.") && sentences(&text) == sentences(&before),
                "Discard drops the agent's work; the paper keeps yours",
            );
        } else {
            report.check(false, "a turn to discard got its branch");
        }

        // 4. A review left pending for the restart.
        if let Some(thread) = self
            .branch_turn(
                "Branch turn left in review.",
                |article, _| append_line(&article.join("main.typ"), "Left for later."),
                article,
                cx,
            )
            .await
        {
            let review = cx.update(|cx| {
                matches!(
                    chat.read(cx).branch_view(&thread),
                    Some(oyl_chat::BranchView::Review(_))
                )
            });
            report.check(review, "a review can wait in its thread");
            pending = review.then_some(thread);
        }
        self.dispatch(Box::new(actions::WriteMode), cx);
        pending
    }

    // -- 9. Reopening -------------------------------------------------------------

    async fn reopen(
        &self,
        report: &mut Report,
        article: &Path,
        pending: Option<String>,
        cx: &mut AsyncApp,
    ) {
        report.step("9. Reopening from the same data folder restores the state");
        self.dispatch(Box::new(actions::AgentMode), cx);
        self.with_shell(cx, |shell, window, cx| {
            shell.set_split_ratio(0.62, cx);
            shell.save_everything(window, cx)
        });
        let _ = self
            .window
            .update(cx, |_, window, _| window.remove_window());
        self.wait(cx, Duration::from_millis(400), |_| false).await;

        let opened = cx.update(|cx| {
            open_shell(
                &self.paths,
                Agents::Given(Vec::new()),
                cloud_options(&self.cloud, &self.tokens),
                cx,
            )
        });
        let Ok((window, shell)) = opened else {
            report.check(false, "a second window opens");
            return;
        };
        let second = SelfTest {
            paths: self.paths.clone(),
            papers: self.papers.clone(),
            window,
            shell,
            previewed: std::cell::Cell::new(false),
            cloud: self.cloud.clone(),
            tokens: self.tokens.clone(),
        };
        let back = second
            .wait(cx, Duration::from_secs(60), |s| {
                s.root.is_some() && s.compiles >= 1
            })
            .await;
        let s = second.snapshot(cx);
        report.check(
            back && s.root.as_deref() == Some(article),
            "the last paper opens again",
        );
        report.check(s.mode == Some(LayoutMode::Agent), "on the Agent tab");
        if let Some(thread) = pending {
            let chat = cx.update(|cx| second.shell.read(cx).chat.clone());
            let restored = second
                .wait_app(cx, Duration::from_secs(10), |cx| {
                    matches!(
                        chat.read(cx).branch_view(&thread),
                        Some(oyl_chat::BranchView::Review(_))
                    )
                })
                .await;
            report.check(restored, "the review left pending is back in its thread");
        }
        let split = cx.update(|cx| second.shell.read(cx).settings.split_ratio);
        report.check(
            (split - 0.62).abs() < 1e-3,
            format!("with the split at {split:.2}"),
        );
        report.check(
            WindowFrame::load(&self.paths).is_some(),
            "the window frame was remembered",
        );
        let _ = second
            .window
            .update(cx, |_, window, _| window.remove_window());
    }
}

fn fingerprints(document: &dyn RenderedDocument) -> Vec<u64> {
    (0..document.page_count())
        .map(|page| document.page_fingerprint(page))
        .collect()
}

fn describe(errors: &[String]) -> String {
    if errors.is_empty() {
        String::new()
    } else {
        format!(" (got: {})", errors.join("; "))
    }
}

fn count_files(dir: &Path, extension: &str) -> usize {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
                })
                .count()
        })
        .unwrap_or(0)
}

/// A small PNG: an amber gradient.
fn test_png() -> anyhow::Result<Vec<u8>> {
    let image = image::RgbaImage::from_fn(96, 64, |x, y| {
        image::Rgba([200 + (x % 56) as u8, 150 + (y % 100) as u8, 60, 255])
    });
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    Ok(bytes)
}

/// A scripted agent that, like a real one, writes the paper's files while
/// its turn runs: when its script reports `main.typ` changed, it appends a
/// sentence to the file on disk first.
struct EditingHarness {
    script: ScriptedHarness,
}

impl EditingHarness {
    fn new() -> Self {
        let pace = Duration::from_millis(120);
        let step = |delay: Duration, event: AgentEvent| ScriptStep { delay, event };
        let mut info = ScriptedHarness::from_events("self-test", Vec::new(), pace).info();
        info.name = "Self-test agent".into();
        info.tagline = "A scripted agent that edits main.typ".into();
        info.models = vec![
            oyl_model::agent::ModelChoice {
                id: "script".into(),
                label: "Script".into(),
            },
            oyl_model::agent::ModelChoice {
                id: "script-careful".into(),
                label: "Script, careful".into(),
            },
        ];
        info.default_model = Some("script".into());
        let steps = vec![
            step(
                pace,
                AgentEvent::Status {
                    message: "Reading the paper…".into(),
                },
            ),
            step(
                pace,
                AgentEvent::ToolStarted {
                    id: "edit-1".into(),
                    name: "Edit".into(),
                    title: "Edit main.typ".into(),
                    detail: None,
                },
            ),
            step(
                pace,
                AgentEvent::FileChanged {
                    path: PathBuf::from("main.typ"),
                },
            ),
            step(
                pace,
                AgentEvent::ToolFinished {
                    id: "edit-1".into(),
                    ok: true,
                    output: Some("Edited main.typ".into()),
                },
            ),
            step(
                pace,
                AgentEvent::TextDelta {
                    text: "I added a sentence about owls.".into(),
                },
            ),
            // Long enough for the watcher to bring the edit in mid-turn.
            step(
                Duration::from_millis(1200),
                AgentEvent::Completed {
                    success: true,
                    summary: "Added a sentence about owls.".into(),
                    session: None,
                },
            ),
        ];
        Self {
            script: ScriptedHarness::new(info, steps),
        }
    }
}

impl Harness for EditingHarness {
    fn info(&self) -> HarnessInfo {
        self.script.info()
    }

    fn start(&self, request: TurnRequest) -> anyhow::Result<TurnHandle> {
        let inner = self.script.start(request)?;
        let (tx, rx) = async_channel::unbounded();
        let stopped = Arc::new(AtomicBool::new(false));
        let cancel_inner = inner.canceller();
        let flag = stopped.clone();
        std::thread::Builder::new()
            .name("oyl-self-test-agent".into())
            .spawn(move || {
                while let Ok(event) = inner.events.recv_blocking() {
                    if let AgentEvent::FileChanged { path } = &event
                        && !flag.load(Ordering::SeqCst)
                        && let Ok(text) = fs::read_to_string(path)
                    {
                        let edited = format!("{}\n{AGENT_SENTENCE}\n", text.trim_end());
                        if let Err(err) = oyl_core::write_file_atomic(path, edited) {
                            log::warn!("self-test agent: {err}");
                        }
                    }
                    let terminal = event.is_terminal();
                    if tx.send_blocking(event).is_err() || terminal {
                        break;
                    }
                }
            })?;
        Ok(TurnHandle::new(rx, move || {
            stopped.store(true, Ordering::SeqCst);
            cancel_inner();
        }))
    }
}

/// Appends a line to a file on disk, as the user (or another app) would.
fn append_line(path: &Path, line: &str) {
    let text = fs::read_to_string(path).unwrap_or_default();
    let edited = format!("{}\n{line}\n", text.trim_end());
    if let Err(err) = oyl_core::write_file_atomic(path, edited) {
        log::warn!("self-test: {err}");
    }
}

/// The start of a line of prose in the article template, where the
/// editor's AI is tried.
const PROSE_LINE: &str = "Ask the agent to draft";

/// What [`FakeRewriter`] appends to the text it polishes.
const POLISHED_MARK: &str = "(polished by the self-test)";

/// The bibliography [`FakeChecker`] was last given.
static FACT_BIB: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// A rewriter that appends [`POLISHED_MARK`].
struct FakeRewriter;

impl Rewriter for FakeRewriter {
    fn name(&self) -> String {
        "Self-test rewriter".into()
    }

    fn rewrite(&self, request: RewriteRequest) -> anyhow::Result<TurnHandle> {
        let text = format!("{} {POLISHED_MARK}", request.text.trim_end());
        let (tx, rx) = async_channel::unbounded();
        tx.send_blocking(AgentEvent::TextDelta { text: text.clone() })?;
        tx.send_blocking(AgentEvent::Completed {
            success: true,
            summary: text,
            session: None,
        })?;
        Ok(TurnHandle::new(rx, || {}))
    }
}

/// A fact checker that finds one doubtful claim among seven.
struct FakeChecker;

impl FactChecker for FakeChecker {
    fn name(&self) -> String {
        "Self-test checker".into()
    }

    fn check(&self, request: FactCheckRequest) -> anyhow::Result<FactCheckHandle> {
        if let Ok(mut bib) = FACT_BIB.lock() {
            bib.clone_from(&request.bibliography);
        }
        let quote = request
            .text
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        let (tx, rx) = async_channel::unbounded();
        tx.send_blocking(FactCheckEvent::Finding(FactFinding {
            quote,
            level: FindingLevel::Questionable,
            problem: "The self-test doubts this.".into(),
            suggestion: None,
            sources: Vec::new(),
        }))?;
        tx.send_blocking(FactCheckEvent::Done { claims: 7 })?;
        Ok(FactCheckHandle::new(rx, || {}))
    }
}

/// A discovery run: two progress reports, then a long wait (it is stopped
/// before it ends).
fn discovery_script(dir: &Path) -> Vec<ScriptStep> {
    let phases: Vec<String> = DISCOVERY_PHASES
        .iter()
        .map(|phase| (*phase).to_string())
        .collect();
    let progress = |phase: usize, step: u32, fraction: f32| {
        AgentEvent::Progress(oyl_model::agent::Progress {
            phase: phases[phase].clone(),
            phases: phases.clone(),
            step: Some(step),
            total: Some(5),
            fraction: Some(fraction),
            metrics: vec![
                ("accuracy".into(), "0.931".into()),
                ("ideas tried".into(), "3".into()),
            ],
            state_dir: Some(dir.to_path_buf()),
        })
    };
    let step = |delay: u64, event: AgentEvent| ScriptStep {
        delay: Duration::from_millis(delay),
        event,
    };
    vec![
        step(60, progress(0, 1, 0.2)),
        step(
            60,
            AgentEvent::ToolStarted {
                id: "eval-1".into(),
                name: "Bash".into(),
                title: "Run python eval.py --split dev".into(),
                detail: None,
            },
        ),
        step(60, progress(1, 2, 0.4)),
        step(
            30_000,
            AgentEvent::Completed {
                success: true,
                summary: "Discovered.".into(),
                session: None,
            },
        ),
    ]
}

/// The id of [`QuickHarness`].
const QUICK_ID: &str = "self-test-quick";

/// The effort of the last turn [`QuickHarness`] was asked to run.
static QUICK_EFFORT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
/// The capability of the last turn [`QuickHarness`] was asked to run.
static QUICK_CAPABILITY: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
/// The composer's placeholder in [`QuickHarness`]'s Discover mode.
const DISCOVER_PLACEHOLDER: &str = "Describe the goal, the metrics, the data and the budget…";

/// A second scripted agent with more models: its "Long task" model runs
/// for a few seconds, the others answer at once without editing.
struct QuickHarness {
    quick: ScriptedHarness,
    long: ScriptedHarness,
    /// A discovery run: progress through its phases, state in a folder.
    discover: ScriptedHarness,
}

/// The phases [`QuickHarness`]'s discovery reports.
const DISCOVERY_PHASES: [&str; 3] = [
    "Searching the state of the art",
    "Evaluating ideas",
    "Writing up",
];
/// The id of the frontier's best row in the discovery fixture.
const FRONTIER_BEST: &str = "i3";

/// Writes the discovery fixture: a Pareto frontier and the explored ideas.
fn write_discovery_fixture(dir: &Path) {
    let pareto = "# Pareto frontier (dev)\n\n| id | parent | commit | idea | aspect | accuracy ↑ | latency_ms ↓ |\n|---|---|---|---|---|---:|---:|\n| i3 | i1 | c3 | Wider window | model | 0.931 | 41.2 |\n| i1 | base | c1 | Spectral features | data | 0.905 | 30.1 |\n";
    let explored = "# Explored ideas\n\n| id | parent | commit | idea | aspect | accuracy | latency_ms | draw | status | observations |\n|---|---|---|---|---|---|---|---|---|---|\n| base | - | c0 | Baseline | - | 0.880 | 28.0 | r=0.10 | kept | The starting point. |\n| i1 | base | c1 | Spectral features | data | 0.905 | 30.1 | r=0.40 | kept | Helps on owls. |\n| i3 | i1 | c3 | Wider window | model | 0.931 | 41.2 | r=0.70 | kept | Best so far. |\n";
    let written = fs::create_dir_all(dir)
        .and_then(|()| fs::write(dir.join("pareto.md"), pareto))
        .and_then(|()| fs::write(dir.join("explored-ideas.md"), explored));
    if let Err(err) = written {
        log::warn!("self-test: the discovery fixture: {err}");
    }
}

impl QuickHarness {
    /// The model whose turns take a while.
    const LONG: &str = "quick-long";

    fn new(discovery: PathBuf) -> Self {
        write_discovery_fixture(&discovery);
        let step = |delay: Duration, event: AgentEvent| ScriptStep { delay, event };
        let mut info =
            ScriptedHarness::from_events(QUICK_ID, Vec::new(), Duration::from_millis(60)).info();
        info.name = "Quick agent".into();
        info.tagline = "A scripted agent that answers at once".into();
        info.models = [
            ("quick", "Quick"),
            ("quick-rapid", "Rapid (fastest)"),
            (Self::LONG, "Long task (slow)"),
            ("quick-deep", "Deep"),
            ("quick-wide", "Wide"),
        ]
        .into_iter()
        .map(|(id, label)| ModelChoice {
            id: id.into(),
            label: label.into(),
        })
        .collect();
        info.default_model = Some("quick".into());
        info.efforts = ["low", "medium", "high", "xhigh", "max"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        info.default_effort = Some("medium".into());
        info.capabilities = vec![
            Capability {
                id: oyl_chat::DISCOVER.into(),
                name: "Discover".into(),
                description: "Researches on its own for a long while.".into(),
                long_running: true,
                placeholder: DISCOVER_PLACEHOLDER.into(),
            },
            Capability {
                id: "review".into(),
                name: "Review".into(),
                description: "Reviews the paper like a referee.".into(),
                long_running: false,
                placeholder: "What should the review look at?".into(),
            },
        ];
        let quick = vec![
            step(
                Duration::from_millis(60),
                AgentEvent::TextDelta {
                    text: "Owls are birds of prey.".into(),
                },
            ),
            step(
                Duration::from_millis(60),
                AgentEvent::Completed {
                    success: true,
                    summary: "Answered.".into(),
                    session: None,
                },
            ),
        ];
        let long = vec![
            step(
                Duration::from_millis(60),
                AgentEvent::ToolStarted {
                    id: "search-1".into(),
                    name: "Search".into(),
                    title: "Searching: owl hearing".into(),
                    detail: None,
                },
            ),
            step(
                Duration::from_millis(3500),
                AgentEvent::ToolFinished {
                    id: "search-1".into(),
                    ok: true,
                    output: Some("12 results".into()),
                },
            ),
            step(
                Duration::from_millis(60),
                AgentEvent::Completed {
                    success: true,
                    summary: "Searched.".into(),
                    session: None,
                },
            ),
        ];
        Self {
            quick: ScriptedHarness::new(info.clone(), quick),
            long: ScriptedHarness::new(info.clone(), long),
            discover: ScriptedHarness::new(info, discovery_script(&discovery)),
        }
    }
}

impl Harness for QuickHarness {
    fn info(&self) -> HarnessInfo {
        self.quick.info()
    }

    fn start(&self, request: TurnRequest) -> anyhow::Result<TurnHandle> {
        if let Ok(mut effort) = QUICK_EFFORT.lock() {
            effort.clone_from(&request.effort);
        }
        if let Ok(mut capability) = QUICK_CAPABILITY.lock() {
            capability.clone_from(&request.capability);
        }
        if request.capability.as_deref() == Some(oyl_chat::DISCOVER) {
            self.discover.start(request)
        } else if request.model.as_deref() == Some(Self::LONG) {
            self.long.start(request)
        } else {
            self.quick.start(request)
        }
    }
}
