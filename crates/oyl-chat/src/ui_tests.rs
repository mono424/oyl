//! The pane in a headless GPUI window: the picker and threads popovers,
//! opened by real clicks and driven by keys.

use std::sync::Arc;

use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Modifiers, Point, TestAppContext,
    VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};
use oyl_model::agent::{
    Harness, HarnessId, HarnessInfo, ModelChoice, Readiness, TurnHandle, TurnRequest,
};
use oyl_model::Engine;

use super::{ChatPane, PopoverConfirm, PopoverDown};
use crate::context::PaperContext;

struct Fake;

impl Harness for Fake {
    fn info(&self) -> HarnessInfo {
        HarnessInfo {
            id: HarnessId::new("fake"),
            name: "Fake".into(),
            tagline: String::new(),
            readiness: Readiness::Ready,
            status: "Ready".into(),
            models: vec![
                ModelChoice {
                    id: "a".into(),
                    label: "Model A".into(),
                },
                ModelChoice {
                    id: "b".into(),
                    label: "Model B".into(),
                },
            ],
            default_model: Some("a".into()),
        }
    }

    fn start(&self, _: TurnRequest) -> anyhow::Result<TurnHandle> {
        anyhow::bail!("not in these tests")
    }
}

fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<ChatPane>, tempfile::TempDir) {
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
                    let mut pane = ChatPane::new(vec![Arc::new(Fake) as Arc<dyn Harness>], window, cx);
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
    assert_eq!(pane.read_with(cx, |pane, _| pane.open_popover()), Some("agent"));

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

