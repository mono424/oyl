//! The outline panel: the paper's headings.

use std::rc::Rc;

use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Icon, IconName, Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, ListAlignment,
    ListState, Pixels, Subscription, Window, div, list, px,
};
use oyl_model::{OutlineItem, SourceLocation};
use oyl_ui::prelude::*;

use crate::model::file_name;
use crate::model::outline::{current_index, filter_items, spans_files};
use crate::ui::{self, PAD_X};
use crate::{Confirm, Dismiss, KEY_CONTEXT, SelectNext, SelectPrev};

/// What the outline panel tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutlineEvent {
    /// Go to this heading in the source.
    Jump(SourceLocation),
    /// The panel asks to be closed (Esc, or its close button).
    Close,
}

/// The headings of the paper, indented by level, with the section the
/// cursor is in marked. Typing filters them; ↑ ↓ and Enter (or a click)
/// jump.
///
/// Feed it with [`set_items`](Self::set_items) and
/// [`set_current`](Self::set_current); subscribe to [`OutlineEvent`]s.
pub struct OutlinePanel {
    focus: FocusHandle,
    filter_input: Entity<InputState>,
    items: Vec<OutlineItem>,
    shown: Rc<Vec<usize>>,
    many_files: bool,
    current: Option<usize>,
    /// The selected heading (index into `items`).
    selected: Option<usize>,
    list: ListState,
    _subscriptions: Vec<Subscription>,
}

impl OutlinePanel {
    /// An empty outline.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::init(cx);
        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter headings"));
        let subscriptions = vec![
            cx.subscribe(&filter_input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.refilter(cx);
                }
            }),
            cx.observe_global::<oyl_ui::OylTheme>(|_, cx| cx.notify()),
        ];
        OutlinePanel {
            focus: cx.focus_handle(),
            filter_input,
            items: Vec::new(),
            shown: Rc::new(Vec::new()),
            many_files: false,
            current: None,
            selected: None,
            list: ListState::new(0, ListAlignment::Top, px(300.)),
            _subscriptions: subscriptions,
        }
    }

    /// The width the card should have.
    pub fn preferred_width(&self) -> Pixels {
        px(420.)
    }

    /// Shows these headings (in document order).
    pub fn set_items(&mut self, items: Vec<OutlineItem>, cx: &mut Context<Self>) {
        let selected = self
            .selected
            .and_then(|ix| self.items.get(ix))
            .map(|item| item.location.clone());
        let same_count = items.len() == self.items.len();
        self.many_files = spans_files(&items);
        self.items = items;
        self.selected = selected
            .and_then(|loc| self.items.iter().position(|item| item.location == loc))
            .or_else(|| self.current.filter(|&ix| ix < self.items.len()));
        if self.current.is_some_and(|ix| ix >= self.items.len()) {
            self.current = None;
        }
        self.recompute(same_count, cx);
    }

    /// Marks the section holding the cursor (`None`: no cursor in the
    /// paper's files), and selects it.
    pub fn set_current(&mut self, at: Option<SourceLocation>, cx: &mut Context<Self>) {
        let current = at.and_then(|at| current_index(&self.items, &at));
        if current == self.current {
            return;
        }
        self.current = current;
        if let Some(ix) = current {
            self.selected = Some(ix);
            if let Some(row) = self.shown.iter().position(|&i| i == ix) {
                self.list.scroll_to_reveal_item(row);
            }
        }
        cx.notify();
    }

    /// The headings shown (after filtering), as indices into the items.
    pub fn shown(&self) -> Vec<usize> {
        self.shown.to_vec()
    }

    /// The heading the cursor is in.
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// The selected heading.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        self.recompute(false, cx);
        if !self.filter_input.read(cx).value().trim().is_empty() {
            self.selected = self.shown.first().copied();
        }
        cx.notify();
    }

    fn recompute(&mut self, keep_scroll: bool, cx: &mut Context<Self>) {
        let query = self.filter_input.read(cx).value().to_string();
        self.shown = Rc::new(filter_items(&self.items, &query));
        ui::sync_list(&self.list, self.shown.len(), keep_scroll);
        cx.notify();
    }

    fn move_selection(&mut self, direction: isize, cx: &mut Context<Self>) {
        let current = self
            .selected
            .and_then(|ix| self.shown.iter().position(|&i| i == ix));
        if let Some(row) = ui::step_selection(self.shown.len(), current, direction, |_| true) {
            self.selected = Some(self.shown[row]);
            self.list.scroll_to_reveal_item(row);
            cx.notify();
        }
    }

    fn jump(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(item) = self.items.get(ix) {
            self.selected = Some(ix);
            cx.emit(OutlineEvent::Jump(item.location.clone()));
            cx.notify();
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let target = self
            .selected
            .filter(|ix| self.shown.contains(ix))
            .or_else(|| self.shown.first().copied());
        if let Some(ix) = target {
            self.jump(ix, cx);
        }
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.filter_input.read(cx).value().is_empty() {
            self.filter_input
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.recompute(false, cx);
            return;
        }
        cx.emit(OutlineEvent::Close);
    }

    fn render_row(&mut self, row: usize, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(&ix) = self.shown.get(row) else {
            return div().into_any_element();
        };
        let Some(item) = self.items.get(ix) else {
            return div().into_any_element();
        };
        let k = cx.tokens().clone();
        let is_current = self.current == Some(ix);
        let selected = self.selected == Some(ix);
        let indent = (item.level.max(1) - 1).min(5) as f32 * 14.;
        let top = item.level <= 1;
        let place = if self.many_files {
            format!("{}:{}", file_name(&item.location.file), item.location.line)
        } else {
            item.location.line.to_string()
        };
        let last = row + 1 == self.shown.len();
        div()
            .px(px(PAD_X - 10.))
            .when(last, |d| d.pb(px(16.)))
            .child(
                ui::row(("heading", ix), selected, cx)
                    .py(px(5.))
                    .h_flex()
                    .gap(px(8.))
                    .child(div().w(px(indent)).flex_none())
                    .child(
                        div()
                            .w(px(3.))
                            .h(px(14.))
                            .flex_none()
                            .rounded_full()
                            .when(is_current, |bar| bar.bg(k.accent)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(if top { 13.5 } else { 13. }))
                            .when(top, |t| t.font_weight(FontWeight::MEDIUM))
                            .text_color(if is_current {
                                k.accent
                            } else if top {
                                k.text
                            } else {
                                k.text_soft
                            })
                            .child(item.title.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.5))
                            .text_color(k.text_faint)
                            .child(place),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.jump(ix, cx))),
            )
            .into_any_element()
    }
}

impl Render for OutlinePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let k = cx.tokens().clone();
        let this = cx.entity().downgrade();
        let count = (!self.items.is_empty()).then(|| self.items.len().to_string());
        let header = ui::header(
            "Outline",
            count,
            Vec::new(),
            Rc::new(move |_, cx| {
                this.update(cx, |_, cx| cx.emit(OutlineEvent::Close)).ok();
            }),
            cx,
        );
        let body = if self.items.is_empty() {
            ui::empty_state(
                "No headings yet. They appear here as you write them.",
                Some(
                    Button::new("outline-back")
                        .small()
                        .label("Back to writing")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(OutlineEvent::Close)))
                        .into_any_element(),
                ),
                cx,
            )
            .into_any_element()
        } else if self.shown.is_empty() {
            div()
                .px(px(PAD_X))
                .py(px(16.))
                .text_size(px(13.))
                .text_color(k.text_faint)
                .child(format!(
                    "No heading matches “{}”.",
                    self.filter_input.read(cx).value()
                ))
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .relative()
                .child(list(self.list.clone(), cx.processor(Self::render_row)).size_full())
                .vertical_scrollbar(&self.list)
                .into_any_element()
        };
        div()
            .id("oyl-outline")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .v_flex()
            .font_family(k.ui_font.clone())
            .text_color(k.text)
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &Confirm, _, cx| this.confirm(cx)))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)))
            .child(header)
            .when(!self.items.is_empty(), |panel| {
                panel.child(
                    div().flex_none().px(px(PAD_X)).pb(px(6.)).child(
                        Input::new(&self.filter_input)
                            .small()
                            .prefix(Icon::new(IconName::Search).small().text_color(k.text_faint))
                            .cleanable(true),
                    ),
                )
            })
            .child(body)
    }
}

impl Focusable for OutlinePanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.items.is_empty() {
            self.focus.clone()
        } else {
            self.filter_input.focus_handle(cx)
        }
    }
}

impl EventEmitter<OutlineEvent> for OutlinePanel {}
