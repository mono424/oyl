//! The chat pane alone, in a card under a narrow source column, driven by
//! two fake harnesses: "Sorcar" replays a scripted turn (setup note,
//! thinking, tool calls, file changes, Markdown with a code block, cost),
//! "Codex" is unavailable.
//!
//! Keys: ⌘J toggles light/dark, ⌘Q quits.
//!
//! `OYL_SMOKE=1` runs it unattended: the window does not take focus, two
//! scripted messages are sent (the second one is queued behind the first),
//! the theme flips once, and the app quits by itself after about 4 seconds,
//! printing what the pane reported.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::anyhow;
use gpui_kit::component::StyledExt as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Bounds, Context, Entity, Subscription, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, div, point, px, size,
};
use oyl_chat::{ChatEvent, ChatPane, EditorContext, PaperContext, ThreadStore};
use oyl_model::agent::{
    AgentEvent, Harness, HarnessId, HarnessInfo, ModelChoice, Readiness, TurnHandle, TurnRequest,
};
use oyl_model::{Diagnostic, Engine};
use oyl_ui::prelude::*;

gpui_kit::actions!(chat_example, [ToggleTheme, Quit]);

const MAIN_TYP: &str = "#set page(paper: \"a4\")\n#set text(font: \"Charter\")\n\n= Abstract\nWe study how attention scales, and we show that attention, when it is scaled, \
scales well in practice and also in theory @vaswani2017.\n\n= Introduction\nLarge models are everywhere.\n";

const REFS_BIB: &str = "@article{vaswani2017,\n  title = {Attention Is All You Need},\n  author = {Vaswani, Ashish and others},\n  year = {2017}\n}\n";

const ANSWER: &str = "Done. The abstract is now **two sentences shorter** and still cites \
the original paper:\n\n```typst\n= Abstract\nWe show that scaled attention works, \
in practice and in theory @vaswani2017.\n```\n\n- Removed the repeated claim about *scale*\n- \
Fixed a typo that broke the build\n- Added the DOI to `refs.bib`\n";

/// Replays one scripted turn on a background thread.
struct ScriptedHarness {
    /// Seconds per scripted step.
    step: f64,
    turns: AtomicUsize,
}

impl Harness for ScriptedHarness {
    fn info(&self) -> HarnessInfo {
        HarnessInfo {
            id: HarnessId::sorcar(),
            name: "Sorcar".into(),
            tagline: "KISS agent framework (demo replay)".into(),
            readiness: Readiness::Ready,
            status: "Demo: replays a scripted turn".into(),
            models: vec![
                ModelChoice {
                    id: "cc/opus".into(),
                    label: "Claude Opus (your Claude subscription)".into(),
                },
                ModelChoice {
                    id: "cc/sonnet".into(),
                    label: "Claude Sonnet (your Claude subscription)".into(),
                },
            ],
            default_model: Some("cc/opus".into()),
        }
    }

    fn start(&self, request: TurnRequest) -> anyhow::Result<TurnHandle> {
        let (tx, rx) = async_channel::unbounded();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let step = self.step;
        let number = self.turns.fetch_add(1, Ordering::SeqCst) + 1;
        thread::Builder::new()
            .name("scripted-harness".into())
            .spawn(move || run_script(request, number, step, &tx, &flag))?;
        Ok(TurnHandle::new(rx, move || {
            cancelled.store(true, Ordering::SeqCst)
        }))
    }
}

/// Sends the script; returns early (with a terminal event) when cancelled.
fn run_script(
    request: TurnRequest,
    number: usize,
    step: f64,
    tx: &async_channel::Sender<AgentEvent>,
    cancelled: &AtomicBool,
) {
    let root = request.project_root.clone();
    let main = request
        .main_file
        .clone()
        .unwrap_or_else(|| root.join("main.typ"));
    let refs = root.join("refs.bib");
    let session = Some(format!("demo-session-{number}"));

    let pause = |fraction: f64| thread::sleep(Duration::from_secs_f64(step * fraction));
    let send = |event: AgentEvent| tx.send_blocking(event).is_ok();
    let stop_if_cancelled = || {
        if cancelled.load(Ordering::SeqCst) {
            let _ = tx.send_blocking(AgentEvent::Completed {
                success: false,
                summary: "Stopped by the user.".into(),
                session: session.clone(),
            });
            true
        } else {
            false
        }
    };
    let stream = |text: &str, fraction: f64| -> bool {
        for word in text.split_inclusive(' ') {
            if cancelled.load(Ordering::SeqCst) {
                return false;
            }
            if !send(AgentEvent::TextDelta { text: word.into() }) {
                return false;
            }
            pause(fraction);
        }
        true
    };
    let tool = |id: &str, name: &str, title: &str, detail: &str| {
        send(AgentEvent::ToolStarted {
            id: format!("{number}-{id}"),
            name: name.into(),
            title: title.into(),
            detail: Some(detail.into()),
        })
    };
    let done = |id: &str, ok: bool, output: &str| {
        send(AgentEvent::ToolFinished {
            id: format!("{number}-{id}"),
            ok,
            output: Some(output.into()),
        })
    };

    if number == 1 {
        send(AgentEvent::Status {
            message: "Preparing Sorcar…".into(),
        });
        pause(1.0);
        send(AgentEvent::Status {
            message: "Preparing Sorcar… ready".into(),
        });
    }
    for chunk in [
        "The user wants the abstract tightened. ",
        "I'll read main.typ, cut the repeated clause, ",
        "and make sure it still compiles.",
    ] {
        send(AgentEvent::ThinkingDelta { text: chunk.into() });
        pause(0.5);
    }
    if stop_if_cancelled() {
        return;
    }

    tool("read", "Read", "Read main.typ", "{\"path\": \"main.typ\"}");
    pause(1.0);
    let content = fs::read_to_string(&main).unwrap_or_default();
    done("read", true, &content);
    tool(
        "grep",
        "Grep",
        "Search for “Abstract”",
        "{\"pattern\": \"Abstract\"}",
    );
    pause(0.6);
    done("grep", true, "main.typ:4:= Abstract");
    if stop_if_cancelled() {
        return;
    }

    if !stream("I'll tighten the abstract and keep its citation. ", 0.15) {
        stop_if_cancelled();
        return;
    }

    tool(
        "edit",
        "Edit",
        "Edit main.typ",
        "{\"path\": \"main.typ\", \"old\": \"We study…\"}",
    );
    pause(0.8);
    let edited = content.replace(
        "We study how attention scales, and we show that attention, when it is scaled, scales well in practice and also in theory",
        "We show that scaled attention works, in practice and in theory",
    );
    let _ = fs::write(&main, edited);
    send(AgentEvent::FileChanged { path: main.clone() });
    done("edit", true, "Replaced 2 lines");
    if stop_if_cancelled() {
        return;
    }

    tool(
        "compile",
        "Bash",
        "Compile with typst",
        "typst compile main.typ",
    );
    pause(1.0);
    let log = (1..=24)
        .map(|n| format!("warning: layout pass {n} took longer than expected"))
        .chain(std::iter::once(
            "error: unknown variable: abstrat\n  ┌─ main.typ:4:2".to_string(),
        ))
        .collect::<Vec<_>>()
        .join("\n");
    done("compile", false, &log);
    tool(
        "fix",
        "Edit",
        "Fix typo in main.typ",
        "{\"path\": \"main.typ\"}",
    );
    pause(0.6);
    send(AgentEvent::FileChanged { path: main.clone() });
    done("fix", true, "Replaced 1 line");
    tool(
        "bib",
        "Edit",
        "Add DOI to refs.bib",
        "{\"path\": \"refs.bib\"}",
    );
    pause(0.6);
    let _ = fs::write(
        &refs,
        REFS_BIB.replace(
            "year = {2017}",
            "year = {2017},\n  doi = {10.48550/arXiv.1706.03762}",
        ),
    );
    send(AgentEvent::FileChanged { path: refs });
    done("bib", true, "Added 1 field");
    tool(
        "recompile",
        "Bash",
        "Compile with typst",
        "typst compile main.typ",
    );
    pause(0.8);
    done("recompile", true, "compiled successfully in 41 ms");
    if stop_if_cancelled() {
        return;
    }

    if !stream(ANSWER, 0.05) {
        stop_if_cancelled();
        return;
    }
    send(AgentEvent::Usage {
        cost_usd: Some(0.0412 * number as f64),
        input_tokens: Some(18_400),
        output_tokens: Some(920),
    });
    send(AgentEvent::Completed {
        success: true,
        summary: "Tightened the abstract".into(),
        session,
    });
}

/// A harness that is not installed.
struct MissingHarness;

impl Harness for MissingHarness {
    fn info(&self) -> HarnessInfo {
        HarnessInfo {
            id: HarnessId::codex(),
            name: "Codex".into(),
            tagline: "OpenAI's coding agent".into(),
            readiness: Readiness::Unavailable,
            status: "Install Codex to use it: npm i -g @openai/codex".into(),
            models: Vec::new(),
            default_model: None,
        }
    }

    fn start(&self, _request: TurnRequest) -> anyhow::Result<TurnHandle> {
        Err(anyhow!("codex is not installed"))
    }
}

/// The source column with the pane docked at its bottom.
struct Demo {
    pane: Entity<ChatPane>,
    main_file: PathBuf,
    source: String,
    _subscriptions: Vec<Subscription>,
}

impl Demo {
    fn new(
        pane: Entity<ChatPane>,
        main_file: PathBuf,
        smoke: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let source = fs::read_to_string(&main_file).unwrap_or_default();
        let subscription = cx.subscribe(&pane, move |this, _, event: &ChatEvent, cx| {
            if smoke {
                println!("chat smoke: {event:?}");
            }
            match event {
                ChatEvent::FileChanged { path } if path == &this.main_file => {
                    this.source = fs::read_to_string(&this.main_file).unwrap_or_default();
                    cx.notify();
                }
                _ => {}
            }
        });
        Self {
            pane,
            main_file,
            source,
            _subscriptions: vec![subscription],
        }
    }
}

impl Render for Demo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let k = cx.tokens().clone();
        div()
            .size_full()
            .flex()
            .justify_center()
            .bg(k.veil)
            .text_color(k.text)
            .font_family(k.ui_font.clone())
            .child(
                div()
                    .v_flex()
                    .w(px(440.))
                    .h_full()
                    .pt(px(44.))
                    .bg(k.veil_strong)
                    .child(
                        div()
                            .id("source")
                            .h(px(200.))
                            .min_h_0()
                            .overflow_y_scroll()
                            .px(px(28.))
                            .py(px(12.))
                            .font_family(k.mono_font.clone())
                            .text_sm()
                            .text_color(k.text_soft)
                            .child(self.source.clone()),
                    )
                    .child(oyl_ui::hairline(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .child(self.pane.clone()),
                    ),
            )
    }
}

fn write_paper(root: &Path) -> anyhow::Result<PaperContext> {
    let main_file = root.join("main.typ");
    fs::write(&main_file, MAIN_TYP)?;
    fs::write(root.join("refs.bib"), REFS_BIB)?;
    Ok(PaperContext {
        root: root.to_path_buf(),
        engine: Engine::Typst,
        main_file,
    })
}

fn main() -> anyhow::Result<()> {
    let smoke = std::env::var_os("OYL_SMOKE").is_some_and(|value| value != "0");
    let paper_dir = tempfile::Builder::new()
        .prefix("oyl-chat-demo")
        .tempdir()?;
    let paper = write_paper(paper_dir.path())?;
    let root = paper.root.clone();
    let step = if smoke { 0.06 } else { 0.45 };

    // Quitting terminates the process from inside the run loop, so the
    // folder is removed by a quit observer rather than on return.
    let paper_dir = Rc::new(RefCell::new(Some(paper_dir)));

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            {
                let paper_dir = paper_dir.clone();
                cx.on_app_quit(move |_| {
                    paper_dir.borrow_mut().take();
                    async {}
                })
                .detach();
            }
            oyl_ui::install(Appearance::System, cx);
            cx.bind_keys([
                gpui_kit::KeyBinding::new("cmd-j", ToggleTheme, None),
                gpui_kit::KeyBinding::new("cmd-q", Quit, None),
            ]);
            cx.on_action(|_: &ToggleTheme, cx| {
                oyl_ui::toggle(cx);
            });
            cx.on_action(|_: &Quit, cx| cx.quit());

            let harnesses: Vec<Arc<dyn Harness>> = vec![
                Arc::new(ScriptedHarness {
                    step,
                    turns: AtomicUsize::new(0),
                }),
                Arc::new(MissingHarness),
            ];
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: None,
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(18.), px(18.))),
                }),
                window_background: WindowBackgroundAppearance::Blurred,
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(480.), px(820.)),
                    cx,
                ))),
                focus: !smoke,
                ..Default::default()
            };
            let main_file = paper.main_file.clone();
            let opened = gpui_kit::open_window(options, cx, move |window, cx| {
                let pane = cx.new(|cx| {
                    let mut pane = ChatPane::new(harnesses, window, cx);
                    pane.set_context_provider(Rc::new(|_cx: &App| EditorContext {
                        active_file: Some(PathBuf::from("main.typ")),
                        selection: Some(oyl_model::agent::Selection {
                            file: PathBuf::from("main.typ"),
                            start_line: 4,
                            end_line: 6,
                            text: "We study how attention scales…".into(),
                        }),
                        diagnostics: vec![
                            Diagnostic::error("unknown variable: abstrat").at("main.typ", 4, 2),
                            Diagnostic::warning("unused import"),
                        ],
                    }));
                    if smoke {
                        pane.set_before_turn(Rc::new(|turn, prompt, _| {
                            println!("chat smoke: before turn {turn}: {prompt}");
                        }));
                    }
                    pane.set_paper(Some(paper.clone()), cx);
                    if !smoke {
                        pane.focus_composer(window, cx);
                    }
                    pane
                });
                cx.new(|cx| Demo::new(pane, main_file, smoke, cx))
            });
            let (_, demo) = match opened {
                Ok(opened) => opened,
                Err(err) => {
                    eprintln!("could not open the window: {err:#}");
                    cx.quit();
                    return;
                }
            };
            let pane = demo.read(cx).pane.clone();

            if smoke {
                let root = root.clone();
                cx.spawn(async move |cx| {
                    let wait = |cx: &mut gpui_kit::AsyncApp, ms| {
                        cx.background_executor().timer(Duration::from_millis(ms))
                    };
                    wait(cx, 300).await;
                    pane.update(cx, |pane, cx| {
                        pane.send("Tighten the abstract and fix the compile errors", cx)
                    });
                    wait(cx, 300).await;
                    // Arrives while the first turn runs: queued, sent after it.
                    pane.update(cx, |pane, cx| pane.send("Now shorten the introduction", cx));
                    wait(cx, 1200).await;
                    cx.update(|cx| {
                        oyl_ui::toggle(cx);
                    });
                    wait(cx, 2200).await;
                    let (running, turns) = pane
                        .read_with(cx, |pane, _| (pane.is_running(), pane.thread().turns.len()));
                    let saved = ThreadStore::for_paper(&root)
                        .list()
                        .map(|threads| threads.first().map(|t| t.turns).unwrap_or(0))
                        .unwrap_or(0);
                    println!("chat smoke: running={running} turns={turns} saved_turns={saved}");
                    cx.update(|cx| cx.quit());
                })
                .detach();
            } else {
                cx.activate(true);
            }
        });

    Ok(())
}
