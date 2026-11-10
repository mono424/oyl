//! Oyl's design system: tokens, light/dark, and the glass shapes.
//!
//! ```ignore
//! use oyl_ui::prelude::*;
//!
//! let k = cx.tokens();
//! oyl_ui::surface(cx).p_3().text_color(k.text).child("Hello")
//! ```

pub mod glass;
pub mod theme;

pub use glass::*;
pub use theme::*;

pub mod prelude {
    pub use crate::theme::{ActiveTokens as _, Appearance, OylTheme, Tokens};
}
