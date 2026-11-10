//! [`ChatSidebar`]: the threads of the paper beside the agent — a new
//! thread, the ones running (live), the recent ones.

use std::time::Duration;

use chrono::{DateTime, Utc};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::{Icon, IconName, Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnimationExt as _, AnyElement, App, ClickEvent, Context, Entity, FocusHandle, Focusable,
    MouseButton, MouseDownEvent, Pixels, Point, PromptLevel, Render, SharedString, Subscription,
    Task, Window, anchored, deferred, div, px,
};
use oyl_ui::prelude::*;
use oyl_ui::{OylTheme, Tokens};

use crate::pane::{ChatPane, RunningThread, ThreadsOverview};
use crate::store::ThreadSummary;

/// Width of the sidebar.
pub const SIDEBAR_WIDTH: f32 = 232.;

/// "now", "2m", "3h", "yesterday", "4d", then the date.
pub fn short_ago(then: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (now - then).num_seconds().max(0);
    if seconds < 60 {
        "now".to_string()
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else if seconds < 2 * 86_400 {
        "yesterday".to_string()
    } else if seconds < 7 * 86_400 {
        format!("{}d", seconds / 86_400)
    } else {
        then.format("%b %-d").to_string()
    }
}

/// How long a turn has run: "12s", "1:05", "1:02:09".
pub fn elapsed(since: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (now - since).num_seconds().max(0);
    let (h, m, s) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else if m > 0 {
        format!("{m}:{s:02}")
    } else {
        format!("{s}s")
    }
}

/// The open "…" menu of a recent thread.
struct RowMenu {
    id: String,
    at: Point<Pixels>,
}

/// A thread being renamed in place.
struct Renaming {
    id: String,
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// The threads of the paper, beside the agent's card in the Agent tab.
pub struct ChatSidebar {
    pane: Entity<ChatPane>,
    menu: Option<RowMenu>,
    renaming: Option<Renaming>,
    hovered: Option<String>,
    /// Redraws the elapsed times every second while a turn runs.
    tick: Option<Task<()>>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for ChatSidebar {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ChatSidebar {
    /// The sidebar of `pane`'s threads.
    pub fn new(pane: Entity<ChatPane>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.observe(&pane, |this, _, cx| {
                this.keep_ticking(cx);
                cx.notify();
            }),
            cx.observe_global::<OylTheme>(|_, cx| cx.notify()),
        ];
        Self {
            pane,
            menu: None,
            renaming: None,
            hovered: None,
            tick: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// The threads as the sidebar lists them.
    pub fn overview(&self, cx: &App) -> ThreadsOverview {
        self.pane.read(cx).threads_overview()
    }

    fn keep_ticking(&mut self, cx: &mut Context<Self>) {
        if self.tick.is_some() || !self.pane.read(cx).is_running() {
            return;
        }
        self.tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let running = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        this.pane.read(cx).is_running()
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
            let _ = this.update(cx, |this, _| this.tick = None);
        }));
    }

    fn new_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pane
            .update(cx, |pane, cx| pane.new_thread(window, cx));
    }

    fn show(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let id = id.to_string();
        self.pane
            .update(cx, |pane, cx| pane.show_thread(&id, window, cx));
    }

    fn stop(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = id.to_string();
        self.pane.update(cx, |pane, cx| pane.stop_thread(&id, cx));
    }

    fn open_menu(&mut self, id: &str, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.menu = Some(RowMenu {
            id: id.to_string(),
            at,
        });
        cx.notify();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    /// Turns a row into a text field with the thread's title.
    pub fn start_rename(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let title = self
            .overview(cx)
            .recent
            .iter()
            .find(|summary| summary.id == id)
            .map(|summary| summary.title.clone())
            .unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } | InputEvent::Blur => this.commit_rename(window, cx),
                InputEvent::Change | InputEvent::Focus => {}
            },
        );
        input.update(cx, |input, cx| input.focus(window, cx));
        self.renaming = Some(Renaming {
            id: id.to_string(),
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn commit_rename(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = self.renaming.take() else {
            return;
        };
        let title = renaming.input.read(cx).value().to_string();
        self.pane
            .update(cx, |pane, cx| pane.rename_thread(&renaming.id, &title, cx));
        cx.notify();
    }

    fn on_escape(&mut self, _: &Escape, _: &mut Window, cx: &mut Context<Self>) {
        if self.renaming.take().is_some() || self.menu.take().is_some() {
            cx.notify();
        } else {
            cx.propagate();
        }
    }

    /// Asks, then deletes the thread.
    pub fn delete_with_confirmation(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let title = self
            .overview(cx)
            .recent
            .iter()
            .find(|summary| summary.id == id)
            .map(|summary| summary.title.clone())
            .unwrap_or_else(|| "this thread".to_string());
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Delete “{title}”?"),
            Some("The conversation is removed from this paper. The paper's files are not changed."),
            &["Delete", "Cancel"],
            cx,
        );
        let pane = self.pane.clone();
        let id = id.to_string();
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(0) {
                cx.update(|cx| pane.update(cx, |pane, cx| pane.delete_thread(&id, cx)));
            }
        })
        .detach();
        cx.notify();
    }

    // -- Drawing -------------------------------------------------------------

    fn row(&self, id: SharedString, current: bool, k: &Tokens) -> gpui_kit::Stateful<gpui_kit::Div> {
        let hover = k.surface_hover;
        div()
            .id(id)
            .h_flex()
            .gap(px(8.))
            .px(px(10.))
            .py(px(8.))
            .rounded(k.radius_m)
            .cursor_pointer()
            .when(current, |this| this.bg(k.accent_soft))
            .when(!current, |this| this.hover(move |style| style.bg(hover)))
    }

    fn render_new(&self, overview: &ThreadsOverview, k: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        self.row("chat-sidebar-new".into(), overview.current_is_new, k)
            .child(Icon::new(IconName::Plus).size(px(14.)).text_color(k.text_soft))
            .child(div().flex_1().text_sm().text_color(k.text).child("New thread"))
            .child(div().text_xs().text_color(k.text_faint).child("⌘N"))
            .on_click(cx.listener(|this, _, window, cx| this.new_thread(window, cx)))
            .into_any_element()
    }

    fn render_running(
        &self,
        index: usize,
        thread: &RunningThread,
        overview: &ThreadsOverview,
        now: DateTime<Utc>,
        k: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = thread.id.clone();
        let hovered = self.hovered.as_deref() == Some(thread.id.as_str());
        let dot = oyl_ui::dot(k.accent).with_animation(
            ("chat-sidebar-pulse", index),
            gpui_kit::Animation::new(Duration::from_millis(1400)).repeat(),
            |dot, delta| dot.opacity(pulse(delta)),
        );
        let show = id.clone();
        let hover_id = id.clone();
        let stop = id.clone();
        let strong = k.text;
        self.row(
            SharedString::from(format!("chat-sidebar-running-{index}")),
            overview.current == thread.id,
            k,
        )
        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
            this.hovered = hovered.then(|| hover_id.clone());
            cx.notify();
        }))
        .on_click(cx.listener(move |this, _, window, cx| this.show(&show, window, cx)))
        .child(div().flex_none().child(dot))
        .child(
            div()
                .v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(1.))
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .text_color(k.text)
                        .child(thread.title.clone()),
                )
                .child(
                    div()
                        .h_flex()
                        .gap(px(6.))
                        .text_xs()
                        .text_color(k.text_faint)
                        .child(div().flex_1().min_w_0().truncate().child(thread.activity.clone()))
                        .child(div().flex_none().child(elapsed(thread.started_at, now))),
                ),
        )
        .when(hovered && !thread.stopping, |this| {
            this.child(
                div()
                    .id(("chat-sidebar-stop", index))
                    .flex_none()
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(k.radius_s)
                    .text_color(k.text_soft)
                    .hover(move |style| style.text_color(strong))
                    .child(Icon::new(IconName::Square).size(px(11.)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.stop(&stop, cx)
                    })),
            )
        })
        .into_any_element()
    }

    fn render_recent(
        &self,
        index: usize,
        summary: &ThreadSummary,
        overview: &ThreadsOverview,
        now: DateTime<Utc>,
        k: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(renaming) = self.renaming.as_ref().filter(|r| r.id == summary.id) {
            return div()
                .px(px(4.))
                .py(px(2.))
                .child(Input::new(&renaming.input).small())
                .into_any_element();
        }
        let id = summary.id.clone();
        let hovered = self.hovered.as_deref() == Some(summary.id.as_str());
        let (show, hover_id, menu_id, right_id) = (id.clone(), id.clone(), id.clone(), id.clone());
        self.row(
            SharedString::from(format!("chat-sidebar-recent-{index}")),
            overview.current == summary.id,
            k,
        )
        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
            this.hovered = hovered.then(|| hover_id.clone());
            cx.notify();
        }))
        .on_click(cx.listener(move |this, _, window, cx| this.show(&show, window, cx)))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.open_menu(&right_id, event.position, cx)
            }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .text_color(k.text)
                .child(summary.title.clone()),
        )
        .map(|this| {
            if hovered {
                this.child(
                    div()
                        .id(("chat-sidebar-more", index))
                        .flex_none()
                        .size(px(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(k.radius_s)
                        .text_color(k.text_soft)
                        .child(Icon::new(IconName::Ellipsis).size(px(13.)))
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.open_menu(&menu_id, event.position(), cx)
                        })),
                )
            } else {
                this.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(k.text_faint)
                        .child(short_ago(summary.updated_at, now)),
                )
            }
        })
        .into_any_element()
    }

    fn render_menu(&self, k: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let hover = k.surface_hover;
        let item = |label: &'static str, danger: bool, k: &Tokens| {
            div()
                .id(label)
                .px(px(10.))
                .py(px(8.))
                .rounded(k.radius_m)
                .cursor_pointer()
                .text_sm()
                .text_color(if danger { k.danger } else { k.text })
                .hover(move |style| style.bg(hover))
                .child(label)
        };
        let (rename, delete) = (menu.id.clone(), menu.id.clone());
        Some(
            deferred(
                anchored()
                    .position(menu.at)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        oyl_ui::raised(cx)
                            .id("chat-sidebar-menu")
                            .occlude()
                            .w(px(160.))
                            .p(px(6.))
                            .v_flex()
                            .font_family(k.ui_font.clone())
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx)))
                            .child(item("Rename", false, k).on_click(cx.listener(
                                move |this, _, window, cx| this.start_rename(&rename, window, cx),
                            )))
                            .child(item("Delete", true, k).on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.delete_with_confirmation(&delete, window, cx)
                                },
                            ))),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}

/// A soft pulse between 0.35 and 1 over one period.
fn pulse(delta: f32) -> f32 {
    let wave = (delta * std::f32::consts::TAU).cos() * 0.5 + 0.5;
    0.35 + 0.65 * wave
}

impl Render for ChatSidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let k = cx.tokens().clone();
        let overview = self.overview(cx);
        let now = Utc::now();
        let mut list: Vec<AnyElement> = Vec::new();
        if !overview.running.is_empty() {
            list.push(oyl_ui::eyebrow("Running", cx).px(px(10.)).pt(px(14.)).pb(px(6.)).into_any_element());
            for (index, thread) in overview.running.iter().enumerate() {
                list.push(self.render_running(index, thread, &overview, now, &k, cx));
            }
        }
        if !overview.recent.is_empty() {
            list.push(oyl_ui::eyebrow("Recent", cx).px(px(10.)).pt(px(14.)).pb(px(6.)).into_any_element());
            for (index, summary) in overview.recent.iter().enumerate() {
                list.push(self.render_recent(index, summary, &overview, now, &k, cx));
            }
        }
        let menu = self.render_menu(&k, cx);
        div()
            .id("chat-sidebar")
            .key_context("ChatSidebar")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_escape))
            .size_full()
            .v_flex()
            .font_family(k.ui_font.clone())
            .text_color(k.text)
            .child(self.render_new(&overview, &k, cx))
            .child(
                div()
                    .id("chat-sidebar-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .v_flex()
                    .children(list),
            )
            .children(menu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    fn at(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + seconds, 0).single().unwrap()
    }

    #[test]
    fn recent_times_are_short() {
        let now = at(0);
        assert_eq!(short_ago(at(-10), now), "now");
        assert_eq!(short_ago(at(-120), now), "2m");
        assert_eq!(short_ago(at(-3 * 3600), now), "3h");
        assert_eq!(short_ago(at(-30 * 3600), now), "yesterday");
        assert_eq!(short_ago(at(-4 * 86_400), now), "4d");
        assert!(!short_ago(at(-30 * 86_400), now).is_empty());
        assert_eq!(short_ago(at(60), now), "now", "a clock skew reads as now");
    }

    #[test]
    fn elapsed_times_count_up() {
        let start = at(0);
        assert_eq!(elapsed(start, at(12)), "12s");
        assert_eq!(elapsed(start, at(65)), "1:05");
        assert_eq!(elapsed(start, at(3729)), "1:02:09");
        assert_eq!(elapsed(start, at(-5)), "0s");
    }
}
