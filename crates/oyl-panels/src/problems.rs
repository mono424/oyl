//! The problems panel: what the compiler reported.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Icon, IconName, Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, EventEmitter, FocusHandle, Focusable, ListAlignment, ListState,
    Pixels, Subscription, Window, div, list, px,
};
use oyl_model::{Diagnostic, Severity, SourceLocation};
use oyl_ui::prelude::*;

use crate::model::problems::{fix_prompt, location, ordered, place, summary};
use crate::ui::{self, PAD_X};
use crate::{Confirm, Dismiss, KEY_CONTEXT, SelectNext, SelectPrev};

/// What the problems panel tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProblemsEvent {
    /// Go to where a problem points.
    Jump(SourceLocation),
    /// Hand the errors to the agent: send this prompt as a turn.
    FixWithAgent {
        /// The errors (file, line, message) and the request to fix them
        /// and keep the paper compiling.
        prompt: String,
    },
    /// The panel asks to be closed (Esc, or its close button).
    Close,
}

/// The compiler's errors (first) and warnings, each with its message,
/// where it points and the compiler's hints; a click or Enter jumps there.
/// "Fix with agent" hands the errors to the agent.
///
/// Feed it with [`set_diagnostics`](Self::set_diagnostics) after every
/// compile; subscribe to [`ProblemsEvent`]s.
pub struct ProblemsPanel {
    focus: FocusHandle,
    diagnostics: Rc<Vec<Diagnostic>>,
    selected: Option<usize>,
    list: ListState,
    _subscriptions: Vec<Subscription>,
}

impl ProblemsPanel {
    /// An empty panel ("The paper compiles.").
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::init(cx);
        ProblemsPanel {
            focus: cx.focus_handle(),
            diagnostics: Rc::new(Vec::new()),
            selected: None,
            list: ListState::new(0, ListAlignment::Top, px(300.)),
            _subscriptions: vec![cx.observe_global::<oyl_ui::OylTheme>(|_, cx| cx.notify())],
        }
    }

    /// The width the card should have.
    pub fn preferred_width(&self) -> Pixels {
        px(420.)
    }

    /// Shows the problems of the latest compile (errors first).
    pub fn set_diagnostics(&mut self, diagnostics: Vec<Diagnostic>, cx: &mut Context<Self>) {
        let keep_scroll = diagnostics.len() == self.diagnostics.len();
        self.diagnostics = Rc::new(ordered(diagnostics));
        self.selected = self
            .selected
            .filter(|&ix| ix < self.diagnostics.len())
            .or_else(|| (!self.diagnostics.is_empty()).then_some(0));
        ui::sync_list(&self.list, self.diagnostics.len(), keep_scroll);
        cx.notify();
    }

    /// The problems shown, in order.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// The prompt "Fix with agent" sends, when there is something to fix.
    pub fn fix_prompt(&self) -> Option<String> {
        fix_prompt(&self.diagnostics)
    }

    fn move_selection(&mut self, direction: isize, cx: &mut Context<Self>) {
        if let Some(ix) =
            ui::step_selection(self.diagnostics.len(), self.selected, direction, |_| true)
        {
            self.selected = Some(ix);
            self.list.scroll_to_reveal_item(ix);
            cx.notify();
        }
    }

    fn jump(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = Some(ix);
        if let Some(location) = self.diagnostics.get(ix).and_then(location) {
            cx.emit(ProblemsEvent::Jump(location));
        }
        cx.notify();
    }

    pub(crate) fn fix(&mut self, cx: &mut Context<Self>) {
        if let Some(prompt) = self.fix_prompt() {
            cx.emit(ProblemsEvent::FixWithAgent { prompt });
        }
    }

    fn render_row(&mut self, ix: usize, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(diagnostic) = self.diagnostics.get(ix).cloned() else {
            return div().into_any_element();
        };
        let k = cx.tokens().clone();
        let (icon, color) = match diagnostic.severity {
            Severity::Error => (IconName::CircleX, k.danger),
            Severity::Warning => (IconName::TriangleAlert, k.warning),
            Severity::Info => (IconName::Info, k.text_faint),
        };
        let jumps = location(&diagnostic).is_some();
        let last = ix + 1 == self.diagnostics.len();
        let row = ui::row(("problem", ix), self.selected == Some(ix), cx)
            .when(!jumps, |row| row.cursor_default())
            .h_flex()
            .items_start()
            .gap(px(9.))
            .child(
                div()
                    .pt(px(2.))
                    .flex_none()
                    .child(Icon::new(icon).small().text_color(color)),
            )
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .line_height(px(18.))
                            .text_color(k.text)
                            .child(diagnostic.message.trim().to_string()),
                    )
                    .children(place(&diagnostic).map(|place| {
                        div()
                            .font_family(k.mono_font.clone())
                            .text_size(px(11.5))
                            .text_color(k.text_faint)
                            .child(place)
                    }))
                    .children(diagnostic.hints.iter().map(|hint| {
                        div()
                            .text_size(px(12.))
                            .line_height(px(16.))
                            .text_color(k.text_soft)
                            .child(format!("Hint: {}", hint.trim()))
                    })),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.jump(ix, cx)));
        div()
            .px(px(PAD_X - 10.))
            .when(last, |d| d.pb(px(16.)))
            .child(row)
            .into_any_element()
    }
}

impl Render for ProblemsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let k = cx.tokens().clone();
        let this = cx.entity().downgrade();
        let count = (!self.diagnostics.is_empty()).then(|| self.diagnostics.len().to_string());
        let mut extras = Vec::new();
        if self.fix_prompt().is_some() {
            extras.push(
                Button::new("problems-fix")
                    .primary()
                    .xsmall()
                    .icon(IconName::Bot)
                    .label("Fix with agent")
                    .on_click(cx.listener(|this, _, _, cx| this.fix(cx)))
                    .into_any_element(),
            );
        }
        let header = ui::header(
            "Problems",
            count,
            extras,
            Rc::new(move |_, cx| {
                this.update(cx, |_, cx| cx.emit(ProblemsEvent::Close)).ok();
            }),
            cx,
        );
        let body = if self.diagnostics.is_empty() {
            ui::empty_state(
                "The paper compiles.",
                Some(
                    Button::new("problems-back")
                        .small()
                        .label("Back to writing")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ProblemsEvent::Close)))
                        .into_any_element(),
                ),
                cx,
            )
            .into_any_element()
        } else {
            div()
                .v_flex()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .flex_none()
                        .px(px(PAD_X))
                        .pb(px(6.))
                        .text_size(px(12.))
                        .text_color(k.text_faint)
                        .child(summary(&self.diagnostics)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .relative()
                        .child(list(self.list.clone(), cx.processor(Self::render_row)).size_full())
                        .vertical_scrollbar(&self.list),
                )
                .into_any_element()
        };
        div()
            .id("oyl-problems")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .v_flex()
            .font_family(k.ui_font.clone())
            .text_color(k.text)
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &Confirm, _, cx| {
                if let Some(ix) = this.selected {
                    this.jump(ix, cx);
                }
            }))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(ProblemsEvent::Close)))
            .child(header)
            .child(body)
    }
}

impl Focusable for ProblemsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<ProblemsEvent> for ProblemsPanel {}
