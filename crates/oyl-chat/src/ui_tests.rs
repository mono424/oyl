//! The pane in a headless GPUI window: the picker and threads popovers,
//! opened by real clicks and driven by keys.

use std::sync::Arc;

use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Modifiers, Point, TestAppContext,
    VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};
use oyl_model::Engine;
use oyl_model::agent::{
    Harness, HarnessId, HarnessInfo, ModelChoice, Readiness, TurnHandle, TurnRequest,
};

use super::{ChatPane, PopoverConfirm, PopoverDown};
use crate::context::PaperContext;

/// A harness that lists `models` and never runs.
struct Fake {
    id: &'static str,
    name: &'static str,
    models: Vec<(&'static str, &'static str)>,
    efforts: Vec<&'static str>,
}

impl Fake {
    fn two_models() -> Self {
        Self {
            id: "fake",
            name: "Fake",
            models: vec![("a", "Model A"), ("b", "Model B")],
            efforts: Vec::new(),
        }
    }

    fn with_efforts() -> Self {
        Self {
            efforts: vec!["low", "medium", "high", "xhigh", "max"],
            ..Self::two_models()
        }
    }

    fn six_models() -> Self {
        Self {
            id: "many",
            name: "Many",
            models: vec![
                ("alpha", "Alpha"),
                ("beta", "Beta (fastest)"),
                ("gamma", "Gamma"),
                ("delta", "Delta"),
                ("epsilon", "Epsilon"),
                ("zeta", "Zeta"),
            ],
            efforts: Vec::new(),
        }
    }
}

impl Harness for Fake {
    fn info(&self) -> HarnessInfo {
        HarnessInfo {
            id: HarnessId::new(self.id),
            name: self.name.into(),
            tagline: String::new(),
            readiness: Readiness::Ready,
            status: "Ready".into(),
            models: self
                .models
                .iter()
                .map(|(id, label)| ModelChoice {
                    id: (*id).into(),
                    label: (*label).into(),
                })
                .collect(),
            default_model: self.models.first().map(|(id, _)| (*id).into()),
            efforts: self.efforts.iter().map(|effort| (*effort).into()).collect(),
            default_effort: (!self.efforts.is_empty()).then(|| "medium".into()),
            capabilities: Vec::new(),
        }
    }

    fn start(&self, _: TurnRequest) -> anyhow::Result<TurnHandle> {
        anyhow::bail!("not in these tests")
    }
}

fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<ChatPane>, tempfile::TempDir) {
    open_with(vec![Fake::two_models()], cx)
}

fn open_with(
    harnesses: Vec<Fake>,
    cx: &mut TestAppContext,
) -> (AnyWindowHandle, Entity<ChatPane>, tempfile::TempDir) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        oyl_ui::install(oyl_ui::Appearance::Light, cx);
    });
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let (window, pane) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(480.), px(700.)),
                })),
                ..Default::default()
            },
            cx,
            |window, cx| {
                cx.new(|cx| {
                    let harnesses = harnesses
                        .into_iter()
                        .map(|fake| Arc::new(fake) as Arc<dyn Harness>)
                        .collect();
                    let mut pane = ChatPane::new(harnesses, window, cx);
                    pane.set_paper(
                        Some(PaperContext {
                            main_file: root.join("main.typ"),
                            root: root.clone(),
                            engine: Engine::Typst,
                        }),
                        cx,
                    );
                    pane
                })
            },
        )
        .unwrap()
    });
    draw(window, cx);
    (window, pane, dir)
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

fn center(bounds: Bounds<gpui_kit::Pixels>) -> Point<gpui_kit::Pixels> {
    point(
        bounds.origin.x + bounds.size.width / 2.,
        bounds.origin.y + bounds.size.height / 2.,
    )
}

#[gpui::test]
fn a_click_on_the_chip_opens_the_picker_and_keys_pick_a_model(cx: &mut TestAppContext) {
    let (window, pane, _dir) = open(cx);
    let chip = pane.read_with(cx, |pane, _| pane.picker_bounds());
    assert!(chip.size.width > px(0.), "the chip was laid out");
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.simulate_click(center(chip), Modifiers::none());
    draw(window, cx);
    assert_eq!(
        pane.read_with(cx, |pane, _| pane.open_popover()),
        Some("agent")
    );

    // ↓ from the default model (A) to B, then Enter.
    cx.update_window(window, |_, window, cx| {
        window.dispatch_action(Box::new(PopoverDown), cx);
        window.dispatch_action(Box::new(PopoverConfirm), cx);
    })
    .unwrap();
    draw(window, cx);
    let (open, selection) = pane.read_with(cx, |pane, _| (pane.open_popover(), pane.selection()));
    assert_eq!(open, None);
    assert_eq!(selection, Some((HarnessId::new("fake"), Some("b".into()))));
}

#[gpui::test]
fn a_second_click_on_the_chip_closes_the_picker(cx: &mut TestAppContext) {
    let (window, pane, _dir) = open(cx);
    let chip = pane.read_with(cx, |pane, _| pane.picker_bounds());
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.simulate_click(center(chip), Modifiers::none());
    draw(window, cx);
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.simulate_click(center(chip), Modifiers::none());
    draw(window, cx);
    assert_eq!(pane.read_with(cx, |pane, _| pane.open_popover()), None);
}

#[gpui::test]
fn a_click_outside_closes_the_picker(cx: &mut TestAppContext) {
    let (window, pane, _dir) = open(cx);
    let chip = pane.read_with(cx, |pane, _| pane.picker_bounds());
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.simulate_click(center(chip), Modifiers::none());
    draw(window, cx);
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.simulate_click(point(px(240.), px(40.)), Modifiers::none());
    draw(window, cx);
    assert_eq!(pane.read_with(cx, |pane, _| pane.open_popover()), None);
}

#[gpui::test]
fn the_picker_lists_every_model_and_its_search_picks_across_harnesses(cx: &mut TestAppContext) {
    let (window, pane, _dir) = open_with(vec![Fake::two_models(), Fake::six_models()], cx);
    let chip = pane.read_with(cx, |pane, _| pane.picker_bounds());
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.simulate_click(center(chip), Modifiers::none());
    draw(window, cx);
    let (models, search) = pane.read_with(cx, |pane, _| {
        (pane.picker_models(), pane.picker_has_search())
    });
    let listed: Vec<&str> = models.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(
        listed,
        vec![
            "Model A", "Model B", "Alpha", "Beta", "Gamma", "Delta", "Epsilon", "Zeta"
        ],
        "every model of every harness, the hint out of the name"
    );
    assert!(search, "eight models get a search field");
    assert!(models[0].selected, "the model in use");

    // Typing in the search field filters; Enter there picks.
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.simulate_input("zet");
    draw(window, cx);
    let names: Vec<String> = pane.read_with(cx, |pane, _| {
        pane.picker_models().into_iter().map(|m| m.name).collect()
    });
    assert_eq!(names, vec!["Zeta".to_string()]);
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.simulate_keystrokes("enter");
    draw(window, cx);
    let (open, selection) = pane.read_with(cx, |pane, _| (pane.open_popover(), pane.selection()));
    assert_eq!(open, None);
    assert_eq!(
        selection,
        Some((HarnessId::new("many"), Some("zeta".into()))),
        "one pick switches the harness and the model"
    );

    // ↓ in the search field moves the highlight across harnesses; Esc
    // there closes the picker.
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.simulate_click(center(chip), Modifiers::none());
    draw(window, cx);
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.simulate_keystrokes("down down enter");
    draw(window, cx);
    let selection = pane.read_with(cx, |pane, _| pane.selection());
    assert_eq!(
        selection,
        Some((HarnessId::new("fake"), Some("b".into()))),
        "from Zeta, ↓ wraps to Model A, then Model B"
    );
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.simulate_click(center(chip), Modifiers::none());
    draw(window, cx);
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.simulate_keystrokes("escape");
    draw(window, cx);
    assert_eq!(pane.read_with(cx, |pane, _| pane.open_popover()), None);
}

#[gpui::test]
fn the_picker_offers_the_harness_efforts_as_pills(cx: &mut TestAppContext) {
    let (window, pane, _dir) = open_with(vec![Fake::with_efforts()], cx);
    let (effort, offered) = pane.read_with(cx, |pane, _| (pane.effort(), pane.efforts_offered()));
    assert_eq!(effort.as_deref(), Some("medium"), "the harness default");
    assert_eq!(offered, vec!["low", "medium", "high", "xhigh", "max"]);
    let chip = pane.read_with(cx, |pane, _| pane.picker_bounds());
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.simulate_click(center(chip), Modifiers::none());
    draw(window, cx);
    let high = pane
        .read_with(cx, |pane, _| pane.effort_pill_bounds("high"))
        .expect("the pill was laid out");
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.simulate_click(center(high), Modifiers::none());
    draw(window, cx);
    let (effort, open) = pane.read_with(cx, |pane, _| (pane.effort(), pane.open_popover()));
    assert_eq!(effort.as_deref(), Some("high"));
    assert_eq!(open, Some("agent"), "the picker stays open for a model");
}

#[gpui::test]
fn a_harness_without_efforts_shows_no_pills(cx: &mut TestAppContext) {
    let (window, pane, _dir) = open(cx);
    let chip = pane.read_with(cx, |pane, _| pane.picker_bounds());
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.simulate_click(center(chip), Modifiers::none());
    draw(window, cx);
    let (effort, pill) = pane.read_with(cx, |pane, _| {
        (pane.effort(), pane.effort_pill_bounds("high"))
    });
    assert_eq!((effort, pill), (None, None));
}

#[gpui::test]
fn changes_from_oyl_cloud_wait_in_a_card_of_their_own(cx: &mut TestAppContext) {
    use super::{
        CLOUD_REVIEW, ChatEvent, ReviewChoice, ReviewConflict, ReviewFile, ReviewLine,
        ReviewLineKind, ReviewView,
    };
    use std::cell::RefCell;
    use std::rc::Rc;

    let (window, pane, _dir) = open(cx);
    let events: Rc<RefCell<Vec<ChatEvent>>> = Rc::default();
    let seen = events.clone();
    cx.update(|cx| {
        cx.subscribe(&pane, move |_, event: &ChatEvent, _| {
            seen.borrow_mut().push(event.clone())
        })
        .detach()
    });
    let review = ReviewView {
        files: vec![ReviewFile {
            path: "main.typ".into(),
            added: 1,
            removed: 1,
            binary: false,
            lines: vec![ReviewLine {
                kind: ReviewLineKind::Added,
                text: "Method, from the cloud.".into(),
            }],
        }],
        conflicts: vec![ReviewConflict {
            id: "0123456789abcdef".into(),
            file: "main.typ".into(),
            place: "line 5".into(),
            lines: true,
            main: "Method, mine.\n".into(),
            agent: "Method, from the cloud.\n".into(),
        }],
        note: None,
    };
    pane.update(cx, |pane, cx| {
        pane.set_cloud_review(Some(review.clone()), Some("From MacBook Pro".into()), cx)
    });
    draw(window, cx);
    assert_eq!(
        pane.read_with(cx, |pane, _| pane.cloud_review().cloned()),
        Some(review)
    );
    // It belongs to no thread: the thread on screen has no review of its own.
    let thread = pane.read_with(cx, |pane, _| pane.thread().id.clone());
    assert!(pane.read_with(cx, |pane, _| pane.branch_view(&thread).is_none()));

    pane.update(cx, |pane, cx| {
        pane.set_review_choice(CLOUD_REVIEW, "0123456789abcdef", ReviewChoice::Yours, cx);
        pane.apply_review(CLOUD_REVIEW, cx);
    });
    draw(window, cx);
    assert!(events.borrow().contains(&ChatEvent::ApplyReview {
        thread: CLOUD_REVIEW.to_string(),
        choices: vec![("0123456789abcdef".to_string(), ReviewChoice::Yours)],
    }));
    pane.update(cx, |pane, cx| pane.set_cloud_review(None, None, cx));
    draw(window, cx);
    assert!(pane.read_with(cx, |pane, _| pane.cloud_review().is_none()));
}
