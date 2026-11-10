//! The elements in a headless GPUI window: frame requests (none at rest,
//! none under reduced motion), layout sizes, hover buttons and clicks,
//! and the discovery board's expanding rows.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use gpui_kit::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, IntoElement,
    Modifiers, ParentElement as _, Pixels, Point, Render, Styled as _, TestAppContext,
    VisualTestContext, Window, WindowBounds, WindowOptions, div, point, px, size,
};
use oyl_model::agent::{AgentEvent, Progress};

use crate::discovery::{DiscoveryBoard, DiscoveryState};
use crate::{
    ProgressBar, ProgressRing, PulseDot, StepTimeline, TaskCard, TaskRow, TaskSummary,
    set_reduce_motion,
};

type Build = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

/// A window whose content is rebuilt from `build` on every render.
struct Host {
    build: Build,
}

impl Render for Host {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child((self.build)(window, cx))
    }
}

fn open(
    width: f32,
    height: f32,
    cx: &mut TestAppContext,
    build: impl Fn(&mut Window, &mut App) -> AnyElement + 'static,
) -> (AnyWindowHandle, Entity<Host>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        oyl_ui::install(oyl_ui::Appearance::Light, cx);
    });
    let build: Build = Rc::new(build);
    let (window, host) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(width), px(height)),
                })),
                ..Default::default()
            },
            cx,
            move |_, cx| cx.new(|_| Host { build }),
        )
        .unwrap()
    });
    draw(window, cx);
    (window, host)
}

fn draw(window: AnyWindowHandle, cx: &mut TestAppContext) {
    for _ in 0..2 {
        cx.update_window(window, |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();
        cx.run_until_parked();
    }
}

/// How many next-frame callbacks the last frames asked for.
fn frames_requested(window: AnyWindowHandle, cx: &mut TestAppContext) -> usize {
    cx.update_window(window, |_, window, cx| window.simulate_next_frame(cx))
        .unwrap()
}

fn bounds(
    window: AnyWindowHandle,
    selector: &'static str,
    cx: &TestAppContext,
) -> Option<Bounds<Pixels>> {
    VisualTestContext::from_window(window, cx).debug_bounds(selector)
}

fn center(bounds: Bounds<Pixels>) -> Point<Pixels> {
    point(
        bounds.origin.x + bounds.size.width / 2.,
        bounds.origin.y + bounds.size.height / 2.,
    )
}

fn click(window: AnyWindowHandle, at: Point<Pixels>, cx: &TestAppContext) {
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.simulate_mouse_move(at, None, Modifiers::none());
    visual.simulate_click(at, Modifiers::none());
}

fn t0() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_900_000_000)
}

// -- Motion ----------------------------------------------------------------

#[gpui::test]
fn a_ring_at_rest_asks_for_no_frames_and_eases_to_a_new_value(cx: &mut TestAppContext) {
    let fraction = Rc::new(Cell::new(0.25f32));
    let value = fraction.clone();
    let (window, host) = open(100., 100., cx, move |_, _| {
        ProgressRing::determinate("ring", value.get())
            .size(px(40.))
            .into_any_element()
    });
    assert_eq!(frames_requested(window, cx), 0, "a settled ring is still");

    fraction.set(0.8);
    host.update(cx, |_, cx| cx.notify());
    draw(window, cx);
    assert!(frames_requested(window, cx) > 0, "the ring glides to 80%");

    cx.executor().advance_clock(Duration::from_secs(1));
    draw(window, cx);
    // The frame that lands on the value may ask for one more; then quiet.
    assert!(frames_requested(window, cx) <= 1);
    draw(window, cx);
    assert_eq!(
        frames_requested(window, cx),
        0,
        "and stops when it gets there"
    );
}

#[gpui::test]
fn indeterminate_indicators_move_until_motion_is_reduced(cx: &mut TestAppContext) {
    let (window, _) = open(300., 120., cx, |_, _| {
        div()
            .flex()
            .flex_col()
            .child(ProgressRing::new("spin"))
            .child(PulseDot::new("pulse"))
            .child(ProgressBar::new("sweep"))
            .into_any_element()
    });
    assert!(
        frames_requested(window, cx) > 0,
        "spinning, breathing, sweeping"
    );

    cx.update(|cx| set_reduce_motion(true, cx));
    draw(window, cx);
    frames_requested(window, cx);
    draw(window, cx);
    assert_eq!(
        frames_requested(window, cx),
        0,
        "reduced motion holds them still"
    );
    assert!(cx.update(|cx| crate::reduce_motion(cx)));
}

#[gpui::test]
fn still_things_ask_for_no_frames(cx: &mut TestAppContext) {
    let (window, _) = open(400., 200., cx, |_, _| {
        div()
            .flex()
            .flex_col()
            .child(PulseDot::new("dot").active(false))
            .child(ProgressBar::new("bar").fraction(Some(0.4)).phase("Round 2"))
            .child(StepTimeline::new("timeline", ["A", "B", "C"]).finished(true))
            .into_any_element()
    });
    assert_eq!(frames_requested(window, cx), 0);
}

#[gpui::test]
fn a_running_row_under_reduced_motion_ticks_without_frames(cx: &mut TestAppContext) {
    cx.update(|cx| set_reduce_motion(true, cx));
    let task = TaskSummary::new("Write the related work", SystemTime::now());
    let (window, _) = open(320., 52., cx, move |_, _| {
        TaskRow::new("row", &task).into_any_element()
    });
    assert_eq!(
        frames_requested(window, cx),
        0,
        "the clock runs on a timer, not frames"
    );
}

// -- Layout ----------------------------------------------------------------

#[gpui::test]
fn the_timeline_collapses_when_narrow(cx: &mut TestAppContext) {
    let progress = Progress {
        phase: "Round 7".into(),
        phases: [
            "Split",
            "Baseline",
            "SOTA search",
            "Rounds",
            "Held-out check",
        ]
        .map(String::from)
        .to_vec(),
        ..Default::default()
    };
    let wide_progress = progress.clone();
    let (wide, _) = open(600., 40., cx, move |_, _| {
        StepTimeline::from_progress("t", &wide_progress).into_any_element()
    });
    let rail = bounds(wide, "step-timeline-wide", cx).expect("wide at 600px");
    assert_eq!(rail.size.height, px(32.));
    assert!(bounds(wide, "step-timeline-compact", cx).is_none());

    let (narrow, _) = open(200., 40., cx, move |_, _| {
        StepTimeline::from_progress("t", &progress).into_any_element()
    });
    assert!(
        bounds(narrow, "step-timeline-compact", cx).is_some(),
        "compact at 200px"
    );
    assert!(bounds(narrow, "step-timeline-wide", cx).is_none());
}

#[gpui::test]
fn a_tray_row_is_52px_and_its_stop_button_stops(cx: &mut TestAppContext) {
    let opened = Rc::new(Cell::new(0));
    let stopped = Rc::new(Cell::new(0));
    let mut task = TaskSummary::new("Raise the classifier's accuracy", t0());
    task.apply_at(
        &AgentEvent::Progress(Progress {
            phase: "Round 3".into(),
            fraction: Some(0.3),
            ..Default::default()
        }),
        t0(),
    );
    let (o, s) = (opened.clone(), stopped.clone());
    let (window, _) = open(320., 200., cx, move |_, _| {
        let (o, s) = (o.clone(), s.clone());
        TaskRow::new("row", &task)
            .on_open(move |_, _, _| o.set(o.get() + 1))
            .on_stop(move |_, _, _| s.set(s.get() + 1))
            .into_any_element()
    });
    let row = bounds(window, "task-row", cx).expect("the row");
    assert_eq!(row.size.height, px(52.));
    assert_eq!(row.size.width, px(320.));
    let stop = bounds(window, "task-row-stop", cx).expect("the stop button");
    assert_eq!(stop.size, size(px(24.), px(24.)));
    assert_eq!(
        stop.origin.x + stop.size.width,
        px(310.),
        "inside the 10px padding"
    );
    assert_eq!(stop.origin.y, px(14.), "vertically centered");

    click(window, center(stop), cx);
    assert_eq!(
        (opened.get(), stopped.get()),
        (0, 1),
        "stop does not open the thread"
    );
    click(window, point(px(60.), px(26.)), cx);
    assert_eq!((opened.get(), stopped.get()), (1, 1));
}

#[gpui::test]
fn the_card_stops_once_and_then_says_so(cx: &mut TestAppContext) {
    let stops = Rc::new(Cell::new(0));
    let task = Rc::new(RefCell::new(TaskSummary::new("Discover", t0())));
    task.borrow_mut().apply_at(
        &AgentEvent::Progress(Progress {
            phase: "Round 2".into(),
            phases: vec!["Baseline".into(), "Rounds".into(), "Report".into()],
            step: Some(2),
            total: Some(10),
            fraction: Some(0.2),
            metrics: vec![("accuracy".into(), "0.931".into())],
            state_dir: None,
        }),
        t0(),
    );
    let (shared, counter) = (task.clone(), stops.clone());
    let (window, host) = open(560., 400., cx, move |_, _| {
        let (task, counter) = (shared.clone(), counter.clone());
        let snapshot = task.borrow().clone();
        TaskCard::new("card", &snapshot)
            .on_stop(move |_, _, _| {
                counter.set(counter.get() + 1);
                task.borrow_mut().request_stop();
            })
            .into_any_element()
    });
    let card = bounds(window, "task-card", cx).expect("the card");
    assert_eq!(card.size.width, px(560.));
    let stop = bounds(window, "task-card-stop", cx).expect("Stop while running");
    assert_eq!(stop.size.height, px(26.));
    assert_eq!(
        stop.origin.x + stop.size.width,
        px(560. - 16. - 1.),
        "inside the padding and edge"
    );

    click(window, center(stop), cx);
    assert_eq!(stops.get(), 1);
    host.update(cx, |_, cx| cx.notify());
    draw(window, cx);
    let stop = bounds(window, "task-card-stop", cx).expect("still shown, as Stopping…");
    click(window, center(stop), cx);
    assert_eq!(stops.get(), 1, "a second click while stopping does nothing");

    task.borrow_mut().apply_at(
        &AgentEvent::Failed {
            message: "cancelled".into(),
        },
        t0(),
    );
    host.update(cx, |_, cx| cx.notify());
    draw(window, cx);
    assert!(
        bounds(window, "task-card-stop", cx).is_none(),
        "no Stop once ended"
    );
}

// -- The discovery board -----------------------------------------------------

const PARETO: &str = "\
| id | parent | commit | idea | aspect | accuracy ↑ | latency_ms ↓ |
|---|---|---|---|---|---|---|
| B | — | a1b2c3d | baseline | — | 0.912 | 34.1 |
| P3 | P1 | 9f8e7d6 | TF-IDF bigrams | features | 0.927 | 36.0 |
";

const EXPLORED: &str = "\
| id | parent | idea | accuracy | status | observations |
|---|---|---|---|---|---|
| B | — | baseline | 0.912 | KEPT | tokenizer dominates |
| P1 | B | lowercase | 0.915 | DOMINATED | small gain |
| P2 | B | bigger C | 0.905 | FAILED | worse |
| P3 | P1 | TF-IDF bigrams | 0.927 | KEPT | negations fixed |
";

const IDEAS: &str = "\
- Char n-grams (aspect: features, parent: P3) — typos
- Mixup on embeddings [data]
";

const SPLIT: &str = "- Goal: dev accuracy ≥ 0.93\n- Round limit: 40\n";

fn board_state() -> DiscoveryState {
    DiscoveryState::from_texts(Some(PARETO), Some(EXPLORED), Some(IDEAS), Some(SPLIT))
}

fn open_board(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<DiscoveryBoard>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        oyl_ui::install(oyl_ui::Appearance::Dark, cx);
    });
    let (window, board) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(640.), px(900.)),
                })),
                ..Default::default()
            },
            cx,
            |_, cx| {
                cx.new(|cx| {
                    let mut board = DiscoveryBoard::new(cx);
                    board.set_state(board_state(), cx);
                    board
                })
            },
        )
        .unwrap()
    });
    draw(window, cx);
    (window, board)
}

#[gpui::test]
fn the_board_reads_its_header_from_the_files(cx: &mut TestAppContext) {
    let (_, board) = open_board(cx);
    board.read_with(cx, |board, _| {
        assert!(board.is_loaded());
        assert_eq!(board.goal(), Some("dev accuracy ≥ 0.93"));
        assert_eq!(board.round_text().as_deref(), Some("Round 3 / 40"));
        assert_eq!(
            board.count_parts(),
            vec![(3, "tried"), (1, "kept"), (1, "failed"), (1, "dominated")]
        );
        assert_eq!(board.state().pending().len(), 2);
    });
    board.update(cx, |board, cx| {
        board.set_progress(
            &Progress {
                phase: "Round 4".into(),
                step: Some(4),
                total: Some(50),
                ..Default::default()
            },
            cx,
        )
    });
    board.read_with(cx, |board, _| {
        assert_eq!(
            board.round_text().as_deref(),
            Some("Round 4 / 50"),
            "the progress wins"
        );
        assert_eq!(board.dir(), None, "no state_dir, nothing watched");
    });
}

#[gpui::test]
fn ideas_expand_and_pending_ideas_unfold(cx: &mut TestAppContext) {
    let (window, board) = open_board(cx);
    let row = bounds(window, "explored-idea-1", cx).expect("the P1 row");
    assert_eq!(row.size.height, px(30.));
    let list = bounds(window, "explored-list", cx).expect("the list");
    assert!(list.size.height >= px(120.));

    click(window, center(row), cx);
    assert!(board.read_with(cx, |b, _| b.is_expanded("P1")));
    draw(window, cx);
    let below = bounds(window, "explored-idea-2", cx).unwrap();
    assert!(
        below.origin.y > row.origin.y + px(30. + 20.),
        "the observations open below P1"
    );
    click(window, center(row), cx);
    assert!(!board.read_with(cx, |b, _| b.is_expanded("P1")));

    assert!(
        !board.read_with(cx, |b, _| b.pending_shown()),
        "pending starts collapsed"
    );
    draw(window, cx);
    let toggle = bounds(window, "pending-toggle", cx).expect("the pending toggle");
    click(window, center(toggle), cx);
    assert!(board.read_with(cx, |b, _| b.pending_shown()));
}

#[gpui::test]
fn an_empty_board_waits(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        oyl_ui::install(oyl_ui::Appearance::Light, cx);
    });
    let board = cx.new(DiscoveryBoard::new);
    board.read_with(cx, |board, _| {
        assert!(!board.is_loaded());
        assert_eq!(board.round_text(), None);
        assert_eq!(board.goal(), None);
    });
    board.update(cx, |board, cx| {
        board.set_goal(Some("  raise accuracy  ".into()), cx);
        board.set_state(DiscoveryState::default(), cx);
    });
    board.read_with(cx, |board, _| {
        assert!(board.is_loaded());
        assert_eq!(board.goal(), Some("raise accuracy"));
    });
}
