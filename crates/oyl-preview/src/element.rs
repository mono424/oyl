//! The element that paints the sheets and takes the pointer.
//!
//! It is a custom element rather than a tree of divs so that the layout is
//! computed in prepaint with the pane's real bounds: a resize re-fits the
//! pages in the same frame, and only the visible sheets cost anything.

use std::panic::Location;
use std::sync::Arc;

use gpui_kit::{
    App, Bounds, BoxShadow, ContentMask, Corners, CursorStyle, DispatchPhase, Element, ElementId,
    Entity, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PinchEvent, Pixels, Refineable as _,
    RenderImage, ScrollEvent, Style, StyleRefinement, Styled, Window, fill, px,
};

use crate::pane::PreviewPane;

/// Shadow under every sheet; its color comes from the `paper_shadow` token.
const SHADOW_OFFSET_Y: f32 = 2.;
const SHADOW_BLUR: f32 = 16.;

/// One sheet to paint, in window coordinates.
pub(crate) struct SheetDraw {
    pub bounds: Bounds<Pixels>,
    /// The page's bitmap and where it lands (its own size at the current
    /// zoom; it may still be from an older compile or zoom).
    pub image: Option<(Arc<RenderImage>, Bounds<Pixels>)>,
}

/// What one frame paints.
pub(crate) struct FrameData {
    pub sheets: Vec<SheetDraw>,
    /// The reveal highlight: where, how strong, corner radius.
    pub highlight: Option<(Bounds<Pixels>, f32, Pixels)>,
    pub paper: Hsla,
    pub paper_dim: Hsla,
    pub paper_shadow: Hsla,
    pub highlight_color: Hsla,
}

/// Paints the pages of a [`PreviewPane`] and routes scroll, pinch, click
/// and pointer movement back to it.
pub(crate) struct PagesElement {
    pane: Entity<PreviewPane>,
    style: StyleRefinement,
}

impl PagesElement {
    pub fn new(pane: Entity<PreviewPane>) -> Self {
        Self {
            pane,
            style: StyleRefinement::default(),
        }
    }
}

impl IntoElement for PagesElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for PagesElement {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

pub(crate) struct PagesPrepaint {
    hitbox: Hitbox,
    frame: FrameData,
}

impl Element for PagesElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<PagesPrepaint>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.refine(&self.style);
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let frame = self
            .pane
            .update(cx, |pane, cx| pane.prepare_frame(bounds, window, cx));
        Some(PagesPrepaint { hitbox, frame })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let Some(PagesPrepaint { hitbox, frame }) = prepaint.take() else {
            return;
        };
        // Paper, not text: the pointer stays an arrow.
        window.set_cursor_style(CursorStyle::Arrow, &hitbox);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            paint_frame(&frame, window);
        });
        self.listen(&hitbox, window);
    }
}

fn paint_frame(frame: &FrameData, window: &mut Window) {
    let shadow = [
        BoxShadow::new(px(0.), px(SHADOW_OFFSET_Y), frame.paper_shadow)
            .blur_radius(px(SHADOW_BLUR)),
    ];
    for sheet in &frame.sheets {
        window.paint_drop_shadows(sheet.bounds, Corners::default(), &shadow);
        window.paint_quad(fill(sheet.bounds, frame.paper));
        if let Some((image, image_bounds)) = &sheet.image
            && let Err(error) = window.paint_image(
                sheet.bounds,
                *image_bounds,
                Corners::default(),
                image.clone(),
                0,
                false,
            )
        {
            log::warn!("preview: painting a page failed: {error:#}");
        }
        if !frame.paper_dim.is_transparent() {
            window.paint_quad(fill(sheet.bounds, frame.paper_dim));
        }
    }
    if let Some((bounds, alpha, radius)) = frame.highlight {
        window.paint_quad(fill(bounds, frame.highlight_color.opacity(alpha)).corner_radii(radius));
    }
}

impl PagesElement {
    fn listen(&self, hitbox: &Hitbox, window: &mut Window) {
        let pane = self.pane.downgrade();

        window.on_mouse_event({
            let (pane, hitbox) = (pane.clone(), hitbox.clone());
            move |event: &ScrollEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && hitbox.should_handle_scroll(window) {
                    pane.update(cx, |pane, cx| pane.handle_scroll(event, cx))
                        .ok();
                    cx.stop_propagation();
                }
            }
        });

        window.on_mouse_event({
            let (pane, hitbox) = (pane.clone(), hitbox.clone());
            move |event: &PinchEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
                    pane.update(cx, |pane, cx| pane.handle_pinch(event, cx))
                        .ok();
                    cx.stop_propagation();
                }
            }
        });

        window.on_mouse_event({
            let (pane, hitbox) = (pane.clone(), hitbox.clone());
            move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
                    pane.update(cx, |pane, cx| pane.handle_mouse_down(event, window, cx))
                        .ok();
                }
            }
        });

        window.on_mouse_event({
            let pane = pane.clone();
            move |event: &MouseUpEvent, phase, _window, cx| {
                if phase == DispatchPhase::Bubble {
                    pane.update(cx, |pane, cx| pane.handle_mouse_up(event, cx))
                        .ok();
                }
            }
        });

        window.on_mouse_event({
            let hitbox = hitbox.clone();
            move |_: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
                    pane.update(cx, |pane, cx| pane.pointer_moved(cx)).ok();
                }
            }
        });
    }
}
