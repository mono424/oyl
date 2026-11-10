//! Oyl's shared vocabulary: the types every crate agrees on.
//!
//! FROZEN CONTRACT. Every crate in the workspace compiles against this one,
//! and they are built in parallel. Changes must be additive (new items, new
//! defaulted trait methods); never rename or remove.
//!
//! - the root: papers, diagnostics, source positions
//! - [`typeset`]: compile requests and laid-out documents
//! - [`agent`]: the harness abstraction and its event stream

pub mod agent;
mod paper;
pub mod typeset;

pub use paper::*;
