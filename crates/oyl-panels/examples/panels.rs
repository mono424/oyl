//! The five overlays on a frosted window, one at a time inside a raised
//! card at the panel's preferred width, over a temporary paper: a few
//! sources (one cited key missing), three versions of history (Oyl, the
//! agent, typing), two generated figures, the template's headings and two
//! compiler problems.
//!
//! Keys: 1–5 (or ⌘1–⌘5) switch the panel — Sources, History, Outline,
//! Figures, Problems — and T (or ⌘T) toggles light and dark. Plain 1–5 and
//! T type into a focused text field instead. Events are printed to stderr.
//!
//! `OYL_SMOKE=1` opens the window without taking focus, cycles through
//! the five panels (selecting a version in History so its diff shows),
//! switches the theme once, prints what each panel holds, and quits after
//! about four seconds.

use std::cell::Cell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Context as _;
use gpui_kit::*;
use oyl_core::Project;
use oyl_history::History;
use oyl_model::{Author, Diagnostic, Engine, OutlineItem, SourceLocation};
use oyl_panels::{
    FiguresEvent, FiguresPanel, HistoryEvent, HistoryPanel, OutlineEvent, OutlinePanel,
    ProblemsEvent, ProblemsPanel, SourcesEvent, SourcesPanel,
};
use oyl_ui::prelude::*;

actions!(
    panels_example,
    [ShowSources, ShowHistory, ShowOutline, ShowFigures, ShowProblems, ToggleTheme]
);

const CONTEXT: &str = "PanelsExample";
const NAMES: [&str; 5] = ["Sources", "History", "Outline", "Figures", "Problems"];

// -- The temporary paper ------------------------------------------------------

const MORE_SOURCES: &str = r#"
@inproceedings{vaswani2017,
  author    = {Vaswani, Ashish and Shazeer, Noam and Parmar, Niki and Uszkoreit, Jakob and Jones, Llion and Gomez, Aidan N. and Kaiser, Lukasz and Polosukhin, Illia},
  title     = {Attention Is All You Need},
  booktitle = {Advances in Neural Information Processing Systems},
  year      = {2017},
  url       = {https://arxiv.org/abs/1706.03762},
}

@book{lamport1994,
  author    = {Lamport, Leslie},
  title     = {{LaTeX}: A Document Preparation System},
  publisher = {Addison-Wesley},
  year      = {1994},
}

@article{hopper1952,
  author  = {Hopper, Grace Murray},
  title   = {The Education of a Computer},
  journal = {Proceedings of the ACM National Meeting},
  year    = {1952},
}
"#;

const RESULTS: &str = "= Results\n\nAttention alone is enough to translate well @vaswani2017, \
and a good typesetter helps @lamport1994[p. 12]. Earlier work on the same \
question @smith2020 is discussed below.\n\n\
#figure(image(\"figures/attention.png\", width: 80%), caption: [Attention weights.]) <fig:attention>\n\n";

struct Paper {
    /// Removed when the example quits.
    dir: tempfile::TempDir,
    root: PathBuf,
    bib: PathBuf,
    source_files: Vec<PathBuf>,
    texts: Vec<String>,
    outline: Vec<OutlineItem>,
    current: SourceLocation,
    diagnostics: Vec<Diagnostic>,
}

fn write_figure(path: &std::path::Path, width: u32, height: u32, bars: bool) -> anyhow::Result<()> {
    let image = image::RgbaImage::from_fn(width, height, |x, y| {
        if bars {
            let band = x * 7 / width;
            let top = height - (height * (2 + band * 11 % 7) / 10);
            if y > top && x % (width / 7) > 12 {
                image::Rgba([173, 101, 0, 255])
            } else {
                image::Rgba([252, 251, 248, 255])
            }
        } else {
            let fx = x as f32 / width as f32;
            let fy = y as f32 / height as f32;
            let w = (1. - (fx - fy).abs()).powi(6);
            let v = (252. - 200. * w) as u8;
            image::Rgba([v, (v as f32 * 0.93) as u8, (v as f32 * 0.85) as u8, 255])
        }
    });
    image
        .save(path)
        .with_context(|| format!("writing {}", path.display()))
}

fn make_paper() -> anyhow::Result<Paper> {
    let dir = tempfile::tempdir().context("creating a temporary folder")?;
    let project = Project::create(dir.path(), "A Quiet Paper", Engine::Typst, "article", "Ada Lovelace")
        .context("creating the paper")?;
    let root = project.root().to_path_buf();
    let history = History::open(&root).context("opening the history")?;
    history
        .snapshot(Author::System, None)
        .context("recording the first version")?;

    let bib = project.bib_path();
    let mut bibtex = fs::read_to_string(&bib).context("reading refs.bib")?;
    bibtex.push_str(MORE_SOURCES);
    fs::write(&bib, bibtex).context("writing refs.bib")?;
    let main = project.main_path();
    let text = fs::read_to_string(&main).context("reading main.typ")?;
    fs::write(&main, text.replacen("= Results\n\n", RESULTS, 1)).context("writing main.typ")?;
    history
        .snapshot(Author::Agent, Some("Agent: add the results and their sources"))
        .context("recording the agent's version")?;
    let text = fs::read_to_string(&main).context("reading main.typ")?;
    fs::write(
        &main,
        text.replacen("Start here.", "Start here, and keep going.", 1),
    )
    .context("writing main.typ")?;
    history.record_typing().context("recording typing")?;

    write_figure(&root.join("figures/attention.png"), 1200, 800, false)?;
    write_figure(&root.join("figures/results-by-size.png"), 900, 600, true)?;

    let source_files = project.source_files();
    let texts = source_files
        .iter()
        .filter_map(|path| fs::read_to_string(path).ok())
        .collect();
    let outline = oyl_core::project_outline(&project);
    let current = outline
        .get(1)
        .map(|item| SourceLocation::new(&item.location.file, item.location.line + 2, 1))
        .unwrap_or_else(|| SourceLocation::new(&main, 1, 1));
    let results_line = fs::read_to_string(&main)
        .ok()
        .and_then(|text| text.lines().position(|l| l.contains("@smith2020")))
        .map(|ix| ix as u32 + 1)
        .unwrap_or(1);
    let diagnostics = vec![
        Diagnostic {
            hints: vec!["add it to refs.bib, or cite a key that is there".into()],
            ..Diagnostic::error("key `smith2020` does not exist in the bibliography")
                .at(&main, results_line, 1)
        },
        Diagnostic::warning("unknown font family: libertinus serif").at(&main, 15, 17),
    ];
    Ok(Paper {
        dir,
        root,
        bib,
        source_files,
        texts,
        outline,
        current,
        diagnostics,
    })
}

// -- The example window ---------------------------------------------------------

struct Demo {
    sources: Entity<SourcesPanel>,
    history: Entity<HistoryPanel>,
    outline: Entity<OutlinePanel>,
    figures: Entity<FiguresPanel>,
    problems: Entity<ProblemsPanel>,
    current: usize,
    /// Frames drawn, counted by a zero-size canvas (for the smoke tour).
    frames: Rc<Cell<usize>>,
    paper: Option<tempfile::TempDir>,
    _subscriptions: Vec<Subscription>,
}

impl Demo {
    fn new(paper: Paper, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Paper {
            dir,
            root,
            bib,
            source_files,
            texts,
            outline,
            current,
            diagnostics,
        } = paper;
        let sources = cx.new(|cx| SourcesPanel::new(root.clone(), bib, Engine::Typst, window, cx));
        sources.update(cx, |panel, cx| panel.refresh(source_files, cx));
        let history = cx.new(|cx| HistoryPanel::new(root.clone(), window, cx));
        let outline_panel = cx.new(|cx| OutlinePanel::new(window, cx));
        outline_panel.update(cx, |panel, cx| {
            panel.set_items(outline, cx);
            panel.set_current(Some(current), cx);
        });
        let figures = cx.new(|cx| FiguresPanel::new(root, Engine::Typst, window, cx));
        figures.update(cx, |panel, cx| panel.refresh(texts, cx));
        let problems = cx.new(|cx| ProblemsPanel::new(window, cx));
        problems.update(cx, |panel, cx| panel.set_diagnostics(diagnostics, cx));

        let subscriptions = vec![
            cx.subscribe(&sources, |_, _, event: &SourcesEvent, _| eprintln!("sources: {event:?}")),
            cx.subscribe(&history, |_, _, event: &HistoryEvent, _| eprintln!("history: {event:?}")),
            cx.subscribe(&outline_panel, |_, _, event: &OutlineEvent, _| {
                eprintln!("outline: {event:?}")
            }),
            cx.subscribe(&figures, |_, _, event: &FiguresEvent, _| eprintln!("figures: {event:?}")),
            cx.subscribe(&problems, |_, _, event: &ProblemsEvent, _| eprintln!("problems: {event:?}")),
            // The card follows the panels' widths (History widens for a diff).
            cx.observe(&sources, |_, _, cx| cx.notify()),
            cx.observe(&history, |_, _, cx| cx.notify()),
            cx.observe(&outline_panel, |_, _, cx| cx.notify()),
            cx.observe(&figures, |_, _, cx| cx.notify()),
            cx.observe(&problems, |_, _, cx| cx.notify()),
            cx.on_app_quit(|demo: &mut Demo, _| {
                drop(demo.paper.take());
                async {}
            }),
        ];
        let demo = Demo {
            sources,
            history,
            outline: outline_panel,
            figures,
            problems,
            current: 0,
            frames: Rc::new(Cell::new(0)),
            paper: Some(dir),
            _subscriptions: subscriptions,
        };
        let focus = demo.focus_of(0, cx);
        window.focus(&focus, cx);
        demo
    }

    fn focus_of(&self, ix: usize, cx: &App) -> FocusHandle {
        match ix {
            0 => self.sources.focus_handle(cx),
            1 => self.history.focus_handle(cx),
            2 => self.outline.focus_handle(cx),
            3 => self.figures.focus_handle(cx),
            _ => self.problems.focus_handle(cx),
        }
    }

    fn show(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.current = ix.min(NAMES.len() - 1);
        let focus = self.focus_of(self.current, cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    fn panel(&self, cx: &App) -> (AnyView, Pixels) {
        match self.current {
            0 => (self.sources.clone().into(), self.sources.read(cx).preferred_width()),
            1 => (self.history.clone().into(), self.history.read(cx).preferred_width()),
            2 => (self.outline.clone().into(), self.outline.read(cx).preferred_width()),
            3 => (self.figures.clone().into(), self.figures.read(cx).preferred_width()),
            _ => (self.problems.clone().into(), self.problems.read(cx).preferred_width()),
        }
    }
}

impl Render for Demo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let k = cx.tokens().clone();
        let frames = self.frames.clone();
        let (panel, width) = self.panel(cx);
        let tabs = NAMES
            .iter()
            .enumerate()
            .map(|(ix, name)| {
                div()
                    .text_color(if ix == self.current { k.text } else { k.text_faint })
                    .child(format!("{} {name}", ix + 1))
            })
            .collect::<Vec<_>>();
        div()
            .size_full()
            .relative()
            .key_context(CONTEXT)
            .font_family(k.ui_font.clone())
            .text_color(k.text)
            .on_action(cx.listener(|this, _: &ShowSources, window, cx| this.show(0, window, cx)))
            .on_action(cx.listener(|this, _: &ShowHistory, window, cx| this.show(1, window, cx)))
            .on_action(cx.listener(|this, _: &ShowOutline, window, cx| this.show(2, window, cx)))
            .on_action(cx.listener(|this, _: &ShowFigures, window, cx| this.show(3, window, cx)))
            .on_action(cx.listener(|this, _: &ShowProblems, window, cx| this.show(4, window, cx)))
            .on_action(cx.listener(|_, _: &ToggleTheme, _, cx| {
                oyl_ui::toggle(cx);
            }))
            .child(
                div()
                    .size_full()
                    .flex()
                    .justify_center()
                    .pt(px(48.))
                    .pb(px(24.))
                    .child(
                        oyl_ui::raised(cx)
                            .w(width)
                            .h_full()
                            .overflow_hidden()
                            .child(panel),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top(px(14.))
                    .left(px(84.))
                    .right(px(24.))
                    .flex()
                    .gap(px(14.))
                    .text_xs()
                    .children(tabs)
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_color(k.text_faint)
                            .child("1–5 or ⌘1–⌘5 switch · T or ⌘T theme"),
                    ),
            )
            .child(canvas(move |_, _, _| frames.set(frames.get() + 1), |_, _, _, _| {}).size_0())
    }
}

fn main() {
    let smoke = std::env::var_os("OYL_SMOKE").is_some_and(|v| v == "1");
    let paper = match make_paper() {
        Ok(paper) => paper,
        Err(error) => {
            eprintln!("could not set up the example paper: {error:#}");
            std::process::exit(1);
        }
    };
    eprintln!("example paper: {}", paper.root.display());
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            oyl_ui::install(Appearance::System, cx);
            let plain = Some("PanelsExample && !Input");
            let anywhere = Some(CONTEXT);
            cx.bind_keys([
                KeyBinding::new("1", ShowSources, plain),
                KeyBinding::new("2", ShowHistory, plain),
                KeyBinding::new("3", ShowOutline, plain),
                KeyBinding::new("4", ShowFigures, plain),
                KeyBinding::new("5", ShowProblems, plain),
                KeyBinding::new("t", ToggleTheme, plain),
                KeyBinding::new("cmd-1", ShowSources, anywhere),
                KeyBinding::new("cmd-2", ShowHistory, anywhere),
                KeyBinding::new("cmd-3", ShowOutline, anywhere),
                KeyBinding::new("cmd-4", ShowFigures, anywhere),
                KeyBinding::new("cmd-5", ShowProblems, anywhere),
                KeyBinding::new("cmd-t", ToggleTheme, anywhere),
            ]);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1200.), px(880.)),
                    cx,
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Oyl panels".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(18.), px(18.))),
                }),
                window_background: WindowBackgroundAppearance::Blurred,
                focus: !smoke,
                // An unfocused normal window opens behind the active app,
                // and an occluded window draws no frames. Floating keeps
                // the smoke window visible (and painting) without taking
                // focus.
                kind: if smoke {
                    WindowKind::Floating
                } else {
                    WindowKind::Normal
                },
                ..Default::default()
            };
            let opened = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Demo::new(paper, window, cx))
            });
            let (handle, demo) = match opened {
                Ok(opened) => opened,
                Err(error) => {
                    eprintln!("could not open the panels window: {error:#}");
                    cx.quit();
                    return;
                }
            };
            if smoke {
                run_smoke_tour(handle, demo, cx);
            } else {
                cx.activate(true);
            }
        });
}

/// A scripted tour for unattended runs; prints what it observes.
fn run_smoke_tour(handle: AnyWindowHandle, demo: Entity<Demo>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let wait = async |cx: &mut AsyncApp, millis: u64| {
            cx.background_executor()
                .timer(Duration::from_millis(millis))
                .await;
        };
        let show = |cx: &mut AsyncApp, ix: usize| {
            handle
                .update(cx, |_, window, cx| {
                    demo.update(cx, |demo, cx| demo.show(ix, window, cx))
                })
                .ok();
        };

        wait(cx, 700).await;
        let visible = handle.update(cx, |_, window, _| window.is_visible()).unwrap_or(false);
        demo.read_with(cx, |demo, cx| {
            let sources = demo.sources.read(cx);
            eprintln!(
                "smoke: window visible: {visible}, {} frames drawn; sources: {} ({}), missing: {:?}, vaswani2017 cited {}×",
                demo.frames.get(),
                sources.source_count(),
                sources.keys().join(", "),
                sources.missing_keys(),
                sources.cited_count("vaswani2017"),
            );
        });

        show(cx, 1);
        wait(cx, 300).await;
        demo.update(cx, |demo, cx| {
            let numbers: Vec<u32> = demo.history.read(cx).versions().iter().map(|v| v.number).collect();
            eprintln!("smoke: history versions {numbers:?}");
            demo.history.update(cx, |panel, cx| panel.select_number(2, cx));
        });
        wait(cx, 500).await;
        demo.read_with(cx, |demo, cx| {
            let panel = demo.history.read(cx);
            eprintln!(
                "smoke: version {:?} selected, {} diff rows, card {:?} wide",
                panel.selected_number(),
                panel.diff_rows().map_or(0, |rows| rows.len()),
                panel.preferred_width(),
            );
        });

        show(cx, 2);
        wait(cx, 500).await;
        demo.read_with(cx, |demo, cx| {
            let panel = demo.outline.read(cx);
            eprintln!(
                "smoke: outline {} headings, current {:?}",
                panel.shown().len(),
                panel.current()
            );
        });

        show(cx, 3);
        cx.update(|cx| {
            oyl_ui::toggle(cx);
        });
        wait(cx, 600).await;
        demo.read_with(cx, |demo, cx| {
            let panel = demo.figures.read(cx);
            let figures: Vec<String> = panel
                .figures()
                .iter()
                .map(|f| format!("{} (used: {})", f.rel_path, f.used))
                .collect();
            eprintln!("smoke: figures {figures:?}");
        });

        show(cx, 4);
        wait(cx, 500).await;
        demo.read_with(cx, |demo, cx| {
            let panel = demo.problems.read(cx);
            eprintln!(
                "smoke: {} problems, fix prompt: {} chars",
                panel.diagnostics().len(),
                panel.fix_prompt().map_or(0, |p| p.len())
            );
        });

        show(cx, 0);
        wait(cx, 400).await;
        demo.update(cx, |demo, cx| {
            eprintln!("smoke: done after {} frames", demo.frames.get());
            drop(demo.paper.take());
            cx.quit();
        });
    })
    .detach();
}
