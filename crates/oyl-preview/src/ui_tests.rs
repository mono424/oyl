//! The pane in a headless GPUI window: lazy rasterizing, painting, texture
//! release, recompiles, input and zoom, through the real element, hit
//! testing and event dispatch of the test platform (scale factor 2).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Focusable as _, Modifiers, Point,
    RenderImage, ScrollDelta, ScrollEvent, ScrollSource, TestAppContext, TouchPhase,
    VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};
use oyl_model::SourceLocation;
use oyl_model::typeset::{PageImage, PagePosition, RenderedDocument};

use super::{PreviewEvent, PreviewPane, PreviewStatus, ZoomMode, ZoomToFitWidth};
use crate::layout::{A4_PT, MARGIN_TOP, MARGIN_X, fit_width_zoom, max_zoom_without_scroll};

const PANE_W: f32 = 800.;
const PANE_H: f32 = 1000.;

/// Plain A4 pages; a page's content is its revision number.
struct Paper {
    revisions: Vec<u64>,
    renders: Arc<Mutex<Vec<(usize, u64)>>>,
}

impl RenderedDocument for Paper {
    fn page_count(&self) -> usize {
        self.revisions.len()
    }

    fn page_size_pt(&self, _index: usize) -> (f32, f32) {
        (A4_PT.width, A4_PT.height)
    }

    fn render_page(&self, index: usize, pixels_per_pt: f32) -> anyhow::Result<PageImage> {
        let revision = self.revisions[index];
        self.renders.lock().unwrap().push((index, revision));
        let width = (A4_PT.width * pixels_per_pt).ceil() as u32;
        let height = (A4_PT.height * pixels_per_pt).ceil() as u32;
        Ok(PageImage {
            width,
            height,
            rgba: vec![255; width as usize * height as usize * 4],
        })
    }

    fn pdf(&self) -> anyhow::Result<Arc<Vec<u8>>> {
        Ok(Arc::new(Vec::new()))
    }

    fn page_fingerprint(&self, index: usize) -> u64 {
        (index as u64) << 32 | self.revisions[index]
    }

    fn source_at(&self, position: PagePosition) -> Option<SourceLocation> {
        Some(SourceLocation::new(
            "/paper/main.typ",
            position.y_pt.round() as u32,
            position.x_pt.round() as u32,
        ))
    }
}

struct Harness {
    window: AnyWindowHandle,
    pane: Entity<PreviewPane>,
    renders: Arc<Mutex<Vec<(usize, u64)>>>,
    events: Rc<RefCell<Vec<PreviewEvent>>>,
}

impl Harness {
    fn open(cx: &mut TestAppContext) -> Self {
        cx.update(|cx| {
            gpui_kit::init(cx);
            oyl_ui::install(oyl_ui::Appearance::Light, cx);
        });
        let (window, pane) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Point::default(),
                        size: size(px(PANE_W), px(PANE_H)),
                    })),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| PreviewPane::new(window, cx)),
            )
            .unwrap()
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        cx.update(|cx| {
            cx.subscribe(&pane, move |_, event: &PreviewEvent, _| {
                sink.borrow_mut().push(event.clone())
            })
            .detach();
        });
        let harness = Self {
            window,
            pane,
            renders: Arc::new(Mutex::new(Vec::new())),
            events,
        };
        harness.frame(cx);
        harness
    }

    fn show(&self, revisions: Vec<u64>, cx: &mut TestAppContext) {
        let document: Arc<dyn RenderedDocument> = Arc::new(Paper {
            revisions,
            renders: self.renders.clone(),
        });
        self.pane
            .update(cx, |pane, cx| pane.set_document(Some(document), cx));
    }

    /// Draws a frame and lets every background job and its follow-up run.
    fn frame(&self, cx: &mut TestAppContext) {
        for _ in 0..2 {
            cx.update_window(self.window, |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
            .unwrap();
            cx.run_until_parked();
        }
    }

    fn take_renders(&self) -> Vec<(usize, u64)> {
        let mut renders = std::mem::take(&mut *self.renders.lock().unwrap());
        renders.sort();
        renders
    }

    fn image(&self, page: usize, cx: &TestAppContext) -> Option<Arc<RenderImage>> {
        self.pane
            .read_with(cx, |pane, _| pane.cache.get(page).map(|b| b.image.clone()))
    }

    fn painted(&self, image: &RenderImage, cx: &mut TestAppContext) -> bool {
        cx.update_window(self.window, |_, window, _| {
            window.has_image_atlas_entry(image)
        })
        .unwrap()
    }

    fn visual(&self, cx: &TestAppContext) -> VisualTestContext {
        VisualTestContext::from_window(self.window, cx)
    }
}

fn fit_zoom() -> f32 {
    fit_width_zoom(&[A4_PT], PANE_W)
}

/// The largest zoom at which a page still fits the test pane's width.
fn zoom_cap() -> f32 {
    max_zoom_without_scroll(&[A4_PT], PANE_W)
}

#[gpui::test]
fn the_empty_pane_waits_calmly(cx: &mut TestAppContext) {
    let h = Harness::open(cx);
    let (pages, used) = h
        .pane
        .read_with(cx, |pane, _| (pane.page_count(), pane.memory_used()));
    assert_eq!(pages, 0);
    assert_eq!(used, 0);
    assert!(h.take_renders().is_empty());
}

#[gpui::test]
fn visible_pages_and_a_page_of_margin_are_rasterized_and_painted(cx: &mut TestAppContext) {
    let h = Harness::open(cx);
    h.show(vec![0; 40], cx);
    h.frame(cx);

    // At fit width one A4 page is taller than the pane: page 1 is on
    // screen, page 2 is the margin, and nothing else is touched.
    assert_eq!(h.take_renders(), vec![(0, 0), (1, 0)]);
    let (zoom, mode, scale) = h.pane.read_with(cx, |pane, _| {
        (
            pane.zoom(),
            pane.zoom_mode(),
            pane.cache.get(0).map(|b| b.pixels_per_pt),
        )
    });
    assert!((zoom - fit_zoom()).abs() < 1e-4);
    assert!((zoom * A4_PT.width - (PANE_W - 2. * MARGIN_X)).abs() < 0.01);
    assert_eq!(mode, ZoomMode::FitWidth);
    // Rasterized at zoom × the window's scale factor.
    assert!((scale.unwrap() - zoom * 2.).abs() < 1e-4);

    let first = h.image(0, cx).unwrap();
    let margin = h.image(1, cx).unwrap();
    assert!(h.painted(&first, cx));
    assert!(!h.painted(&margin, cx));
}

#[gpui::test]
fn a_recompile_rerasterizes_only_the_changed_page_without_flicker(cx: &mut TestAppContext) {
    let h = Harness::open(cx);
    h.show(vec![0; 40], cx);
    h.frame(cx);
    h.take_renders();
    let scroll_before = h.pane.read_with(cx, |pane, _| pane.scroll);
    let old = h.image(0, cx).unwrap();
    let unchanged = h.image(1, cx).unwrap();

    let mut revisions = vec![0; 40];
    revisions[0] = 1;
    h.show(revisions, cx);
    // Until the new bitmap is ready the old one stays on screen.
    assert!(Arc::ptr_eq(&h.image(0, cx).unwrap(), &old));

    h.frame(cx);
    assert_eq!(h.take_renders(), vec![(0, 1)]);
    let new = h.image(0, cx).unwrap();
    assert!(!Arc::ptr_eq(&new, &old));
    assert!(Arc::ptr_eq(&h.image(1, cx).unwrap(), &unchanged));
    assert!(h.painted(&new, cx));
    // The replaced bitmap's GPU texture is released.
    assert!(!h.painted(&old, cx));
    assert_eq!(h.pane.read_with(cx, |pane, _| pane.scroll), scroll_before);
}

#[gpui::test]
fn results_of_a_superseded_compile_are_dropped(cx: &mut TestAppContext) {
    let h = Harness::open(cx);
    // Two compiles in a row before any rasterization finishes.
    h.show(vec![0; 10], cx);
    let mut revisions = vec![0; 10];
    revisions[0] = 7;
    h.show(revisions, cx);
    h.frame(cx);

    // The first page's first job is superseded: it runs (it cannot be
    // stopped) but its result is ignored, and the page is rasterized once
    // more as it is now. The unchanged second page is rasterized once.
    assert_eq!(h.take_renders(), vec![(0, 0), (0, 7), (1, 0)]);
    let fingerprint = h
        .pane
        .read_with(cx, |pane, _| pane.cache.get(0).map(|b| b.fingerprint));
    assert_eq!(fingerprint, Some(7));
}

#[gpui::test]
fn a_failed_compile_keeps_the_last_good_pages(cx: &mut TestAppContext) {
    let h = Harness::open(cx);
    h.show(vec![0; 3], cx);
    h.frame(cx);
    h.take_renders();
    h.pane.update(cx, |pane, cx| {
        pane.set_document(None, cx);
        pane.set_status(PreviewStatus::Failed { errors: 3 }, cx);
    });
    h.frame(cx);
    let (pages, has_document, status) = h.pane.read_with(cx, |pane, _| {
        (
            pane.page_count(),
            pane.document().is_some(),
            pane.status().clone(),
        )
    });
    assert_eq!(pages, 3);
    assert!(has_document);
    assert_eq!(status, PreviewStatus::Failed { errors: 3 });
    assert!(h.take_renders().is_empty());
    assert!(h.painted(&h.image(0, cx).unwrap(), cx));
}

#[gpui::test]
fn fewer_pages_release_the_bitmaps_of_the_pages_that_are_gone(cx: &mut TestAppContext) {
    let h = Harness::open(cx);
    // At 25% all four pages are on screen.
    h.show(vec![0; 4], cx);
    h.pane.update(cx, |pane, cx| pane.set_zoom(0.25, cx));
    h.frame(cx);
    let pages: Vec<_> = (0..4).filter_map(|page| h.image(page, cx)).collect();
    assert_eq!(pages.len(), 4);
    assert!(pages.iter().all(|image| h.painted(image, cx)));
    assert_eq!(h.take_renders().len(), 4);

    h.show(vec![0; 2], cx);
    h.frame(cx);
    assert_eq!(h.pane.read_with(cx, |pane, _| pane.cache.len()), 2);
    assert!(pages[..2].iter().all(|image| h.painted(image, cx)));
    assert!(pages[2..].iter().all(|image| !h.painted(image, cx)));
    // The pages that stayed keep their bitmaps.
    assert!(h.take_renders().is_empty());
}

#[gpui::test]
fn clicking_a_page_reports_the_source_under_the_pointer(cx: &mut TestAppContext) {
    let h = Harness::open(cx);
    h.show(vec![0; 40], cx);
    h.frame(cx);
    let zoom = fit_zoom();
    // 100 pt right and 200 pt down from page 1's top-left corner.
    let x = (PANE_W - A4_PT.width * zoom) / 2. + 100. * zoom;
    let y = MARGIN_TOP + 200. * zoom;
    h.visual(cx)
        .simulate_click(point(px(x), px(y)), Modifiers::default());
    assert_eq!(
        *h.events.borrow(),
        vec![PreviewEvent::JumpToSource(SourceLocation::new(
            "/paper/main.typ",
            200,
            100
        ))]
    );

    // A click in the margin beside the page maps to nothing.
    h.events.borrow_mut().clear();
    h.visual(cx)
        .simulate_click(point(px(5.), px(y)), Modifiers::default());
    assert!(h.events.borrow().is_empty());
}

#[gpui::test]
fn command_scroll_zooms_around_the_pointer_and_reports_once_settled(cx: &mut TestAppContext) {
    let h = Harness::open(cx);
    h.show(vec![0; 40], cx);
    h.frame(cx);
    // Scroll into the middle first: at the very top a zoom-out has no room
    // to keep the point under the pointer.
    h.visual(cx).simulate_event(ScrollEvent {
        position: point(px(400.), px(500.)),
        delta: ScrollDelta::Pixels(point(px(0.), px(-6_000.))),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
        source: ScrollSource::Pointer,
    });
    h.frame(cx);
    let pointer = point(300., 400.);
    let before = h
        .pane
        .read_with(cx, |pane, _| pane.layout.hit_test(pane.scroll, pointer))
        .unwrap();

    h.visual(cx).simulate_event(ScrollEvent {
        position: point(px(pointer.x), px(pointer.y)),
        delta: ScrollDelta::Pixels(point(px(0.), px(-30.))),
        modifiers: Modifiers {
            platform: true,
            ..Default::default()
        },
        touch_phase: TouchPhase::Moved,
        source: ScrollSource::Pointer,
    });
    let (zoom, mode, after) = h.pane.read_with(cx, |pane, _| {
        (
            pane.zoom(),
            pane.zoom_mode(),
            pane.layout.hit_test(pane.scroll, pointer),
        )
    });
    let after = after.unwrap();
    assert!(zoom < fit_zoom() * 0.85, "{zoom}");
    assert_eq!(mode, ZoomMode::Custom(zoom));
    // Zoomed out, the narrower pages are centered again, so only the
    // vertical position under the pointer is kept.
    assert_eq!(after.page, before.page);
    assert!(
        (after.pt.y - before.pt.y).abs() < 0.01,
        "{after:?} vs {before:?}"
    );
    assert!(h.events.borrow().is_empty());

    // The bitmaps were made for the old zoom and are shown scaled until the
    // zoom has been quiet for a moment; then the change is reported and the
    // visible pages are rasterized sharp.
    h.take_renders();
    cx.executor().advance_clock(Duration::from_millis(200));
    h.frame(cx);
    assert_eq!(
        *h.events.borrow(),
        vec![PreviewEvent::ZoomChanged(ZoomMode::Custom(zoom))]
    );
    let renders = h.take_renders();
    assert!(renders.contains(&(after.page, 0)), "{renders:?}");
    let scale = h
        .pane
        .read_with(cx, |pane, _| {
            pane.cache.get(after.page).map(|b| b.pixels_per_pt)
        })
        .unwrap();
    assert!((scale - zoom * 2.).abs() < 1e-3);
}

#[gpui::test]
fn plain_scrolling_moves_the_pages_and_the_current_page(cx: &mut TestAppContext) {
    let h = Harness::open(cx);
    h.show(vec![0; 40], cx);
    h.frame(cx);
    h.visual(cx).simulate_event(ScrollEvent {
        position: point(px(400.), px(500.)),
        delta: ScrollDelta::Pixels(point(px(0.), px(-10_000.))),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
        source: ScrollSource::Pointer,
    });
    h.frame(cx);
    let (scroll, current, visible) = h.pane.read_with(cx, |pane, _| {
        (
            pane.scroll,
            pane.current_page(),
            pane.layout.visible_range(pane.scroll),
        )
    });
    assert_eq!(scroll.y, 10_000.);
    assert!(visible.contains(&current));
    let renders = h.take_renders();
    assert!(
        renders.iter().any(|&(page, _)| page == current),
        "{renders:?}"
    );
}

#[gpui::test]
fn keys_zoom_in_and_back_to_fit_width(cx: &mut TestAppContext) {
    let h = Harness::open(cx);
    h.show(vec![0; 5], cx);
    h.frame(cx);
    cx.update_window(h.window, |_, window, cx| {
        let focus = h.pane.focus_handle(cx);
        window.focus(&focus, cx);
    })
    .unwrap();

    h.visual(cx).simulate_keystrokes("cmd--");
    let (zoom, mode) = h
        .pane
        .read_with(cx, |pane, _| (pane.zoom(), pane.zoom_mode()));
    assert!(zoom < fit_zoom(), "{zoom}");
    assert_eq!(mode, ZoomMode::Custom(zoom));

    // Zooming in stops where the page fills the pane: never a sideways scroll.
    h.visual(cx).simulate_keystrokes("cmd-= cmd-= cmd-= cmd-=");
    let (zoom, canvas) = h
        .pane
        .read_with(cx, |pane, _| (pane.zoom(), pane.layout.canvas()));
    assert!(zoom <= zoom_cap() + 1e-4, "{zoom} > {}", zoom_cap());
    assert!(canvas.width <= PANE_W + 0.5, "{canvas:?}");

    h.visual(cx).dispatch_action(ZoomToFitWidth);
    let (zoom, mode) = h
        .pane
        .read_with(cx, |pane, _| (pane.zoom(), pane.zoom_mode()));
    assert!((zoom - fit_zoom()).abs() < 1e-4);
    assert_eq!(mode, ZoomMode::FitWidth);
}

#[gpui::test]
fn a_narrower_pane_never_scrolls_sideways(cx: &mut TestAppContext) {
    let h = Harness::open(cx);
    h.show(vec![0; 3], cx);
    h.frame(cx);
    h.pane.update(cx, |pane, cx| pane.set_zoom(3.0, cx));
    h.frame(cx);
    let canvas = h.pane.read_with(cx, |pane, _| pane.layout.canvas());
    assert!(canvas.width <= PANE_W + 0.5, "{canvas:?}");
}

#[gpui::test]
fn fit_width_follows_the_pane_until_a_zoom_is_chosen(cx: &mut TestAppContext) {
    let h = Harness::open(cx);
    h.show(vec![0; 5], cx);
    h.frame(cx);
    cx.simulate_window_resize(h.window, size(px(1000.), px(900.)));
    h.frame(cx);
    let zoom = h.pane.read_with(cx, |pane, _| pane.zoom());
    assert!((zoom - fit_width_zoom(&[A4_PT], 1000.)).abs() < 1e-4);

    h.pane.update(cx, |pane, cx| pane.set_zoom(0.8, cx));
    cx.simulate_window_resize(h.window, size(px(700.), px(900.)));
    h.frame(cx);
    assert_eq!(h.pane.read_with(cx, |pane, _| pane.zoom()), 0.8);
}

#[gpui::test]
fn a_small_memory_budget_evicts_offscreen_bitmaps_and_their_textures(cx: &mut TestAppContext) {
    let h = Harness::open(cx);
    h.show(vec![0; 40], cx);
    h.frame(cx);
    let first = h.image(0, cx).unwrap();
    assert!(h.painted(&first, cx));

    h.visual(cx).simulate_event(ScrollEvent {
        position: point(px(400.), px(500.)),
        delta: ScrollDelta::Pixels(point(px(0.), px(-10_000.))),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
        source: ScrollSource::Pointer,
    });
    h.frame(cx);
    h.pane.update(cx, |pane, cx| pane.set_memory_budget(1, cx));
    h.frame(cx);

    let (cached_first, current, used) = h.pane.read_with(cx, |pane, _| {
        (
            pane.cache.get(0).is_some(),
            pane.current_page(),
            pane.memory_used(),
        )
    });
    assert!(!cached_first);
    assert!(!h.painted(&first, cx));
    // What is on screen stays, whatever the budget.
    let visible = h.image(current, cx).unwrap();
    assert!(h.painted(&visible, cx));
    assert!(used > 0);
}

#[gpui::test]
fn scroll_to_reveals_a_far_position(cx: &mut TestAppContext) {
    let h = Harness::open(cx);
    cx.update(|cx| cx.set_reduce_motion(true));
    h.show(vec![0; 40], cx);
    h.frame(cx);
    h.pane.update(cx, |pane, cx| {
        pane.scroll_to(
            PagePosition {
                page: 30,
                x_pt: 100.,
                y_pt: 300.,
            },
            cx,
        )
    });
    h.frame(cx);
    let (current, pulse) = h.pane.read_with(cx, |pane, _| {
        (pane.current_page(), pane.pulse.map(|p| p.anchor.page))
    });
    assert_eq!(current, 30);
    assert_eq!(pulse, Some(30));
}
