//! The few shapes everything in Oyl is built from.
//!
//! Each returns a plain `Div` already carrying its fill, edge and radius;
//! callers add layout and children.

use gpui_kit::component::StyledExt as _;
use gpui_kit::prelude::*;
use gpui_kit::{App, Div, Hsla, SharedString, div, px};

use crate::theme::ActiveTokens as _;

/// A resting glass surface: cards, panels that float over the veil.
pub fn surface(cx: &App) -> Div {
    let k = cx.tokens();
    div()
        .bg(k.surface)
        .border_1()
        .border_color(k.hairline)
        .rounded(k.radius_l)
}

/// The card a working surface sits in: the source editor, the agent thread.
/// Darker than the veil in dark mode so text reads calmly on it.
pub fn card(cx: &App) -> Div {
    let k = cx.tokens();
    div()
        .bg(k.card)
        .border_1()
        .border_color(k.card_edge)
        .rounded(k.radius_l)
}

/// A [`card`] that is fully opaque, for a card that slides over the paper
/// (the agent): nothing underneath shows through, and it casts a shadow.
pub fn solid_card(cx: &App) -> Div {
    let k = cx.tokens();
    div()
        .bg(k.card_solid)
        .border_1()
        .border_color(k.card_edge)
        .rounded(k.radius_l)
        .shadow_lg()
}

/// A nearly opaque floating layer for what overlaps content: the command
/// palette, menus, popovers, dialogs.
pub fn raised(cx: &App) -> Div {
    let k = cx.tokens();
    div()
        .bg(k.raised)
        .border_1()
        .border_color(k.hairline_strong)
        .rounded(k.radius_l)
        .shadow_xl()
}

/// A floating pill bar: the island at the top of the window, the preview's
/// zoom control. Lays its children out in a row.
pub fn island(cx: &App) -> Div {
    let k = cx.tokens();
    div()
        .h_flex()
        .items_center()
        .gap_1()
        .h(px(36.))
        .px(px(6.))
        .bg(k.surface)
        .border_1()
        .border_color(k.hairline)
        .rounded_full()
}

/// A small rounded label: engine badge, counters, cite keys.
pub fn chip(label: impl Into<SharedString>, cx: &App) -> Div {
    let k = cx.tokens();
    div()
        .h_flex()
        .items_center()
        .h(px(20.))
        .px(px(7.))
        .rounded(k.radius_s)
        .bg(k.surface_hover)
        .text_xs()
        .text_color(k.text_soft)
        .child(label.into())
}

/// A chip in the accent color, for the one thing that should be noticed.
pub fn accent_chip(label: impl Into<SharedString>, cx: &App) -> Div {
    let k = cx.tokens();
    chip(label, cx).bg(k.accent_soft).text_color(k.accent)
}

/// A tiny uppercase caption above a group ("SOURCES", "HISTORY").
pub fn eyebrow(label: impl Into<SharedString>, cx: &App) -> Div {
    let k = cx.tokens();
    let label: SharedString = label.into();
    div()
        .text_xs()
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(k.text_faint)
        .child(SharedString::from(label.to_uppercase()))
}

/// A 1px horizontal rule.
pub fn hairline(cx: &App) -> Div {
    div().h(px(1.)).w_full().bg(cx.tokens().hairline)
}

/// A 6px status dot.
pub fn dot(color: Hsla) -> Div {
    div().size(px(6.)).rounded_full().bg(color)
}
