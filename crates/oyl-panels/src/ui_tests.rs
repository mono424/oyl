//! The panels in a headless GPUI window, driven through the real key
//! bindings, input handling and event dispatch of the test platform: filter,
//! paste BibTeX, remove with a warning, select a version and restore, jump.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, ClipboardItem, Context, Entity, EventEmitter,
    ExternalPaths, FileDropEvent, Focusable, Point, Render, TestAppContext, VisualTestContext,
    Window, WindowBounds, WindowOptions, point, px, size,
};
use oyl_history::History;
use oyl_model::{Author, Diagnostic, Engine, OutlineItem, SourceLocation};

use crate::{
    FiguresEvent, FiguresPanel, HistoryEvent, HistoryPanel, OutlineEvent, OutlinePanel,
    ProblemsEvent, ProblemsPanel, SourcesEvent, SourcesPanel,
};

const WIDTH: f32 = 1100.;
const HEIGHT: f32 = 900.;

struct Harness<V: 'static> {
    window: AnyWindowHandle,
    view: Entity<V>,
}

impl<V: Render + Focusable> Harness<V> {
    fn open(
        cx: &mut TestAppContext,
        build: impl FnOnce(&mut Window, &mut Context<V>) -> V + 'static,
    ) -> Self {
        cx.update(|cx| {
            gpui_kit::init(cx);
            oyl_ui::install(oyl_ui::Appearance::Light, cx);
        });
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Point::default(),
                        size: size(px(WIDTH), px(HEIGHT)),
                    })),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| build(window, cx)),
            )
            .unwrap()
        });
        let harness = Harness { window, view };
        harness.frame(cx);
        harness
    }

    /// Draws a frame and lets every background job and its follow-up run.
    fn frame(&self, cx: &mut TestAppContext) {
        for _ in 0..3 {
            cx.update_window(self.window, |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
            cx.run_until_parked();
        }
    }

    fn focus(&self, cx: &mut TestAppContext) {
        let view = self.view.clone();
        cx.update_window(self.window, |_, window, cx| {
            let handle = view.focus_handle(cx);
            window.focus(&handle, cx);
        })
        .unwrap();
        self.frame(cx);
    }

    fn keys(&self, keys: &str, cx: &mut TestAppContext) {
        VisualTestContext::from_window(self.window, cx).simulate_keystrokes(keys);
        self.frame(cx);
    }

    fn type_text(&self, text: &str, cx: &mut TestAppContext) {
        VisualTestContext::from_window(self.window, cx).simulate_input(text);
        self.frame(cx);
    }

    fn record<E: Clone + 'static>(&self, cx: &mut TestAppContext) -> Rc<RefCell<Vec<E>>>
    where
        V: EventEmitter<E>,
    {
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        cx.update(|cx| {
            cx.subscribe(&self.view, move |_, event: &E, _| sink.borrow_mut().push(event.clone()))
                .detach();
        });
        events
    }

    fn read<R>(&self, cx: &TestAppContext, f: impl FnOnce(&V) -> R) -> R {
        self.view.read_with(cx, |view, _| f(view))
    }
}

const BIB: &str = "@article{knuth1984,
  author  = {Knuth, Donald E.},
  title   = {Literate Programming},
  journal = {The Computer Journal},
  year    = {1984},
}

@inproceedings{vaswani2017,
  author    = {Vaswani, Ashish and Shazeer, Noam},
  title     = {Attention Is All You Need},
  booktitle = {NeurIPS},
  year      = {2017},
}

@book{lamport1994,
  author    = {Lamport, Leslie},
  title     = {LaTeX: A Document Preparation System},
  publisher = {Addison-Wesley},
  year      = {1994},
}
";

const MAIN: &str = "= Introduction\n\nAs @vaswani2017 shows, and again @vaswani2017[p. 3].\n\nSee also @smith2020.\n";

/// A paper folder with `refs.bib` and `main.typ`.
fn paper(bib: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::write(root.join("refs.bib"), bib).unwrap();
    fs::write(root.join("main.typ"), MAIN).unwrap();
    (dir, root)
}

fn sources_panel(root: &Path, cx: &mut TestAppContext) -> Harness<SourcesPanel> {
    let (root, bib) = (root.to_path_buf(), root.join("refs.bib"));
    let h = Harness::open(cx, move |window, cx| {
        SourcesPanel::new(root, bib, Engine::Typst, window, cx)
    });
    let main = h.read(cx, |p| p.root.join("main.typ"));
    h.view.update(cx, |p, cx| p.refresh(vec![main], cx));
    h.frame(cx);
    h
}

#[gpui::test]
fn sources_load_count_citations_and_find_missing_keys(cx: &mut TestAppContext) {
    let (_dir, root) = paper(BIB);
    let h = sources_panel(&root, cx);
    let (keys, cited, missing) = h.read(cx, |p| {
        (p.keys(), p.cited_count("vaswani2017"), p.missing_keys())
    });
    assert_eq!(keys, ["knuth1984", "vaswani2017", "lamport1994"]);
    assert_eq!(cited, 2);
    assert_eq!(missing, ["smith2020"]);
}

#[gpui::test]
fn typing_filters_the_sources_and_enter_cites_the_first_match(cx: &mut TestAppContext) {
    let (_dir, root) = paper(BIB);
    let h = sources_panel(&root, cx);
    let events = h.record::<SourcesEvent>(cx);
    h.focus(cx);
    h.type_text("lamport", cx);
    assert_eq!(h.read(cx, |p| p.visible_keys()), ["lamport1994"]);
    assert_eq!(h.read(cx, |p| p.selected_key().map(String::from)), Some("lamport1994".into()));
    h.keys("enter", cx);
    assert_eq!(
        *events.borrow(),
        vec![SourcesEvent::InsertCitation {
            text: "@lamport1994".into()
        }]
    );

    // Esc clears the filter first, then closes.
    h.keys("escape", cx);
    assert_eq!(h.read(cx, |p| p.visible_keys().len()), 3);
    h.keys("escape", cx);
    assert_eq!(events.borrow().last(), Some(&SourcesEvent::Close));
}

#[gpui::test]
fn arrows_move_through_missing_keys_and_sources(cx: &mut TestAppContext) {
    let (_dir, root) = paper(BIB);
    let h = sources_panel(&root, cx);
    h.focus(cx);
    // The missing key comes first, then the sources.
    h.keys("down", cx);
    assert_eq!(h.read(cx, |p| p.selected_key().map(String::from)), None);
    h.keys("down", cx);
    assert_eq!(h.read(cx, |p| p.selected_key().map(String::from)), Some("knuth1984".into()));
    h.keys("down down down", cx);
    assert_eq!(h.read(cx, |p| p.selected_key().map(String::from)), Some("lamport1994".into()));
    h.keys("up", cx);
    assert_eq!(h.read(cx, |p| p.selected_key().map(String::from)), Some("vaswani2017".into()));
}

#[gpui::test]
fn pasting_bibtex_adds_it_and_a_second_paste_points_at_the_duplicate(cx: &mut TestAppContext) {
    let (_dir, root) = paper("");
    let h = sources_panel(&root, cx);
    let events = h.record::<SourcesEvent>(cx);
    // With no sources yet, the panel's focus is the add field.
    h.focus(cx);
    let bibtex = "@article{hopper1952,\n  author = {Hopper, Grace},\n  title = {The Education of a Computer},\n  year = {1952},\n}\n";
    cx.write_to_clipboard(ClipboardItem::new_string(bibtex.to_string()));
    h.keys("cmd-v", cx);
    assert_eq!(h.read(cx, |p| p.keys()), ["hopper1952"]);
    assert_eq!(h.read(cx, |p| p.notice_text().map(String::from)), Some("Added hopper1952.".into()));
    assert!(fs::read_to_string(root.join("refs.bib")).unwrap().contains("hopper1952"));
    assert_eq!(*events.borrow(), vec![SourcesEvent::Changed]);

    // Now the focus is the filter; paste into the add field again.
    let add = h.read(cx, |p| p.add_input.clone());
    cx.update_window(h.window, |_, window, cx| {
        let handle = add.focus_handle(cx);
        window.focus(&handle, cx);
    })
    .unwrap();
    h.frame(cx);
    h.keys("cmd-v", cx);
    assert_eq!(h.read(cx, |p| p.keys()), ["hopper1952"]);
    assert_eq!(
        h.read(cx, |p| p.notice_text().map(String::from)),
        Some("Already in your sources as hopper1952.".into())
    );
    assert_eq!(events.borrow().len(), 1, "nothing new was written");
}

#[gpui::test]
fn removing_a_cited_source_asks_first_and_says_how_many_citations_break(cx: &mut TestAppContext) {
    let (_dir, root) = paper(BIB);
    let h = sources_panel(&root, cx);
    let events = h.record::<SourcesEvent>(cx);
    h.focus(cx);
    h.type_text("attention", cx);
    h.view.update(cx, |p, cx| p.remove("vaswani2017", false, cx));
    h.frame(cx);
    let warning = h.read(cx, |p| p.pending_removal()).unwrap();
    assert!(warning.contains("cites it twice"), "{warning}");
    assert!(h.read(cx, |p| p.keys()).contains(&"vaswani2017".to_string()));

    // Enter confirms.
    h.keys("enter", cx);
    assert_eq!(h.read(cx, |p| p.keys()), ["knuth1984", "lamport1994"]);
    assert!(!fs::read_to_string(root.join("refs.bib")).unwrap().contains("vaswani2017"));
    assert_eq!(*events.borrow(), vec![SourcesEvent::Changed]);
    // Its citations now show as missing.
    assert_eq!(h.read(cx, |p| p.missing_keys()), ["vaswani2017", "smith2020"]);

    // A source nothing cites goes at once.
    h.view.update(cx, |p, cx| p.remove("knuth1984", false, cx));
    h.frame(cx);
    assert_eq!(h.read(cx, |p| p.keys()), ["lamport1994"]);
    assert_eq!(h.read(cx, |p| p.pending_removal()), None);
}

#[gpui::test]
fn the_page_field_turns_into_a_locator(cx: &mut TestAppContext) {
    let (_dir, root) = paper(BIB);
    let h = sources_panel(&root, cx);
    let events = h.record::<SourcesEvent>(cx);
    h.focus(cx);
    h.type_text("knuth", cx);
    let page = h.read(cx, |p| p.page_input.clone());
    cx.update_window(h.window, |_, window, cx| {
        let handle = page.focus_handle(cx);
        window.focus(&handle, cx);
    })
    .unwrap();
    h.frame(cx);
    h.type_text("97", cx);
    h.keys("enter", cx);
    assert_eq!(
        *events.borrow(),
        vec![SourcesEvent::InsertCitation {
            text: "@knuth1984[p. 97]".into()
        }]
    );
}

/// A paper with two versions: the first by Oyl, the second by the agent.
fn paper_with_history() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let history = History::open(&root).unwrap();
    fs::write(root.join("main.typ"), "= Intro\n\nThe model is fast.\n").unwrap();
    history.snapshot(Author::System, None).unwrap();
    fs::write(
        root.join("main.typ"),
        "= Intro\n\nThe model is very fast and small.\n",
    )
    .unwrap();
    history
        .snapshot(Author::Agent, Some("Agent: tighten the intro"))
        .unwrap();
    (dir, root)
}

#[gpui::test]
fn selecting_a_version_shows_its_changes_and_restoring_brings_it_back(cx: &mut TestAppContext) {
    let (_dir, root) = paper_with_history();
    let path = root.clone();
    let h = Harness::open(cx, move |window, cx| HistoryPanel::new(path, window, cx));
    let events = h.record::<HistoryEvent>(cx);
    assert_eq!(h.read(cx, |p| p.versions().len()), 2);
    assert_eq!(h.read(cx, |p| p.preferred_width()), px(420.));

    h.focus(cx);
    h.keys("down", cx);
    assert_eq!(h.read(cx, |p| p.selected_number()), Some(2));
    assert!(h.read(cx, |p| p.preferred_width()) > px(900.));
    let rows = h.read(cx, |p| p.diff_rows().map(|r| r.to_vec())).unwrap();
    let added = rows.iter().find_map(|row| match row {
        crate::model::history::DiffRow::Line(line)
            if line.kind == oyl_history::LineKind::Add =>
        {
            Some(line.clone())
        }
        _ => None,
    });
    let added = added.expect("an added line");
    assert_eq!(added.text, "The model is very fast and small.");
    assert!(!added.changes.is_empty(), "the changed words are marked");

    // Version 1, then Enter twice: ask, then restore.
    h.keys("down", cx);
    assert_eq!(h.read(cx, |p| p.selected_number()), Some(1));
    h.keys("enter", cx);
    assert!(h.read(cx, |p| p.is_confirming_restore()));
    h.keys("enter", cx);
    assert_eq!(*events.borrow(), vec![HistoryEvent::Restored { version_number: 1 }]);
    assert_eq!(
        fs::read_to_string(root.join("main.typ")).unwrap(),
        "= Intro\n\nThe model is fast.\n"
    );
    let versions = h.read(cx, |p| p.versions().to_vec());
    assert_eq!(versions.len(), 3, "restoring adds a version");
    assert_eq!(versions[0].label.as_deref(), Some("Restored version 1"));
    assert_eq!(h.read(cx, |p| p.selected_number()), None);

    // Esc steps back: from a selection to the list, then closes.
    h.keys("down escape", cx);
    assert_eq!(h.read(cx, |p| p.selected_number()), None);
    h.keys("escape", cx);
    assert_eq!(events.borrow().last(), Some(&HistoryEvent::Close));
}

#[gpui::test]
fn compared_to_now_shows_what_changed_since(cx: &mut TestAppContext) {
    let (_dir, root) = paper_with_history();
    fs::write(root.join("notes.typ"), "New notes.\n").unwrap();
    let path = root.clone();
    let h = Harness::open(cx, move |window, cx| HistoryPanel::new(path, window, cx));
    h.view.update(cx, |p, cx| {
        p.select_number(2, cx);
        p.set_compare(super::history::Compare::ToNow, cx);
    });
    h.frame(cx);
    let rows = h.read(cx, |p| p.diff_rows().map(|r| r.to_vec())).unwrap();
    assert!(rows.iter().any(|row| matches!(
        row,
        crate::model::history::DiffRow::File { path, status: oyl_history::FileStatus::Added, .. }
            if path == "notes.typ"
    )));
}

#[gpui::test]
fn the_outline_filters_marks_the_current_section_and_jumps(cx: &mut TestAppContext) {
    let h = Harness::open(cx, OutlinePanel::new);
    let events = h.record::<OutlineEvent>(cx);
    let at = |line| SourceLocation::new("/p/main.typ", line, 1);
    let items = vec![
        OutlineItem { level: 1, title: "Introduction".into(), location: at(1) },
        OutlineItem { level: 2, title: "Motivation".into(), location: at(9) },
        OutlineItem { level: 1, title: "Background".into(), location: at(20) },
        OutlineItem { level: 1, title: "Conclusion".into(), location: at(40) },
    ];
    h.view.update(cx, |p, cx| {
        p.set_items(items, cx);
        p.set_current(Some(at(12)), cx);
    });
    h.frame(cx);
    assert_eq!(h.read(cx, |p| (p.current(), p.selected())), (Some(1), Some(1)));

    h.focus(cx);
    h.keys("down enter", cx);
    assert_eq!(*events.borrow(), vec![OutlineEvent::Jump(at(20))]);

    h.type_text("concl", cx);
    assert_eq!(h.read(cx, |p| p.shown()), vec![3]);
    h.keys("enter", cx);
    assert_eq!(events.borrow().last(), Some(&OutlineEvent::Jump(at(40))));
    h.keys("escape escape", cx);
    assert_eq!(events.borrow().last(), Some(&OutlineEvent::Close));
}

#[gpui::test]
fn problems_put_errors_first_jump_and_hand_errors_to_the_agent(cx: &mut TestAppContext) {
    let h = Harness::open(cx, ProblemsPanel::new);
    let events = h.record::<ProblemsEvent>(cx);
    h.view.update(cx, |p, cx| {
        p.set_diagnostics(
            vec![
                Diagnostic::warning("unused import").at("/p/main.typ", 2, 1),
                Diagnostic::error("unknown variable: foo").at("/p/main.typ", 14, 3),
            ],
            cx,
        )
    });
    h.frame(cx);
    assert!(h.read(cx, |p| p.diagnostics()[0].is_error()));
    h.focus(cx);
    h.keys("enter", cx);
    assert_eq!(
        *events.borrow(),
        vec![ProblemsEvent::Jump(SourceLocation::new("/p/main.typ", 14, 3))]
    );
    h.keys("down enter", cx);
    assert_eq!(
        events.borrow().last(),
        Some(&ProblemsEvent::Jump(SourceLocation::new("/p/main.typ", 2, 1)))
    );
    h.view.update(cx, |p, cx| p.fix(cx));
    let Some(ProblemsEvent::FixWithAgent { prompt }) = events.borrow().last().cloned() else {
        panic!("expected a fix prompt");
    };
    assert!(prompt.contains("/p/main.typ:14:3 — unknown variable: foo"), "{prompt}");
    assert!(!prompt.contains("unused import"));

    h.view.update(cx, |p, cx| p.set_diagnostics(Vec::new(), cx));
    h.frame(cx);
    assert_eq!(h.read(cx, |p| p.fix_prompt()), None);
}

fn write_png(path: &Path, width: u32, height: u32) {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([40, 90, 160, 255]));
    image.save(path).unwrap();
}

#[gpui::test]
fn figures_list_thumbnails_and_insert_a_block_with_caption(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::create_dir_all(root.join("figures")).unwrap();
    write_png(&root.join("figures/model.png"), 640, 480);
    write_png(&root.join("figures/results.png"), 300, 200);
    let path = root.clone();
    let h = Harness::open(cx, move |window, cx| {
        FiguresPanel::new(path, Engine::Typst, window, cx)
    });
    let events = h.record::<FiguresEvent>(cx);
    h.view.update(cx, |p, cx| {
        p.refresh(vec!["#image(\"figures/model.png\")".into()], cx)
    });
    h.frame(cx);
    let figures = h.read(cx, |p| p.figures().to_vec());
    assert_eq!(figures.len(), 2);
    assert!(figures[0].used);
    assert!(!figures[1].used);
    assert!(h.read(cx, |p| p.thumbs.values().all(|t| t.image.is_some()) && p.thumbs.len() == 2));

    h.focus(cx);
    h.keys("right right", cx);
    assert_eq!(
        h.read(cx, |p| p.selected_path().map(Path::to_path_buf)),
        Some(root.join("figures/results.png"))
    );
    h.keys("enter", cx);
    assert!(h.read(cx, |p| p.is_composing()));
    h.type_text("Accuracy by size", cx);
    h.keys("enter", cx);
    let Some(FiguresEvent::Insert { text }) = events.borrow().last().cloned() else {
        panic!("expected an insert");
    };
    assert_eq!(
        text,
        "#figure(image(\"figures/results.png\", width: 80%), caption: [Accuracy by size]) <fig:results>"
    );
    assert!(!h.read(cx, |p| p.is_composing()));
}

#[gpui::test]
fn dropping_an_image_imports_it(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let png = outside.path().join("Plot Final.png");
    write_png(&png, 50, 40);
    let path = root.clone();
    let h = Harness::open(cx, move |window, cx| {
        FiguresPanel::new(path, Engine::Latex, window, cx)
    });
    let events = h.record::<FiguresEvent>(cx);
    let mut visual = VisualTestContext::from_window(h.window, cx);
    let position = point(px(200.), px(300.));
    visual.simulate_event(FileDropEvent::Entered {
        position,
        paths: ExternalPaths(vec![png].into()),
    });
    visual.simulate_event(FileDropEvent::Submit { position });
    h.frame(cx);
    let figures = h.read(cx, |p| p.figures().to_vec());
    assert_eq!(figures.len(), 1);
    assert!(figures[0].rel_path.starts_with("figures/"), "{:?}", figures[0]);
    assert!(figures[0].path.is_file());
    assert_eq!(*events.borrow(), vec![FiguresEvent::Changed]);
}
