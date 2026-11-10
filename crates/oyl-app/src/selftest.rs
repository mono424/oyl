//! `oyl --self-test`: drives the real window through Oyl's main paths
//! and exits 0 when every check passes, 1 otherwise.
//!
//! Everything happens in a fresh temporary folder: the data folder (as
//! `OYL_DATA_DIR` would set it) and the papers. The window opens without
//! taking focus. The agent is a scripted harness that edits `main.typ` on
//! disk in the middle of its turn, the way a real agent would.

use std::fmt::Display;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::{
    AnyWindowHandle, App, AsyncApp, Bounds, Entity, Keystroke, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, Pixels, Window, point,
};
use oyl_agent::{ScriptStep, ScriptedHarness};
use oyl_core::LayoutMode;
use oyl_editor::EditorEvent;
use oyl_model::agent::{AgentEvent, Harness, HarnessInfo, TurnHandle, TurnRequest};
use oyl_model::typeset::RenderedDocument;
use oyl_model::{Author, Engine, SourceLocation};

use crate::actions;
use crate::persist::{AppPaths, WindowFrame};
use crate::shell::{Agents, Launch, Shell, ShellOptions};

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
    let temp = match tempfile::Builder::new()
        .prefix("oyl-self-test-")
        .tempdir()
    {
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

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            crate::init_app(cx);
            let harness: Arc<dyn Harness> = Arc::new(EditingHarness::new());
            let opened = open_shell(&paths, Agents::Given(vec![harness.clone()]), cx);
            let (window, shell) = match opened {
                Ok(opened) => opened,
                Err(err) => {
                    eprintln!("self-test: no window: {err:#}");
                    std::process::exit(1);
                }
            };
            let paths = paths.clone();
            let papers = papers.clone();
            cx.spawn(async move |cx| {
                let mut report = Report::new();
                let test = SelfTest {
                    paths: paths.clone(),
                    papers,
                    window,
                    shell,
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

fn open_shell(
    paths: &AppPaths,
    agents: Agents,
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
        self.slides(report, cx).await;
        self.agent_turn(report, harness, &article, cx).await;
        self.media(report, &article, cx).await;
        self.exports(report, cx).await;
        self.palette(report, &article, cx).await;
        self.panels(report, &article, cx).await;
        self.reopen(report, &article, cx).await;
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
        report.step("3. Write, Read, Split; dark and light");
        let checks: [(Box<dyn gpui_kit::Action>, LayoutMode); 3] = [
            (Box::new(actions::WriteMode), LayoutMode::Write),
            (Box::new(actions::ReadMode), LayoutMode::Read),
            (Box::new(actions::SplitMode), LayoutMode::Split),
        ];
        for (action, mode) in checks {
            self.dispatch(action, cx);
            let (now, zen, saved) = cx.update(|cx| {
                let shell = self.shell.read(cx);
                let zen = shell.editor.read(cx).is_zen();
                (shell.mode, zen, shell.settings.layout_mode)
            });
            report.check(
                now == mode && zen == (mode == LayoutMode::Write) && saved == mode,
                format!(
                    "{}: on screen, editor zen {}, remembered",
                    crate::layout::mode_label(mode),
                    if zen { "on" } else { "off" }
                ),
            );
        }
        // In Split, the paper gets exactly the right column below the header.
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

    // -- 3b. The card: Source and Agent slides, the picker ----------------------

    /// The slide shown, whether it settled, and where focus is.
    fn slide_state(&self, cx: &mut AsyncApp) -> (crate::shell::Slide, bool, bool, bool) {
        self.with_shell(cx, |shell, window, cx| {
            let settled = !shell.slides.progress.is_animating(std::time::Instant::now());
            let chat = shell.chat_has_focus(window, cx);
            let content = match shell.mode {
                LayoutMode::Read => {
                    use gpui_kit::Focusable as _;
                    shell.preview.focus_handle(cx).is_focused(window)
                }
                _ => {
                    use gpui_kit::Focusable as _;
                    shell.editor.focus_handle(cx).is_focused(window)
                }
            };
            (shell.slides.shown, settled, chat, content)
        })
        .unwrap_or((crate::shell::Slide::Source, false, false, false))
    }

    async fn settle(&self, cx: &mut AsyncApp) {
        self.wait(cx, Duration::from_millis(400), |_| false).await;
    }

    /// The agent's card where the mode puts it: over the paper column in
    /// Split, 440 px against the right edge in Read and Write.
    fn check_agent_card(&self, report: &mut Report, mode: LayoutMode, cx: &mut AsyncApp) {
        let measured = self.with_shell(cx, |shell, window, _| {
            (
                shell.slides.agent_bounds.get(),
                shell.content_bounds.get(),
                window.viewport_size(),
                shell.settings.split_ratio,
            )
        });
        let Some((card, content, viewport, ratio)) = measured else {
            return;
        };
        let near = |a: f32, b: f32| (a - b).abs() < 1.5;
        let gap = crate::layout::CARD_GAP;
        let left = f32::from(card.origin.x);
        let top = f32::from(card.origin.y);
        let right = left + f32::from(card.size.width);
        let bottom = top + f32::from(card.size.height);
        let (expected_left, width) =
            crate::layout::agent_card(mode, f32::from(content.size.width), ratio);
        let expected_left = f32::from(content.origin.x) + expected_left;
        report.check(
            near(left, expected_left)
                && near(right, expected_left + width)
                && near(right, f32::from(viewport.width) - gap)
                && near(top, crate::layout::HEADER_HEIGHT + gap)
                && near(bottom, f32::from(viewport.height) - gap),
            format!(
                "{}: the agent's card sits at ({left:.0},{top:.0})–({right:.0},{bottom:.0}){}",
                crate::layout::mode_label(mode),
                match mode {
                    LayoutMode::Split => ", over the paper column".to_string(),
                    _ => format!(", {:.0} px wide on the right", right - left),
                }
            ),
        );
    }

    /// In Split, the handle still resizes while the agent's card is open:
    /// code on the left, the agent's card on the right.
    async fn drag_split_while_agent_open(&self, report: &mut Report, cx: &mut AsyncApp) {
        self.frame(cx);
        let (handle, before, card_before) = cx.update(|cx| {
            let shell = self.shell.read(cx);
            (
                shell.handle.hit_bounds.get(),
                shell.settings.split_ratio,
                shell.slides.agent_bounds.get(),
            )
        });
        let from = point(
            handle.origin.x + handle.size.width / 2.,
            handle.origin.y + handle.size.height / 2.,
        );
        let to = point(from.x - gpui_kit::px(120.), from.y);
        let _ = self.window.update(cx, move |_, window, cx| {
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: from,
                    pressed_button: None,
                    modifiers: Modifiers::default(),
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: from,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: to,
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers::default(),
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position: to,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                }),
                cx,
            );
        });
        self.settle(cx).await;
        let (after, card_after) = cx.update(|cx| {
            let shell = self.shell.read(cx);
            (shell.settings.split_ratio, shell.slides.agent_bounds.get())
        });
        let moved = f32::from(card_before.origin.x) - f32::from(card_after.origin.x);
        report.check(
            after < before - 0.05 && (moved - 120.).abs() < 2.,
            format!(
                "Split: dragging the handle with the agent open resizes code vs agent ({before:.2} → {after:.2}, card moved {moved:.0} px)"
            ),
        );
        self.with_shell(cx, move |shell, _, cx| shell.set_split_ratio(before, cx));
        self.settle(cx).await;
    }

    async fn slides(&self, report: &mut Report, cx: &mut AsyncApp) {
        use crate::shell::Slide;
        report.step("3b. The source card, the agent's card, the picker");
        let modes: [(Box<dyn gpui_kit::Action>, LayoutMode); 3] = [
            (Box::new(actions::SplitMode), LayoutMode::Split),
            (Box::new(actions::WriteMode), LayoutMode::Write),
            (Box::new(actions::ReadMode), LayoutMode::Read),
        ];
        for (action, mode) in modes {
            let name = crate::layout::mode_label(mode);
            self.dispatch(action, cx);
            self.settle(cx).await;
            self.dispatch(Box::new(actions::FocusAgent), cx);
            self.settle(cx).await;
            let (slide, settled, chat, _) = self.slide_state(cx);
            report.check(
                slide == Slide::Agent && settled && chat,
                format!("{name}: ⌘L slides the agent's card in, caret in the composer"),
            );
            self.check_agent_card(report, mode, cx);
            if mode == LayoutMode::Split {
                self.drag_split_while_agent_open(report, cx).await;
            }
            self.press("escape", cx);
            self.settle(cx).await;
            let away = cx.update(|cx| self.shell.read(cx).slides.agent_bounds.get());
            report.check(
                away.size.width == Pixels::default(),
                format!("{name}: closed, the agent's card is off screen"),
            );
            let (slide, settled, _, content) = self.slide_state(cx);
            report.check(
                slide == Slide::Source && settled && content,
                format!(
                    "{name}: Esc in the composer slides back to the {}",
                    if mode == LayoutMode::Read { "paper" } else { "source" }
                ),
            );
            let button = cx.update(|cx| self.shell.read(cx).agent_button_bounds.get());
            self.click(button, cx);
            self.settle(cx).await;
            let (shown, ..) = self.slide_state(cx);
            self.click(button, cx);
            self.settle(cx).await;
            let (back, ..) = self.slide_state(cx);
            report.check(
                shown == Slide::Agent && back == Slide::Source,
                format!("{name}: the header's Agent button toggles the slides"),
            );
            self.dispatch(Box::new(actions::FocusAgent), cx);
            self.dispatch(Box::new(actions::FocusAgent), cx);
            self.settle(cx).await;
            let (slide, ..) = self.slide_state(cx);
            report.check(slide == Slide::Source, format!("{name}: ⌘L twice is back on the source"));
        }

        // The editor's scroll area ends inside the card, in Split and Write.
        for (action, mode) in [
            (Box::new(actions::SplitMode) as Box<dyn gpui_kit::Action>, LayoutMode::Split),
            (Box::new(actions::WriteMode), LayoutMode::Write),
        ] {
            self.dispatch(action, cx);
            self.settle(cx).await;
            let measured = self.with_shell(cx, |shell, window, cx| {
                let card = shell.slides.source_bounds.get();
                let text = shell.editor.read(cx).text_bounds(cx);
                (card, text, window.viewport_size(), shell.content_bounds.get())
            });
            let Some((card, Some(text), viewport, content)) = measured else {
                report.check(false, "the editor's text area was laid out");
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
            let (tl, tt, tr, tb) = edges(text);
            // The editor's own padding (oyl-editor): top, right, bottom,
            // left; the scroll viewport (and its clip) starts inside it.
            let (pt, pr, pb, pl) = match mode {
                LayoutMode::Write => (20., 32., 20., 32.),
                _ => (16., 20., 16., 12.),
            };
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
            let gap = crate::layout::CARD_GAP;
            let right_edge = match mode {
                LayoutMode::Split => {
                    let ratio = cx.update(|cx| self.shell.read(cx).settings.split_ratio);
                    f32::from(content.origin.x)
                        + crate::layout::clamp_split(ratio, f32::from(content.size.width))
                            * f32::from(content.size.width)
                        - gap
                }
                _ => f32::from(viewport.width) - gap,
            };
            let near = |a: f32, b: f32| (a - b).abs() < 1.5;
            report.check(
                near(cl, gap)
                    && near(ct, crate::layout::HEADER_HEIGHT + gap)
                    && near(cb, f32::from(viewport.height) - gap)
                    && near(cr, right_edge),
                format!(
                    "{}: the card keeps {gap:.0}px from the edges, the header and the column boundary",
                    crate::layout::mode_label(mode)
                ),
            );
        }

        // The picker opens on a click; a model picked there is used.
        self.dispatch(Box::new(actions::SplitMode), cx);
        self.dispatch(Box::new(actions::FocusAgent), cx);
        self.settle(cx).await;
        let chat = cx.update(|cx| self.shell.read(cx).chat.clone());
        let chip = cx.update(|cx| chat.read(cx).picker_bounds());
        self.click(chip, cx);
        let opened = cx.update(|cx| chat.read(cx).open_popover());
        report.check(
            opened == Some("agent"),
            format!(
                "a click on the composer's chip ({:.0},{:.0} {:.0}×{:.0}) opens the picker",
                f32::from(chip.origin.x),
                f32::from(chip.origin.y),
                f32::from(chip.size.width),
                f32::from(chip.size.height)
            ),
        );
        let before = cx.update(|cx| chat.read(cx).selection());
        self.dispatch(Box::new(oyl_chat::PopoverDown), cx);
        self.dispatch(Box::new(oyl_chat::PopoverConfirm), cx);
        let (after, open) = cx.update(|cx| (chat.read(cx).selection(), chat.read(cx).open_popover()));
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
        self.click(chip, cx);
        self.press("escape", cx);
        let open = cx.update(|cx| chat.read(cx).open_popover());
        let (slide, ..) = self.slide_state(cx);
        report.check(
            open.is_none() && slide == crate::shell::Slide::Agent,
            "Esc closes the picker first, the agent stays",
        );
        self.press("escape", cx);
        self.settle(cx).await;
        let (slide, ..) = self.slide_state(cx);
        report.check(slide == crate::shell::Slide::Source, "a second Esc goes back to the source");
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
        first("read mode", self, cx);
        self.dispatch(
            Box::new(gpui_kit::base::actions::Confirm { secondary: false }),
            cx,
        );
        let mode = self.snapshot(cx).mode;
        report.check(
            mode == Some(LayoutMode::Read) && !self.showing("palette", cx),
            "a command picked in the palette runs (Read mode)",
        );
        self.dispatch(Box::new(actions::SplitMode), cx);
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
        let (slide, ..) = self.slide_state(cx);
        report.check(
            slide == crate::shell::Slide::Agent,
            "and switched the card to the agent",
        );
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

    // -- 9. Reopening -------------------------------------------------------------

    async fn reopen(&self, report: &mut Report, article: &Path, cx: &mut AsyncApp) {
        report.step("9. Reopening from the same data folder restores the state");
        self.dispatch(Box::new(actions::ReadMode), cx);
        self.with_shell(cx, |shell, window, cx| {
            shell.set_split_ratio(0.62, cx);
            shell.save_everything(window, cx)
        });
        let _ = self
            .window
            .update(cx, |_, window, _| window.remove_window());
        self.wait(cx, Duration::from_millis(400), |_| false).await;

        let opened = cx.update(|cx| open_shell(&self.paths, Agents::Given(Vec::new()), cx));
        let Ok((window, shell)) = opened else {
            report.check(false, "a second window opens");
            return;
        };
        let second = SelfTest {
            paths: self.paths.clone(),
            papers: self.papers.clone(),
            window,
            shell,
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
        report.check(s.mode == Some(LayoutMode::Read), "in Read mode");
        let (split, slide) = cx.update(|cx| {
            let shell = second.shell.read(cx);
            (shell.settings.split_ratio, shell.slides.shown)
        });
        report.check(
            (split - 0.62).abs() < 1e-3,
            format!("with the split at {split:.2}"),
        );
        report.check(
            slide == crate::shell::Slide::Source,
            "and the card on its Source slide",
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
