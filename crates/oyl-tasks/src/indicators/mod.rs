//! The progress indicators: small, calm, accent-on-hairline elements that
//! the tray, the task card, the chat and the app shell all use.
//!
//! Every indicator is a `RenderOnce` element built fresh each frame; the
//! ones that move keep their clock in GPUI's element state under their
//! id, so give each a stable, unique id. None requests a frame when it is
//! at rest or when reduced motion is on ([`crate::reduce_motion`]).

mod bar;
mod pulse;
mod ring;
mod timeline;

pub use bar::{BAR_HEIGHT, BAR_LABEL_GAP, BAR_LABEL_HEIGHT, ProgressBar, SWEEP_WIDTH};
pub use pulse::PulseDot;
pub use ring::{
    ProgressRing, RING_LABEL_MIN, RING_MAX, RING_MIN, RingArc, RingLabel, SPIN_ARC, paint_ring,
    tabular_figures,
};
pub use timeline::{MIN_PHASE_WIDTH, PhaseState, StepTimeline, TIMELINE_HEIGHT};
