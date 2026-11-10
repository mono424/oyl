//! The pieces every panel is drawn with: the header, rows, the empty state,
//! inline notices, fields, a two-way switch, list syncing.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{IconName, Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Div, ElementId, Entity, FontWeight, ListOffset, ListState, SharedString,
    Stateful, Window, div, px,
};
use oyl_ui::prelude::*;

/// Horizontal padding of a panel's content.
pub(crate) const PAD_X: f32 = 16.;

/// A click handler shared between elements.
pub(crate) type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// A handler told which option was chosen.
pub(crate) type ChoiceHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// The panel's header: the eyebrow title, an optional count, extra controls
/// and the close button.
pub(crate) fn header(
    title: &str,
    count: Option<String>,
    extras: Vec<AnyElement>,
    on_close: Handler,
    cx: &App,
) -> Div {
    div()
        .h_flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h(px(44.))
        .pl(px(PAD_X))
        .pr(px(10.))
        .child(oyl_ui::eyebrow(title.to_string(), cx))
        .children(count.map(|count| oyl_ui::chip(count, cx)))
        .child(div().flex_1())
        .children(extras)
        .child(
            Button::new("oyl-panel-close")
                .ghost()
                .xsmall()
                .icon(IconName::Close)
                .tooltip("Close · Esc")
                .on_click(move |_, window, cx| on_close(window, cx)),
        )
}

/// The calm state of a panel with nothing to show: one sentence and the
/// primary action.
pub(crate) fn empty_state(sentence: &str, action: Option<AnyElement>, cx: &App) -> Div {
    let k = cx.tokens();
    div()
        .v_flex()
        .flex_1()
        .min_h(px(160.))
        .items_center()
        .justify_center()
        .gap(px(14.))
        .px(px(PAD_X + 16.))
        .pb(px(40.))
        .child(
            div()
                .max_w(px(300.))
                .text_center()
                .font_family(k.serif_font.clone())
                .text_size(px(15.))
                .line_height(px(22.))
                .text_color(k.text_soft)
                .child(sentence.to_string()),
        )
        .children(action)
}

/// The tone of an inline notice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    /// Neutral information ("Looking up…", "Added").
    Quiet,
    /// Something went wrong.
    Error,
    /// Something to be careful about.
    Warning,
}

/// A short message inside the panel, in plain words.
pub(crate) fn notice(tone: Tone, text: impl Into<SharedString>, cx: &App) -> Div {
    let k = cx.tokens();
    let (bg, fg) = match tone {
        Tone::Quiet => (k.surface, k.text_soft),
        Tone::Error => (k.danger_soft, k.danger),
        Tone::Warning => (k.warning_soft, k.warning),
    };
    div()
        .px(px(10.))
        .py(px(7.))
        .rounded(k.radius_m)
        .bg(bg)
        .text_size(px(12.5))
        .line_height(px(18.))
        .text_color(fg)
        .child(text.into())
}

/// A selectable row: hover and selection backgrounds, rounded.
pub(crate) fn row(id: impl Into<ElementId>, selected: bool, cx: &App) -> Stateful<Div> {
    let k = cx.tokens();
    let hover = k.surface_hover;
    div()
        .id(id)
        .w_full()
        .px(px(10.))
        .py(px(7.))
        .rounded(k.radius_m)
        .cursor_pointer()
        .when(selected, |row| row.bg(k.accent_soft))
        .when(!selected, |row| row.hover(move |style| style.bg(hover)))
}

/// A caption above a group of rows, inside a list.
pub(crate) fn group_caption(label: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .px(px(10.))
        .pt(px(12.))
        .pb(px(4.))
        .child(oyl_ui::eyebrow(label, cx))
}

/// A labelled field of a form.
pub(crate) fn field(label: &str, input: &Entity<InputState>, cx: &App) -> Div {
    let k = cx.tokens();
    div()
        .v_flex()
        .gap(px(4.))
        .min_w_0()
        .child(
            div()
                .text_size(px(11.5))
                .text_color(k.text_faint)
                .child(label.to_string()),
        )
        .child(Input::new(input).small())
}

/// A two-way (or more) switch: the chosen option on a raised pill.
pub(crate) fn segmented(
    id: &'static str,
    options: &[&str],
    chosen: usize,
    on_choose: ChoiceHandler,
    cx: &App,
) -> Div {
    let k = cx.tokens();
    div()
        .h_flex()
        .p(px(2.))
        .gap(px(2.))
        .rounded(k.radius_m)
        .bg(k.surface)
        .border_1()
        .border_color(k.hairline)
        .children(options.iter().enumerate().map(|(ix, label)| {
            let on_choose = on_choose.clone();
            let active = ix == chosen;
            let hover = k.surface_hover;
            div()
                .id((id, ix))
                .h(px(22.))
                .px(px(9.))
                .h_flex()
                .items_center()
                .rounded(k.radius_s)
                .text_size(px(12.))
                .cursor_pointer()
                .when(active, |pill| {
                    pill.bg(k.surface_active)
                        .text_color(k.text)
                        .font_weight(FontWeight::MEDIUM)
                })
                .when(!active, |pill| {
                    pill.text_color(k.text_soft)
                        .hover(move |style| style.bg(hover))
                })
                .child(label.to_string())
                .on_click(move |_, window, cx| on_choose(ix, window, cx))
        }))
}

/// Points a list at a new number of items. With `keep_scroll` the list
/// stays where it was (as far as the new items allow); otherwise it goes
/// back to the top.
pub(crate) fn sync_list(list: &ListState, count: usize, keep_scroll: bool) {
    let top = list.logical_scroll_top();
    list.reset(count);
    if keep_scroll && count > 0 {
        list.scroll_to(ListOffset {
            item_ix: top.item_ix.min(count - 1),
            offset_in_item: if top.item_ix < count {
                top.offset_in_item
            } else {
                px(0.)
            },
        });
    }
}

/// The next selectable index from `current` in `direction` (+1 / −1),
/// skipping what `selectable` refuses. From nothing, down starts at the
/// first and up at the last.
pub(crate) fn step_selection(
    count: usize,
    current: Option<usize>,
    direction: isize,
    selectable: impl Fn(usize) -> bool,
) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let mut ix: isize = match current {
        Some(ix) => ix as isize + direction,
        None if direction > 0 => 0,
        None => count as isize - 1,
    };
    while (0..count as isize).contains(&ix) {
        if selectable(ix as usize) {
            return Some(ix as usize);
        }
        ix += direction;
    }
    current.filter(|&ix| ix < count && selectable(ix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_steps_over_what_cannot_be_selected() {
        let selectable = |ix: usize| ix != 0 && ix != 3;
        assert_eq!(step_selection(6, None, 1, selectable), Some(1));
        assert_eq!(step_selection(6, Some(2), 1, selectable), Some(4));
        assert_eq!(step_selection(6, Some(4), -1, selectable), Some(2));
        assert_eq!(step_selection(6, Some(1), -1, selectable), Some(1));
        assert_eq!(step_selection(6, Some(5), 1, selectable), Some(5));
        assert_eq!(step_selection(6, None, -1, selectable), Some(5));
        assert_eq!(step_selection(0, None, 1, selectable), None);
        assert_eq!(step_selection(2, None, 1, |_| false), None);
    }
}
