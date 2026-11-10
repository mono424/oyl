//! [`PulseDot`]: the small accent dot that breathes while something runs.

use gpui_kit::prelude::*;
use gpui_kit::{Animation, AnimationExt as _, App, ElementId, Hsla, Pixels, Window, div, px};
use oyl_ui::prelude::*;

use crate::motion::{PULSE_PERIOD, breath};

/// The 6px "running" dot: its opacity breathes between 0.35 and 1 over
/// [`PULSE_PERIOD`], easing in and out, in step with every other pulse on
/// screen. Inactive, or under reduced motion, it holds still at full
/// strength and requests no frames.
#[derive(IntoElement)]
pub struct PulseDot {
    id: ElementId,
    color: Option<Hsla>,
    side: Pixels,
    active: bool,
}

impl PulseDot {
    /// A breathing 6px dot in the accent color.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            color: None,
            side: px(6.),
            active: true,
        }
    }

    /// Another color (a token, such as `success` for a finished task).
    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }

    /// Another diameter.
    pub fn size(mut self, side: Pixels) -> Self {
        self.side = side;
        self
    }

    /// Whether it breathes (`false` draws a still dot).
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }
}

impl RenderOnce for PulseDot {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.color.unwrap_or(cx.tokens().accent);
        let dot = div().flex_none().size(self.side).rounded_full().bg(color);
        if self.active {
            dot.with_animation(
                self.id,
                Animation::new(PULSE_PERIOD).repeat_synced(),
                |dot, t| dot.opacity(breath(t)),
            )
            .into_any_element()
        } else {
            dot.into_any_element()
        }
    }
}
