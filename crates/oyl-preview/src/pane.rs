//! The preview pane: the rendered paper, floating on the glass.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{IconName, Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Bounds, Context, EventEmitter, FocusHandle, Focusable, Global, KeyBinding, MouseButton,
    MouseDownEvent, MouseUpEvent, PinchEvent, Pixels, Point, RenderImage, ScrollDelta, ScrollEvent,
    Size, Subscription, Task, Window, actions, div, point, px, size,
};
use oyl_model::SourceLocation;
use oyl_model::typeset::{PagePosition, RenderedDocument};
use oyl_ui::prelude::*;
use oyl_ui::{OylTheme, Tokens};
use serde::{Deserialize, Serialize};

use crate::bitmap::{PageBitmap, rasterize};
use crate::cache::{BitmapCache, DEFAULT_BUDGET_BYTES};
use crate::element::{FrameData, PagesElement, SheetDraw};
use crate::layout::{
    Anchor, PageLayout, ZoomDirection, clamp_zoom, empty_sheet_size, fit_page_zoom, fit_width_zoom,
    max_zoom_without_scroll, pinch_zoom_factor, rescroll, sanitize_page_size, step_zoom,
    wheel_zoom_factor, with_margin,
};
use crate::motion::{ScrollAnimation, Tween, ease_out_cubic, pulse_alpha, shuttle_position};
use crate::raster::{
    JobId, JobTracker, RasterKey, accept_result, concurrency_limit, needs_raster, raster_order,
    raster_scale,
};

/// The key context of the pane, for keymaps.
pub const KEY_CONTEXT: &str = "PreviewPane";

actions!(
    oyl_preview,
    [
        /// Zooms in one step (⌘+).
        ZoomIn,
        /// Zooms out one step (⌘−).
        ZoomOut,
        /// Fits the page width to the pane and keeps tracking it (⌘0).
        ZoomToFitWidth,
        /// Fits the whole current page in the pane (⌥⌘0).
        ZoomToFitPage,
        /// Scrolls up a little (↑).
        ScrollLineUp,
        /// Scrolls down a little (↓).
        ScrollLineDown,
        /// Scrolls up by a screenful (page up, ⇧space).
        ScrollPageUp,
        /// Scrolls down by a screenful (page down, space).
        ScrollPageDown,
        /// Scrolls to the first page (home).
        ScrollToStart,
        /// Scrolls to the last page (end).
        ScrollToEnd,
    ]
);

struct KeysBound;

impl Global for KeysBound {}

/// Registers the pane's key bindings (⌘+, ⌘−, ⌘0, ⌥⌘0, arrows, page keys).
/// They apply only while the pane is focused. Idempotent;
/// [`PreviewPane::new`] calls it.
pub fn init(cx: &mut App) {
    if cx.has_global::<KeysBound>() {
        return;
    }
    cx.set_global(KeysBound);
    let context = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("cmd-=", ZoomIn, context),
        KeyBinding::new("cmd-+", ZoomIn, context),
        KeyBinding::new("cmd--", ZoomOut, context),
        KeyBinding::new("cmd-0", ZoomToFitWidth, context),
        KeyBinding::new("alt-cmd-0", ZoomToFitPage, context),
        KeyBinding::new("up", ScrollLineUp, context),
        KeyBinding::new("down", ScrollLineDown, context),
        KeyBinding::new("pageup", ScrollPageUp, context),
        KeyBinding::new("shift-space", ScrollPageUp, context),
        KeyBinding::new("pagedown", ScrollPageDown, context),
        KeyBinding::new("space", ScrollPageDown, context),
        KeyBinding::new("home", ScrollToStart, context),
        KeyBinding::new("end", ScrollToEnd, context),
    ]);
}

/// What the pane says about the compile behind it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PreviewStatus {
    /// Nothing is running; the pages are current.
    #[default]
    Idle,
    /// A compile is running. A slim hint appears along the top edge when
    /// it takes longer than a moment; the pages stay as they are.
    Compiling,
    /// The last compile failed. The last good pages stay on screen and a
    /// chip names the number of errors.
    Failed {
        /// How many errors the compiler reported.
        errors: usize,
    },
}

/// How the zoom is chosen. Persist it to restore the user's choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoomMode {
    /// The page width fills the pane (capped on wide panes) and follows it
    /// as the pane is resized. The default.
    #[default]
    FitWidth,
    /// The whole current page fits in the pane, following its size.
    FitPage,
    /// A fixed zoom (1.0 = 100%), within 25%–500%.
    Custom(f32),
}

/// What the pane tells its owner.
#[derive(Clone, Debug, PartialEq)]
pub enum PreviewEvent {
    /// The user clicked a spot on a page that maps to this source position.
    JumpToSource(SourceLocation),
    /// The user clicked the error chip.
    ShowProblems,
    /// The zoom choice changed (sent once it settles, not for every step
    /// of a pinch, and not when fit-width merely follows the pane size).
    ZoomChanged(ZoomMode),
}

/// Quiet time after the last zoom change before pages are re-rasterized
/// sharp at the new zoom.
const SETTLE_DELAY: Duration = Duration::from_millis(120);
/// How long the zoom pill stays after the pointer stops moving.
const CHROME_LINGER: Duration = Duration::from_millis(1600);
const CHROME_FADE_IN: Duration = Duration::from_millis(140);
const CHROME_FADE_OUT: Duration = Duration::from_millis(380);
/// A compile shorter than this never shows the progress hint.
const COMPILE_HINT_GRACE: Duration = Duration::from_millis(250);
const COMPILE_HINT_FADE: Duration = Duration::from_millis(220);
const COMPILE_HINT_PERIOD: Duration = Duration::from_millis(1800);
const REVEAL_DURATION: Duration = Duration::from_millis(260);

/// Distance of the zoom pill from the bottom edge.
const PILL_BOTTOM: f32 = 16.;
/// Height of a wheel "line" for mice that scroll in lines.
const WHEEL_LINE: f32 = 20.;
/// One arrow-key scroll step.
const LINE_SCROLL: f32 = 48.;
/// A press that moves further than this is a drag, not a click.
const CLICK_SLOP: f32 = 4.;

/// The highlight band after a reveal, relative to the revealed point (pt).
const PULSE_BEFORE_PT: f32 = 8.;
const PULSE_AFTER_PT: f32 = 220.;
const PULSE_ABOVE_PT: f32 = 11.;
const PULSE_BELOW_PT: f32 = 5.;

#[derive(Clone, Copy, Debug)]
struct PageSlot {
    fingerprint: u64,
    /// The last rasterization that failed, so it is not retried in a loop.
    failed: Option<RasterKey>,
}

#[derive(Clone, Copy, Debug)]
struct Pulse {
    anchor: Anchor,
    started: Instant,
}

/// The rendered paper: true white sheets with a soft shadow, stacked on the
/// window's glass, with zoom, lazy background rasterization and
/// click-to-source.
///
/// Feed it compile results with [`set_document`](Self::set_document) and
/// the compile state with [`set_status`](Self::set_status); subscribe to
/// [`PreviewEvent`]s.
pub struct PreviewPane {
    focus_handle: FocusHandle,

    document: Option<Arc<dyn RenderedDocument>>,
    page_sizes: Vec<Size<f32>>,
    slots: Vec<PageSlot>,

    cache: BitmapCache<PageBitmap>,
    /// Images no longer painted, whose GPU textures are released at the
    /// start of the next frame.
    retired: Vec<Arc<RenderImage>>,
    jobs: JobTracker,

    status: PreviewStatus,
    compiling_since: Option<Instant>,
    compile_hint_timer: Option<Task<()>>,

    zoom_mode: ZoomMode,
    zoom: f32,
    /// False from a zoom change until the zoom has been quiet for
    /// [`SETTLE_DELAY`]; meanwhile bitmaps are shown scaled, not redone.
    zoom_settled: bool,
    emitted_mode: ZoomMode,
    settle_timer: Option<Task<()>>,

    viewport: Size<f32>,
    scale_factor: f32,
    origin: Point<Pixels>,
    layout: PageLayout,
    scroll: Point<f32>,
    scroll_animation: Option<ScrollAnimation>,
    pending_reveal: Option<PagePosition>,
    pulse: Option<Pulse>,
    press: Option<Point<Pixels>>,

    chrome: Tween,
    chrome_activity: Instant,
    chrome_timer_armed: bool,
    pill_hovered: bool,

    _subscriptions: Vec<Subscription>,
}

impl PreviewPane {
    /// An empty pane ("Your paper appears here") at fit-width zoom.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        init(cx);
        let now = Instant::now();
        let subscriptions = vec![
            cx.observe_global::<OylTheme>(|_, cx| cx.notify()),
            cx.on_release(|pane: &mut Self, cx: &mut App| pane.release_all_images(cx)),
        ];
        let limit = concurrency_limit(cx.background_executor().num_cpus());
        Self {
            focus_handle: cx.focus_handle(),
            document: None,
            page_sizes: Vec::new(),
            slots: Vec::new(),
            cache: BitmapCache::new(DEFAULT_BUDGET_BYTES),
            retired: Vec::new(),
            jobs: JobTracker::new(limit),
            status: PreviewStatus::Idle,
            compiling_since: None,
            compile_hint_timer: None,
            zoom_mode: ZoomMode::FitWidth,
            zoom: 1.,
            zoom_settled: true,
            emitted_mode: ZoomMode::FitWidth,
            settle_timer: None,
            viewport: size(0., 0.),
            scale_factor: window.scale_factor(),
            origin: point(px(0.), px(0.)),
            layout: PageLayout::empty(size(0., 0.)),
            scroll: point(0., 0.),
            scroll_animation: None,
            pending_reveal: None,
            pulse: None,
            press: None,
            chrome: Tween::settled(0., now),
            chrome_activity: now,
            chrome_timer_armed: false,
            pill_hovered: false,
            _subscriptions: subscriptions,
        }
    }

    // -- Public API -------------------------------------------------------

    /// Shows a new compile result. The scroll position and every unchanged
    /// page's bitmap are kept; a changed page keeps showing its old bitmap
    /// until the new one is ready. `None` (the compile failed) keeps the
    /// last good document on screen.
    pub fn set_document(
        &mut self,
        document: Option<Arc<dyn RenderedDocument>>,
        cx: &mut Context<Self>,
    ) {
        let Some(document) = document else {
            return;
        };
        let count = document.page_count();
        let mut page_sizes = Vec::with_capacity(count);
        let mut slots = Vec::with_capacity(count);
        for page in 0..count {
            page_sizes.push(sanitize_page_size(document.page_size_pt(page)));
            let fingerprint = document.page_fingerprint(page);
            let failed = self
                .slots
                .get(page)
                .filter(|slot| slot.fingerprint == fingerprint)
                .and_then(|slot| slot.failed);
            slots.push(PageSlot {
                fingerprint,
                failed,
            });
        }

        let gone = self.cache.truncate(count);
        self.retire(gone);
        self.page_sizes = page_sizes;
        self.slots = slots;
        self.document = Some(document);
        if self.pulse.is_some_and(|pulse| pulse.anchor.page >= count) {
            self.pulse = None;
        }
        if self
            .pending_reveal
            .is_some_and(|position| position.page >= count)
        {
            self.pending_reveal = None;
        }

        let top = self.top_anchor();
        self.relayout(top, top, cx);
        self.pump_jobs(cx);
        cx.notify();
    }

    /// The document on screen (the last good one).
    pub fn document(&self) -> Option<&Arc<dyn RenderedDocument>> {
        self.document.as_ref()
    }

    /// Updates the compile state shown by the pane.
    pub fn set_status(&mut self, status: PreviewStatus, cx: &mut Context<Self>) {
        if self.status == status {
            return;
        }
        let was_compiling = self.status == PreviewStatus::Compiling;
        self.status = status;
        if self.status == PreviewStatus::Compiling {
            if !was_compiling {
                self.compiling_since = Some(Instant::now());
                self.compile_hint_timer = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(COMPILE_HINT_GRACE).await;
                    this.update(cx, |_, cx| cx.notify()).ok();
                }));
            }
        } else {
            self.compiling_since = None;
            self.compile_hint_timer = None;
        }
        cx.notify();
    }

    /// The compile state shown by the pane.
    pub fn status(&self) -> &PreviewStatus {
        &self.status
    }

    /// Zooms in one step, around the middle of the pane.
    pub fn zoom_in(&mut self, cx: &mut Context<Self>) {
        let zoom = step_zoom(self.zoom, ZoomDirection::In);
        self.set_zoom_mode(ZoomMode::Custom(zoom), cx);
    }

    /// Zooms out one step, around the middle of the pane.
    pub fn zoom_out(&mut self, cx: &mut Context<Self>) {
        let zoom = step_zoom(self.zoom, ZoomDirection::Out);
        self.set_zoom_mode(ZoomMode::Custom(zoom), cx);
    }

    /// Fits the page width to the pane, and keeps it fitted as the pane is
    /// resized.
    pub fn zoom_fit_width(&mut self, cx: &mut Context<Self>) {
        self.set_zoom_mode(ZoomMode::FitWidth, cx);
    }

    /// Fits the whole current page in the pane, and keeps it fitted.
    pub fn zoom_fit_page(&mut self, cx: &mut Context<Self>) {
        self.set_zoom_mode(ZoomMode::FitPage, cx);
    }

    /// Sets a fixed zoom (1.0 = 100%), clamped to 25%–500%.
    pub fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        self.set_zoom_mode(ZoomMode::Custom(zoom), cx);
    }

    /// Sets how the zoom is chosen (e.g. restoring a persisted choice).
    pub fn set_zoom_mode(&mut self, mode: ZoomMode, cx: &mut Context<Self>) {
        self.zoom_mode = match mode {
            ZoomMode::Custom(zoom) => ZoomMode::Custom(clamp_zoom(zoom)),
            other => other,
        };
        self.scroll_animation = None;
        let center = self.center_anchor();
        self.relayout(center, center, cx);
        // Settle even when the zoom value did not move, so a changed
        // choice is still reported.
        self.schedule_settle(cx);
        self.reveal_chrome(cx);
        cx.notify();
    }

    /// The zoom on screen (1.0 = 100%).
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// How the zoom is chosen.
    pub fn zoom_mode(&self) -> ZoomMode {
        self.zoom_mode
    }

    /// Scrolls (smoothly, unless reduced motion is on) so that a page
    /// position is in view, then briefly pulses a highlight there. The pane
    /// does not move when the position is already comfortably visible.
    pub fn scroll_to(&mut self, position: PagePosition, cx: &mut Context<Self>) {
        if position.page >= self.slots.len() {
            return;
        }
        if self.has_viewport() {
            self.reveal(position, cx);
        } else {
            self.pending_reveal = Some(position);
        }
        cx.notify();
    }

    /// Number of pages of the document on screen.
    pub fn page_count(&self) -> usize {
        self.slots.len()
    }

    /// The page in view (0-based): the page at the top when scrolled to the
    /// start, at the bottom when scrolled to the end, in the middle between.
    pub fn current_page(&self) -> usize {
        self.layout.current_page(self.scroll).unwrap_or(0)
    }

    /// Changes the memory budget of page bitmaps (300 MB by default).
    /// Pages on screen are never evicted, whatever the budget.
    pub fn set_memory_budget(&mut self, bytes: usize, cx: &mut Context<Self>) {
        self.cache.set_budget(bytes);
        let protected = with_margin(self.layout.visible_range(self.scroll), self.slots.len());
        let evicted = self.cache.evict(protected);
        if !evicted.is_empty() {
            self.retire(evicted);
            cx.notify();
        }
    }

    /// Bytes of page bitmaps held right now.
    pub fn memory_used(&self) -> usize {
        self.cache.used_bytes()
    }

    // -- Layout and zoom --------------------------------------------------

    fn has_viewport(&self) -> bool {
        self.viewport.width > 0. && self.viewport.height > 0.
    }

    fn top_anchor(&self) -> Point<f32> {
        point(self.viewport.width / 2., 0.)
    }

    fn center_anchor(&self) -> Point<f32> {
        point(self.viewport.width / 2., self.viewport.height / 2.)
    }

    /// The zoom the current mode asks for.
    fn mode_zoom(&self) -> f32 {
        match self.zoom_mode {
            // Never wider than the pane: shrinking the column shrinks the
            // page instead of adding a sideways scrollbar.
            ZoomMode::Custom(zoom) => clamp_zoom(zoom).min(self.zoom_cap()),
            _ if !self.has_viewport() || self.page_sizes.is_empty() => self.zoom,
            ZoomMode::FitWidth => fit_width_zoom(&self.page_sizes, self.viewport.width),
            ZoomMode::FitPage => {
                let page = self.current_page().min(self.page_sizes.len() - 1);
                fit_page_zoom(self.page_sizes[page], self.viewport)
            }
        }
    }

    /// Lays the pages out again (new pages, zoom or pane size), keeping
    /// what was under `old_local` under `new_local`.
    fn relayout(&mut self, old_local: Point<f32>, new_local: Point<f32>, cx: &mut Context<Self>) {
        let zoom = self.mode_zoom();
        let zoom_moved = (zoom - self.zoom).abs() > 1e-4;
        self.zoom = zoom;
        let layout = PageLayout::new(&self.page_sizes, zoom, self.viewport);
        self.scroll = rescroll(&self.layout, self.scroll, old_local, &layout, new_local);
        self.layout = layout;
        if zoom_moved {
            self.schedule_settle(cx);
        }
    }

    /// The largest zoom at which every page fits the pane's width.
    fn zoom_cap(&self) -> f32 {
        if !self.has_viewport() || self.page_sizes.is_empty() {
            return clamp_zoom(f32::MAX);
        }
        max_zoom_without_scroll(&self.page_sizes, self.viewport.width)
    }

    /// Zooms by a factor around a pane-local point (⌘-scroll, pinch).
    fn zoom_by(&mut self, factor: f32, around: Point<f32>, cx: &mut Context<Self>) {
        let zoom = clamp_zoom(self.zoom * factor).min(self.zoom_cap());
        if (zoom - self.zoom).abs() < 1e-4 {
            return;
        }
        self.zoom_mode = ZoomMode::Custom(zoom);
        self.scroll_animation = None;
        self.relayout(around, around, cx);
        self.reveal_chrome(cx);
        cx.notify();
    }

    /// Re-rasterizes at the new zoom once it has been quiet for a moment.
    fn schedule_settle(&mut self, cx: &mut Context<Self>) {
        self.zoom_settled = false;
        self.settle_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTLE_DELAY).await;
            this.update(cx, |pane, cx| pane.settle_zoom(cx)).ok();
        }));
    }

    fn settle_zoom(&mut self, cx: &mut Context<Self>) {
        self.zoom_settled = true;
        if self.zoom_mode != self.emitted_mode {
            self.emitted_mode = self.zoom_mode;
            cx.emit(PreviewEvent::ZoomChanged(self.zoom_mode));
        }
        self.pump_jobs(cx);
        cx.notify();
    }

    /// Takes the pane's size from the frame being drawn. Returns whether
    /// the chrome (zoom label, page number) needs another render.
    fn update_viewport(
        &mut self,
        viewport: Size<f32>,
        scale_factor: f32,
        cx: &mut Context<Self>,
    ) -> bool {
        let resized = viewport != self.viewport;
        let rescaled = scale_factor != self.scale_factor;
        if !resized && !rescaled {
            return false;
        }
        let first = !self.has_viewport();
        let old_top = self.top_anchor();
        self.viewport = viewport;
        self.scale_factor = scale_factor;
        if resized {
            let new_top = self.top_anchor();
            self.relayout(old_top, new_top, cx);
        }
        if first {
            // Nothing was on screen to scale: the zoom is final at once.
            self.zoom_settled = true;
            self.settle_timer = None;
        }
        true
    }

    fn scroll_by(&mut self, delta: Point<f32>, cx: &mut Context<Self>) {
        self.scroll_animation = None;
        let next = self
            .layout
            .clamp_scroll(point(self.scroll.x + delta.x, self.scroll.y + delta.y));
        if next != self.scroll {
            self.scroll = next;
            cx.notify();
        }
        self.reveal_chrome(cx);
    }

    fn scroll_to_offset(&mut self, y: f32, cx: &mut Context<Self>) {
        self.scroll_animation = None;
        let next = self.layout.clamp_scroll(point(self.scroll.x, y));
        if next != self.scroll {
            self.scroll = next;
            cx.notify();
        }
        self.reveal_chrome(cx);
    }

    fn reveal(&mut self, position: PagePosition, cx: &mut Context<Self>) {
        let anchor = Anchor {
            page: position.page,
            pt: point(position.x_pt, position.y_pt),
        };
        let target = self.layout.reveal(self.scroll, anchor);
        let now = Instant::now();
        let distance = (target.x - self.scroll.x).abs() + (target.y - self.scroll.y).abs();
        if cx.reduce_motion() || distance < 1. {
            self.scroll = target;
            self.scroll_animation = None;
        } else {
            // Far jumps start close to the target, so the glide stays short.
            let reach = self.viewport.height * 1.5;
            let from = point(
                self.scroll.x,
                target.y - (target.y - self.scroll.y).clamp(-reach, reach),
            );
            self.scroll_animation = Some(ScrollAnimation::new(from, target, REVEAL_DURATION, now));
        }
        self.pulse = Some(Pulse {
            anchor,
            started: now,
        });
    }

    fn to_local(&self, position: Point<Pixels>) -> Point<f32> {
        point(
            (position.x - self.origin.x).as_f32(),
            (position.y - self.origin.y).as_f32(),
        )
    }

    // -- Rasterizing ------------------------------------------------------

    /// Starts background rasterizations for the visible pages and their
    /// one-page margin, nearest first, as far as the parallelism allows.
    fn pump_jobs(&mut self, cx: &mut Context<Self>) {
        let Some(document) = self.document.clone() else {
            return;
        };
        let count = self.slots.len();
        if !self.jobs.has_capacity() || self.layout.len() != count {
            return;
        }
        let visible = self.layout.visible_range(self.scroll);
        let focus = self
            .layout
            .current_page(self.scroll)
            .unwrap_or(visible.start);
        for page in raster_order(visible, focus, count) {
            if !self.jobs.has_capacity() {
                break;
            }
            if self.jobs.is_running_page(page) {
                continue;
            }
            let held = self.cache.get(page).map(PageBitmap::held);
            let slot = self.slots[page];
            let want = RasterKey {
                page,
                fingerprint: slot.fingerprint,
                pixels_per_pt: self.target_scale(page),
            };
            if !needs_raster(held, &want, self.zoom_settled)
                || slot.failed.is_some_and(|f| f.same_job(&want))
            {
                continue;
            }
            let id = self.jobs.start(want);
            let document = document.clone();
            let render = cx
                .background_executor()
                .spawn(async move { rasterize(&*document, want) });
            cx.spawn(async move |this, cx| {
                let result = render.await;
                this.update(cx, |pane, cx| pane.finish_job(id, result, cx))
                    .ok();
            })
            .detach();
        }
    }

    /// The resolution a page should have at the zoom on screen.
    fn target_scale(&self, page: usize) -> f32 {
        raster_scale(self.zoom, self.scale_factor, self.page_sizes[page])
    }

    fn finish_job(
        &mut self,
        id: JobId,
        result: anyhow::Result<PageBitmap>,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.jobs.finish(id) else {
            return;
        };
        match result {
            Ok(bitmap) => {
                let current = self.slots.get(key.page).map(|slot| slot.fingerprint);
                let held = self.cache.get(key.page).map(PageBitmap::held);
                let accepted = current.is_some()
                    && accept_result(&key, current, held, self.target_scale(key.page));
                if accepted {
                    let bytes = bitmap.bytes();
                    if let Some(old) = self.cache.insert(key.page, bitmap, bytes) {
                        self.retired.push(old.image);
                    }
                    let visible = self.layout.visible_range(self.scroll);
                    let evicted = self
                        .cache
                        .evict(with_margin(visible.clone(), self.slots.len()));
                    self.retire(evicted);
                    if visible.contains(&key.page) || !self.retired.is_empty() {
                        cx.notify();
                    }
                }
                // A dropped bitmap was never painted; letting it go frees it.
            }
            Err(error) => {
                log::warn!("preview: {error:#}");
                if let Some(slot) = self.slots.get_mut(key.page)
                    && slot.fingerprint == key.fingerprint
                {
                    slot.failed = Some(key);
                }
            }
        }
        self.pump_jobs(cx);
    }

    fn retire(&mut self, bitmaps: Vec<PageBitmap>) {
        self.retired
            .extend(bitmaps.into_iter().map(|bitmap| bitmap.image));
    }

    fn release_all_images(&mut self, cx: &mut App) {
        let bitmaps = self.cache.drain();
        self.retire(bitmaps);
        for image in self.retired.drain(..) {
            cx.drop_image(image, None);
        }
    }

    // -- Frame ------------------------------------------------------------

    /// Everything the page element needs to paint this frame. Runs during
    /// prepaint, when the pane's bounds are known.
    pub(crate) fn prepare_frame(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FrameData {
        // Released here, at the start of a frame that no longer paints them.
        for image in self.retired.drain(..) {
            cx.drop_image(image, Some(window));
        }

        self.origin = bounds.origin;
        let viewport = size(bounds.size.width.as_f32(), bounds.size.height.as_f32());
        let scale_factor = window.scale_factor();
        let mut render_again = self.update_viewport(viewport, scale_factor, cx);
        if let Some(position) = self.pending_reveal.take() {
            self.reveal(position, cx);
            render_again = true;
        }

        let now = Instant::now();
        if let Some(animation) = self.scroll_animation {
            self.scroll = self.layout.clamp_scroll(animation.value(now));
            if animation.is_finished(now) {
                self.scroll_animation = None;
            }
            render_again = true;
        }

        let count = self.slots.len();
        let visible = if self.layout.len() == count {
            self.layout.visible_range(self.scroll)
        } else {
            0..0
        };
        self.cache.touch(with_margin(visible.clone(), count));
        self.pump_jobs(cx);

        let snap = |v: f32| (v * scale_factor).round() / scale_factor;
        let window_rect = |x: f32, y: f32, w: f32, h: f32| {
            let left = snap(bounds.origin.x.as_f32() + x - self.scroll.x);
            let top = snap(bounds.origin.y.as_f32() + y - self.scroll.y);
            Bounds::new(point(px(left), px(top)), size(px(snap(w)), px(snap(h))))
        };

        let zoom = self.layout.zoom();
        let sheets = visible
            .clone()
            .filter_map(|page| {
                let sheet = self.layout.page(page)?;
                let rect = window_rect(
                    sheet.origin.x,
                    sheet.origin.y,
                    sheet.size.width,
                    sheet.size.height,
                );
                let image = self.cache.get(page).map(|bitmap| {
                    let covers = bitmap.size_pt();
                    let image_rect = Bounds::new(
                        rect.origin,
                        size(
                            px(snap(covers.width * zoom)),
                            px(snap(covers.height * zoom)),
                        ),
                    );
                    (bitmap.image.clone(), image_rect)
                });
                Some(SheetDraw {
                    bounds: rect,
                    image,
                })
            })
            .collect();

        let mut highlight = None;
        if let Some(pulse) = self.pulse {
            let alpha = pulse_alpha(now.saturating_duration_since(pulse.started));
            match self.layout.page(pulse.anchor.page) {
                Some(sheet) if alpha > 0. => {
                    let page_w = sheet.size.width / zoom;
                    let page_h = sheet.size.height / zoom;
                    let left = (pulse.anchor.pt.x - PULSE_BEFORE_PT).clamp(0., page_w);
                    let right = (pulse.anchor.pt.x + PULSE_AFTER_PT).clamp(0., page_w);
                    let top = (pulse.anchor.pt.y - PULSE_ABOVE_PT).clamp(0., page_h);
                    let bottom = (pulse.anchor.pt.y + PULSE_BELOW_PT).clamp(0., page_h);
                    if visible.contains(&pulse.anchor.page) && right > left && bottom > top {
                        let rect = window_rect(
                            sheet.origin.x + left * zoom,
                            sheet.origin.y + top * zoom,
                            (right - left) * zoom,
                            (bottom - top) * zoom,
                        );
                        highlight = Some((rect, alpha, px(3. * zoom.max(0.5))));
                    }
                    render_again = true;
                }
                _ => self.pulse = None,
            }
        }

        if render_again {
            let pane = cx.entity();
            window.on_next_frame(move |_, cx| pane.update(cx, |_, cx| cx.notify()));
        }

        let k = cx.tokens();
        FrameData {
            sheets,
            highlight,
            paper: k.paper,
            paper_dim: k.paper_dim,
            paper_shadow: k.paper_shadow,
            highlight_color: k.selection,
        }
    }

    // -- Input --------------------------------------------------------------

    pub(crate) fn handle_scroll(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        if event.modifiers.platform {
            let (delta_y, precise) = match event.delta {
                ScrollDelta::Pixels(delta) => (delta.y.as_f32(), true),
                ScrollDelta::Lines(delta) => (delta.y, false),
            };
            let around = self.to_local(event.position);
            self.zoom_by(wheel_zoom_factor(delta_y, precise), around, cx);
            return;
        }
        let delta = event.delta.pixel_delta(px(WHEEL_LINE));
        self.scroll_by(point(-delta.x.as_f32(), -delta.y.as_f32()), cx);
    }

    pub(crate) fn handle_pinch(&mut self, event: &PinchEvent, cx: &mut Context<Self>) {
        let around = self.to_local(event.position);
        self.zoom_by(pinch_zoom_factor(event.delta), around, cx);
    }

    pub(crate) fn handle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        if event.button == MouseButton::Left && !event.first_mouse {
            self.press = Some(event.position);
        }
    }

    pub(crate) fn handle_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some(pressed_at) = self.press.take() else {
            return;
        };
        if event.button != MouseButton::Left {
            return;
        }
        let moved = (event.position.x - pressed_at.x).as_f32().abs()
            + (event.position.y - pressed_at.y).as_f32().abs();
        if moved > CLICK_SLOP {
            return;
        }
        let local = self.to_local(event.position);
        let Some(anchor) = self.layout.hit_test(self.scroll, local) else {
            return;
        };
        let Some(document) = &self.document else {
            return;
        };
        let position = PagePosition {
            page: anchor.page,
            x_pt: anchor.pt.x,
            y_pt: anchor.pt.y,
        };
        if let Some(location) = document.source_at(position) {
            cx.emit(PreviewEvent::JumpToSource(location));
        }
    }

    pub(crate) fn pointer_moved(&mut self, cx: &mut Context<Self>) {
        self.reveal_chrome(cx);
    }

    // -- Chrome -------------------------------------------------------------

    /// Fades the zoom pill in and restarts its linger.
    fn reveal_chrome(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        self.chrome_activity = now;
        if self.chrome.target() < 1. {
            self.chrome.retarget(1., CHROME_FADE_IN, now);
            cx.notify();
        }
        self.arm_chrome_timer(cx);
    }

    fn arm_chrome_timer(&mut self, cx: &mut Context<Self>) {
        if self.chrome_timer_armed {
            return;
        }
        self.chrome_timer_armed = true;
        cx.spawn(async move |this, cx| {
            let mut wait = CHROME_LINGER;
            loop {
                cx.background_executor().timer(wait).await;
                match this.update(cx, |pane, cx| pane.chrome_tick(cx)) {
                    Ok(Some(next)) => wait = next,
                    _ => break,
                }
            }
        })
        .detach();
    }

    /// Fades the pill out once the pointer has rested long enough; returns
    /// how long to wait before checking again otherwise.
    fn chrome_tick(&mut self, cx: &mut Context<Self>) -> Option<Duration> {
        if self.pill_hovered {
            return Some(CHROME_LINGER);
        }
        let idle = self.chrome_activity.elapsed();
        if idle < CHROME_LINGER {
            return Some(CHROME_LINGER - idle);
        }
        self.chrome.retarget(0., CHROME_FADE_OUT, Instant::now());
        self.chrome_timer_armed = false;
        cx.notify();
        None
    }

    fn pill_hover(&mut self, hovered: bool, cx: &mut Context<Self>) {
        self.pill_hovered = hovered;
        if hovered {
            self.reveal_chrome(cx);
        } else {
            self.chrome_activity = Instant::now();
            self.arm_chrome_timer(cx);
        }
    }

    // -- Render pieces ------------------------------------------------------

    fn render_empty(&self, k: &Tokens) -> Option<impl IntoElement + use<>> {
        if !self.slots.is_empty() || !self.has_viewport() {
            return None;
        }
        let caption = div()
            .font_family(k.serif_font.clone())
            .text_sm()
            .text_color(k.text_faint)
            .child("Your paper appears here");
        let content = match empty_sheet_size(self.viewport) {
            Some(sheet) => div()
                .w(px(sheet.width))
                .h(px(sheet.height))
                .border_1()
                .border_color(k.hairline_strong)
                .rounded(px(2.))
                .flex()
                .items_center()
                .justify_center()
                .child(caption),
            None => caption,
        };
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(content),
        )
    }

    fn render_compile_hint(
        &self,
        k: &Tokens,
        now: Instant,
        window: &mut Window,
        cx: &App,
    ) -> Option<impl IntoElement + use<>> {
        let since = self.compiling_since?;
        let shown = now
            .saturating_duration_since(since)
            .checked_sub(COMPILE_HINT_GRACE)?;
        if !self.has_viewport() {
            return None;
        }
        let opacity = ease_out_cubic(shown.as_secs_f32() / COMPILE_HINT_FADE.as_secs_f32());
        let still = cx.reduce_motion();
        let position = if still {
            0.5
        } else {
            shuttle_position(shown, COMPILE_HINT_PERIOD)
        };
        if !still || opacity < 1. {
            window.request_animation_frame();
        }
        let track = self.viewport.width;
        let segment = (track * 0.18).clamp(40., 180.);
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(px(2.))
                .opacity(opacity)
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left(px((track - segment) * position))
                        .w(px(segment))
                        .h_full()
                        .rounded_full()
                        .bg(k.accent),
                ),
        )
    }

    fn render_error_chip(
        &self,
        k: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let PreviewStatus::Failed { errors } = self.status else {
            return None;
        };
        let label = match errors {
            0 => "Compile failed".to_string(),
            1 => "1 error".to_string(),
            n => format!("{n} errors"),
        };
        // Plain text, no chip: the error count is information, not a control
        // that should draw the eye away from the page.
        let hover_text = k.danger.opacity(0.75);
        Some(
            div()
                .absolute()
                .right(px(20.))
                .bottom(px(PILL_BOTTOM + 6.))
                .child(
                    div()
                        .id("oyl-preview-problems")
                        .occlude()
                        .h(px(24.))
                        .h_flex()
                        .items_center()
                        .text_xs()
                        .text_color(k.danger)
                        .hover(move |style| style.text_color(hover_text))
                        .cursor_pointer()
                        .child(label)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(PreviewEvent::ShowProblems))),
                ),
        )
    }

    fn render_pill(
        &self,
        k: &Tokens,
        now: Instant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        if self.slots.is_empty() {
            return None;
        }
        if self.chrome.is_animating(now) {
            window.request_animation_frame();
        }
        let opacity = self.chrome.value(now);
        if opacity <= 0.01 {
            return None;
        }
        let pages = format!("{} / {}", self.current_page() + 1, self.slots.len());
        let zoom = format!("{}%", (self.zoom * 100.).round() as i32);
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(PILL_BOTTOM))
                .flex()
                .justify_center()
                .child(
                    oyl_ui::island(cx)
                        .id("oyl-preview-pill")
                        .occlude()
                        .opacity(opacity)
                        .on_hover(
                            cx.listener(|this, hovered: &bool, _, cx| {
                                this.pill_hover(*hovered, cx)
                            }),
                        )
                        .child(
                            div()
                                .px(px(8.))
                                .text_xs()
                                .text_color(k.text_soft)
                                .child(pages),
                        )
                        .child(div().w(px(1.)).h(px(16.)).bg(k.hairline))
                        .child(
                            Button::new("oyl-preview-zoom-out")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Minus)
                                .tooltip("Zoom out")
                                .on_click(cx.listener(|this, _, _, cx| this.zoom_out(cx))),
                        )
                        .child(
                            div()
                                .id("oyl-preview-zoom-fit")
                                .h_flex()
                                .justify_center()
                                .min_w(px(44.))
                                .h(px(24.))
                                .rounded(k.radius_s)
                                .hover(|style| style.bg(k.surface_hover))
                                .text_xs()
                                .text_color(k.text)
                                .cursor_pointer()
                                .child(zoom)
                                .on_click(cx.listener(|this, _, _, cx| this.zoom_fit_width(cx))),
                        )
                        .child(
                            Button::new("oyl-preview-zoom-in")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Plus)
                                .tooltip("Zoom in")
                                .on_click(cx.listener(|this, _, _, cx| this.zoom_in(cx))),
                        ),
                ),
        )
    }
}

impl Render for PreviewPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let k = cx.tokens().clone();
        let now = Instant::now();
        let empty = self.render_empty(&k);
        let hint = self.render_compile_hint(&k, now, window, cx);
        let chip = self.render_error_chip(&k, cx);
        let pill = self.render_pill(&k, now, window, cx);
        div()
            .id("oyl-preview")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .overflow_hidden()
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.zoom_in(cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.zoom_out(cx)))
            .on_action(cx.listener(|this, _: &ZoomToFitWidth, _, cx| this.zoom_fit_width(cx)))
            .on_action(cx.listener(|this, _: &ZoomToFitPage, _, cx| this.zoom_fit_page(cx)))
            .on_action(cx.listener(|this, _: &ScrollLineUp, _, cx| {
                this.scroll_by(point(0., -LINE_SCROLL), cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollLineDown, _, cx| {
                this.scroll_by(point(0., LINE_SCROLL), cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollPageUp, _, cx| {
                let step = this.viewport.height * 0.9;
                this.scroll_by(point(0., -step), cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollPageDown, _, cx| {
                let step = this.viewport.height * 0.9;
                this.scroll_by(point(0., step), cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollToStart, _, cx| this.scroll_to_offset(0., cx)))
            .on_action(
                cx.listener(|this, _: &ScrollToEnd, _, cx| this.scroll_to_offset(f32::MAX, cx)),
            )
            .child(
                PagesElement::new(cx.entity())
                    .absolute()
                    .inset_0()
                    .size_full(),
            )
            .children(empty)
            .children(hint)
            .children(chip)
            .children(pill)
    }
}

impl Focusable for PreviewPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PreviewEvent> for PreviewPane {}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod ui_tests;
