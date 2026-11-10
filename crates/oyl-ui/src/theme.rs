//! Oyl's look: frosted glass, warm ink, one amber accent.
//!
//! The window is a blurred pane of the desktop under a tinted *veil*. Nothing
//! sits in boxes unless it floats: floating things are *surfaces* (a faint
//! lighter glass) or *raised* (nearly opaque, for menus and the palette).
//! The paper itself is the only truly opaque thing on screen.
//!
//! Every crate reads colors from [`Tokens`] (`cx.tokens()`), never from
//! literals, so light and dark stay in step. The component library's own
//! theme is projected from the same tokens in [`apply`].

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{App, Global, Hsla, Pixels, Rgba, SharedString, Window, WindowAppearance, px, rgba};
use serde::{Deserialize, Serialize};

/// What the user chose in the light/dark toggle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Follow macOS.
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    /// The next choice when the toggle is clicked: it flips what is on
    /// screen right now, and so leaves `System` for an explicit choice.
    pub fn toggled(self, cx: &App) -> Appearance {
        if self.is_dark(cx) {
            Appearance::Light
        } else {
            Appearance::Dark
        }
    }

    pub fn is_dark(self, cx: &App) -> bool {
        match self {
            Appearance::Dark => true,
            Appearance::Light => false,
            Appearance::System => matches!(
                cx.window_appearance(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
        }
    }
}

/// The design tokens. Colors are final (alpha included): paint them as they
/// are, do not fade them again.
#[derive(Clone, Debug)]
pub struct Tokens {
    pub dark: bool,

    // -- Window ----------------------------------------------------------
    /// The tint over the blurred desktop. The window root paints it once
    /// for the whole window; do not paint it again.
    pub veil: Hsla,
    /// A calmer, more opaque tint for where long text is read or written
    /// (the editor column), painted on top of the veil.
    pub veil_strong: Hsla,

    // -- Cards -----------------------------------------------------------
    /// The card the source (and the agent) sit in: recessed and darker than
    /// the veil in dark mode, a quiet white sheet in light mode.
    pub card: Hsla,
    /// 1px edge of a card.
    pub card_edge: Hsla,
    /// The same card tone, opaque: for a card that slides over the paper,
    /// which must not show through.
    pub card_solid: Hsla,

    // -- Floating things --------------------------------------------------
    /// Resting glass surface (islands, cards, chat bubbles).
    pub surface: Hsla,
    pub surface_hover: Hsla,
    pub surface_active: Hsla,
    /// Nearly opaque, for what overlaps content: menus, palette, popovers.
    pub raised: Hsla,
    /// 1px edges of surfaces.
    pub hairline: Hsla,
    /// 1px edges of raised things and focused controls.
    pub hairline_strong: Hsla,

    // -- Text ------------------------------------------------------------
    pub text: Hsla,
    /// Secondary text.
    pub text_soft: Hsla,
    /// Placeholders, captions, line numbers.
    pub text_faint: Hsla,

    // -- Accent ----------------------------------------------------------
    /// The one accent: amber, like lamplight on paper.
    pub accent: Hsla,
    /// Accent as a translucent fill (selected rows, chips).
    pub accent_soft: Hsla,
    /// Text on an `accent` fill.
    pub on_accent: Hsla,

    // -- Meaning ---------------------------------------------------------
    pub danger: Hsla,
    pub danger_soft: Hsla,
    pub warning: Hsla,
    pub warning_soft: Hsla,
    pub success: Hsla,
    pub success_soft: Hsla,

    // -- Paper -----------------------------------------------------------
    /// The sheet. Always paper-white: the preview shows what will print.
    pub paper: Hsla,
    /// Shadow under a sheet.
    pub paper_shadow: Hsla,
    /// A wash over the sheets in dark mode so they do not glare.
    pub paper_dim: Hsla,

    // -- Editor ----------------------------------------------------------
    pub selection: Hsla,
    pub line_highlight: Hsla,
    pub caret: Hsla,

    // -- Shape -----------------------------------------------------------
    /// Small controls: chips, keys.
    pub radius_s: Pixels,
    /// Controls: buttons, inputs, rows.
    pub radius_m: Pixels,
    /// Surfaces: cards, panels, the palette.
    pub radius_l: Pixels,

    // -- Type ------------------------------------------------------------
    pub ui_font: SharedString,
    pub mono_font: SharedString,
    /// For the few display moments (welcome title, empty states).
    pub serif_font: SharedString,
}

fn c(hex: u32) -> Hsla {
    let color: Rgba = rgba(hex);
    color.into()
}

impl Tokens {
    pub fn dark() -> Self {
        Self {
            dark: true,
            veil: c(0x15151AB8),
            veil_strong: c(0x111114C7),
            card: c(0x0C0C0FD1),
            card_edge: c(0xFFFFFF12),
            card_solid: c(0x141417FF),
            surface: c(0xFFFFFF0F),
            surface_hover: c(0xFFFFFF1A),
            surface_active: c(0xFFFFFF26),
            raised: c(0x222227F2),
            hairline: c(0xFFFFFF17),
            hairline_strong: c(0xFFFFFF2B),
            text: c(0xEDECEAFF),
            text_soft: c(0xEDECEAA6),
            text_faint: c(0xEDECEA61),
            accent: c(0xE9B76AFF),
            accent_soft: c(0xE9B76A2B),
            on_accent: c(0x1C1407FF),
            danger: c(0xFF7B72FF),
            danger_soft: c(0xFF7B7229),
            warning: c(0xF0C355FF),
            warning_soft: c(0xF0C35529),
            success: c(0x74D19BFF),
            success_soft: c(0x74D19B29),
            paper: c(0xFCFBF8FF),
            paper_shadow: c(0x00000066),
            paper_dim: c(0x15151A14),
            selection: c(0xE9B76A47),
            line_highlight: c(0xFFFFFF0A),
            caret: c(0xE9B76AFF),
            radius_s: px(6.),
            radius_m: px(10.),
            radius_l: px(16.),
            ui_font: ".SystemUIFont".into(),
            mono_font: "Menlo".into(),
            serif_font: "Charter".into(),
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            veil: c(0xF3F1ECBD),
            veil_strong: c(0xFBFAF7CC),
            card: c(0xFFFFFFC7),
            card_edge: c(0x0000000F),
            card_solid: c(0xFCFBF9FF),
            surface: c(0xFFFFFF8C),
            surface_hover: c(0xFFFFFFBF),
            surface_active: c(0xFFFFFFE6),
            raised: c(0xFFFFFFF2),
            hairline: c(0x00000014),
            hairline_strong: c(0x00000026),
            text: c(0x1D1C1AFF),
            text_soft: c(0x1D1C1AA8),
            text_faint: c(0x1D1C1A66),
            accent: c(0xAD6500FF),
            accent_soft: c(0xAD650024),
            on_accent: c(0xFFFFFFFF),
            danger: c(0xC6392FFF),
            danger_soft: c(0xC6392F1F),
            warning: c(0x9A6200FF),
            warning_soft: c(0x9A62001F),
            success: c(0x2B7A4BFF),
            success_soft: c(0x2B7A4B1F),
            paper: c(0xFFFFFFFF),
            paper_shadow: c(0x00000024),
            paper_dim: c(0x00000000),
            selection: c(0xAD650033),
            line_highlight: c(0x00000008),
            caret: c(0xAD6500FF),
            radius_s: px(6.),
            radius_m: px(10.),
            radius_l: px(16.),
            ui_font: ".SystemUIFont".into(),
            mono_font: "Menlo".into(),
            serif_font: "Charter".into(),
        }
    }
}

/// The global behind `cx.tokens()`. Observe it
/// (`cx.observe_global::<OylTheme>`) to react to a light/dark switch.
pub struct OylTheme {
    pub appearance: Appearance,
    pub tokens: Tokens,
}

impl Global for OylTheme {}

/// `cx.tokens()` for `App` (and through deref, for `Context<T>`).
pub trait ActiveTokens {
    fn tokens(&self) -> &Tokens;
}

impl ActiveTokens for App {
    fn tokens(&self) -> &Tokens {
        &self.global::<OylTheme>().tokens
    }
}

/// Installs Oyl's look. Call once, after `gpui_kit::init(cx)`.
pub fn install(appearance: Appearance, cx: &mut App) {
    apply(appearance, cx);
}

/// The user's current choice.
pub fn appearance(cx: &App) -> Appearance {
    cx.global::<OylTheme>().appearance
}

/// Switches light/dark. Every window repaints.
pub fn set_appearance(appearance: Appearance, cx: &mut App) {
    apply(appearance, cx);
}

/// Flips between light and dark and returns the new choice (to persist).
pub fn toggle(cx: &mut App) -> Appearance {
    let next = appearance(cx).toggled(cx);
    apply(next, cx);
    next
}

/// Re-resolves `Appearance::System` after macOS switched its appearance.
/// Call from `window.observe_window_appearance`.
pub fn system_appearance_changed(_window: &mut Window, cx: &mut App) {
    let current = appearance(cx);
    if current == Appearance::System {
        apply(current, cx);
    }
}

fn apply(appearance: Appearance, cx: &mut App) {
    let dark = appearance.is_dark(cx);
    let tokens = if dark {
        Tokens::dark()
    } else {
        Tokens::light()
    };

    // Project the tokens onto the component library's theme, so its
    // buttons, inputs, menus and scrollbars belong to the same glass.
    Theme::change(
        if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        None,
        cx,
    );
    let k = tokens.clone();
    Theme::update(cx, move |theme| {
        let clear = Hsla::transparent_black();
        let t = &mut theme.colors;

        t.background = k.veil;
        t.foreground = k.text;
        t.border = k.hairline;
        t.input = k.hairline_strong;
        t.ring = k.accent.opacity(0.55);
        t.caret = k.caret;
        t.selection = k.selection;

        t.muted = k.surface;
        t.muted_foreground = k.text_soft;
        t.accent = k.surface_hover;
        t.accent_foreground = k.text;

        t.popover = k.raised;
        t.popover_foreground = k.text;

        t.primary = k.accent;
        t.primary_hover = k.accent.opacity(0.88);
        t.primary_active = k.accent.opacity(0.76);
        t.primary_foreground = k.on_accent;

        t.secondary = k.surface;
        t.secondary_hover = k.surface_hover;
        t.secondary_active = k.surface_active;
        t.secondary_foreground = k.text;

        t.danger = k.danger;
        t.danger_hover = k.danger.opacity(0.88);
        t.danger_active = k.danger.opacity(0.76);
        t.warning = k.warning;
        t.success = k.success;

        t.link = k.accent;
        t.link_hover = k.accent.opacity(0.85);
        t.link_active = k.accent.opacity(0.7);

        t.list = clear;
        t.list_even = clear;
        t.list_head = clear;
        t.list_hover = k.surface_hover;
        t.list_active = k.accent_soft;
        t.list_active_border = k.accent.opacity(0.5);

        t.tab = clear;
        t.tab_bar = clear;
        t.tab_bar_segmented = k.surface;
        t.tab_active = k.surface_active;
        t.tab_active_foreground = k.text;
        t.tab_foreground = k.text_soft;

        t.title_bar = clear;
        t.title_bar_border = clear;
        t.status_bar = clear;
        t.status_bar_border = clear;

        t.scrollbar = clear;
        t.scrollbar_thumb = k.text.opacity(0.16);
        t.scrollbar_thumb_hover = k.text.opacity(0.30);

        t.skeleton = k.surface_hover;
        t.overlay = if k.dark {
            c(0x00000066)
        } else {
            c(0x1D1C1A2E)
        };
        t.drop_target = k.accent_soft;
        t.drag_border = k.accent;
        t.window_border = k.hairline_strong;

        theme.radius = k.radius_m;
        theme.radius_lg = k.radius_l;
        theme.mono_font_family = k.mono_font.clone();
    });

    cx.set_global(OylTheme { appearance, tokens });
    cx.refresh_windows();
}
