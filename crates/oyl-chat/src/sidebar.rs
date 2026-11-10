//! [`ChatSidebar`]: the threads of the paper beside the agent — a new
//! thread, every thread (newest activity first), and at the bottom a quiet
//! button for the agents working in the background.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::{Icon, IconName, Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, Bounds, ClickEvent, Context, Entity,
    FocusHandle, Focusable, MouseButton, MouseDownEvent, Pixels, Point, PromptLevel, Render,
    SharedString, Subscription, Task, Window, anchored, canvas, deferred, div, px,
};
use oyl_ui::prelude::*;
use oyl_ui::{OylTheme, Tokens};

use crate::pane::{ChatPane, ClosePopover, RunningThread, ThreadsOverview};
use crate::store::ThreadSummary;
use oyl_tasks::{PulseDot, TaskRow, TaskState, TaskSummary, TrayButton};
use std::time::SystemTime;

/// Width of the sidebar.
pub const SIDEBAR_WIDTH: f32 = 232.;
/// The key context of the tray of background agents.
pub(crate) const TRAY_CONTEXT: &str = "ChatTray";
/// Height of the "N agents running" button.
const TRAY_BUTTON_HEIGHT: f32 = 30.;
/// Space between the button and the list above it, and the tray.
const TRAY_GAP: f32 = 6.;
/// The tray grows to at most this share of the sidebar's height.
const TRAY_MAX_SHARE: f32 = 0.6;
/// Inner padding of the tray.
const TRAY_PADDING: f32 = 4.;
/// How long the tray takes to open.
const TRAY_OPEN: Duration = Duration::from_millis(180);

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

/// The tray button's words ("2 agents running", "1 done", as
/// [`oyl_tasks::tray_label`] says), or nothing: the button is hidden.
pub fn tray_label(running: usize, finished: usize) -> Option<String> {
    (running + finished > 0).then(|| oyl_tasks::tray_label(running, finished))
}

/// Where each tray item was laid out, by thread id.
type ItemBounds = Rc<RefCell<Vec<(String, Bounds<Pixels>)>>>;

/// One item of the tray, as [`ChatSidebar::tray_items`] reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayItem {
    /// The thread id.
    pub id: String,
    /// The thread's title.
    pub title: String,
    /// The live activity ("Searching: …") or how it ended ("Done · …").
    pub line: String,
    /// Whether its turn still runs.
    pub running: bool,
}

/// The tray's items: the turns running (newest first), then the ones that
/// ended unseen (newest first).
fn tray_items(overview: &ThreadsOverview) -> Vec<TrayItem> {
    let mut running: Vec<&RunningThread> = overview.running.iter().collect();
    running.sort_by_key(|thread| std::cmp::Reverse(thread.started_at));
    let running = running.into_iter().map(|thread| TrayItem {
        id: thread.id.clone(),
        title: thread.title.clone(),
        line: thread.activity.clone(),
        running: true,
    });
    let finished = overview.finished.iter().rev().map(|thread| TrayItem {
        id: thread.id.clone(),
        title: thread.title.clone(),
        line: thread.outcome.clone(),
        running: false,
    });
    running.chain(finished).collect()
}

/// The open "…" menu of a thread.
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
    /// The tray of background agents is open; counts the openings (each
    /// one animates anew).
    tray_open: bool,
    tray_opened: usize,
    tray_hovered: Option<String>,
    tray_focus: FocusHandle,
    /// Where the tray button is, as last laid out.
    tray_button: Rc<Cell<Bounds<Pixels>>>,
    /// Where each tray item is, as last laid out.
    tray_item_bounds: ItemBounds,
    /// The sidebar's height, as last laid out (the tray's limit follows).
    height: Rc<Cell<Pixels>>,
    /// Where the "New discovery" row is, as last laid out.
    discovery_bounds: Rc<Cell<Bounds<Pixels>>>,
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
            cx.observe(&pane, |this, pane, cx| {
                this.keep_ticking(cx);
                // Nothing left to show: the tray closes.
                let overview = pane.read(cx).threads_overview();
                if overview.running.is_empty() && overview.finished.is_empty() {
                    this.tray_open = false;
                }
                cx.notify();
            }),
            cx.observe_global::<OylTheme>(|_, cx| cx.notify()),
        ];
        Self {
            pane,
            menu: None,
            renaming: None,
            hovered: None,
            tray_open: false,
            tray_opened: 0,
            tray_hovered: None,
            tray_focus: cx.focus_handle(),
            tray_button: Rc::default(),
            tray_item_bounds: Rc::default(),
            height: Rc::default(),
            discovery_bounds: Rc::default(),
            tick: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// The threads as the sidebar lists them.
    pub fn overview(&self, cx: &App) -> ThreadsOverview {
        self.pane.read(cx).threads_overview()
    }

    /// What the tray button says (`None`: it is hidden).
    pub fn tray_label(&self, cx: &App) -> Option<String> {
        let overview = self.overview(cx);
        tray_label(overview.running.len(), overview.finished.len())
    }

    /// The tray's items, open or not.
    pub fn tray_items(&self, cx: &App) -> Vec<TrayItem> {
        tray_items(&self.overview(cx))
    }

    /// Whether the tray is open.
    pub fn is_tray_open(&self) -> bool {
        self.tray_open
    }

    /// Where the tray button is, as last laid out (window coordinates).
    pub fn tray_button_bounds(&self) -> Bounds<Pixels> {
        self.tray_button.get()
    }

    /// Where the tray item of thread `id` is, as last laid out.
    pub fn tray_item_bounds(&self, id: &str) -> Option<Bounds<Pixels>> {
        self.tray_item_bounds
            .borrow()
            .iter()
            .find(|(item, _)| item == id)
            .map(|(_, bounds)| *bounds)
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
        self.pane.update(cx, |pane, cx| pane.new_thread(window, cx));
    }

    fn new_discovery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pane
            .update(cx, |pane, cx| pane.new_discovery(window, cx));
    }

    /// Where the "New discovery" row is, as last laid out (window
    /// coordinates); zero-sized when no harness offers discovery.
    pub fn discovery_row_bounds(&self) -> Bounds<Pixels> {
        self.discovery_bounds.get()
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

    fn dismiss(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = id.to_string();
        self.pane
            .update(cx, |pane, cx| pane.dismiss_finished(&id, cx));
    }

    // -- The tray ------------------------------------------------------------

    /// Opens the tray, or closes it when it is open.
    pub fn toggle_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tray_open {
            self.close_tray(window, cx);
            return;
        }
        self.menu = None;
        self.tray_open = true;
        self.tray_opened += 1;
        self.tray_focus.focus(window, cx);
        cx.notify();
    }

    fn close_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tray_open {
            return;
        }
        self.tray_open = false;
        self.tray_hovered = None;
        if self.tray_focus.is_focused(window) {
            self.pane
                .update(cx, |pane, cx| pane.focus_composer(window, cx));
        }
        cx.notify();
    }

    /// Shows the thread of a tray item and closes the tray.
    fn open_from_tray(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.tray_open = false;
        self.tray_hovered = None;
        self.show(id, window, cx);
        cx.notify();
    }

    fn on_close_tray(&mut self, _: &ClosePopover, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tray(window, cx);
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    fn open_menu(&mut self, id: &str, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.menu = Some(RowMenu {
            id: id.to_string(),
            at,
        });
        cx.notify();
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
    pub fn delete_with_confirmation(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    fn row(
        &self,
        id: SharedString,
        current: bool,
        k: &Tokens,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
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

    fn render_new(
        &self,
        overview: &ThreadsOverview,
        k: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = overview.current_is_new && !overview.current_is_discovery;
        self.row("chat-sidebar-new".into(), current, k)
            .child(
                Icon::new(IconName::Plus)
                    .size(px(14.))
                    .text_color(k.text_soft),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(k.text)
                    .child("New thread"),
            )
            .child(div().text_xs().text_color(k.text_faint).child("⌘N"))
            .on_click(cx.listener(|this, _, window, cx| this.new_thread(window, cx)))
            .into_any_element()
    }

    /// "New discovery": a fresh thread in Discover mode, first class.
    fn render_discovery(
        &self,
        overview: &ThreadsOverview,
        k: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !overview.discovery_available {
            self.discovery_bounds.set(Bounds::default());
            return None;
        }
        let current = overview.current_is_new && overview.current_is_discovery;
        let bounds = self.discovery_bounds.clone();
        Some(
            self.row("chat-sidebar-discovery".into(), current, k)
                .relative()
                .child(
                    div()
                        .flex_none()
                        .w(px(14.))
                        .flex()
                        .justify_center()
                        .text_sm()
                        .text_color(k.accent)
                        .child("✦"),
                )
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(k.text)
                        .child("New discovery"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(k.text_faint)
                        .child("auto-research"),
                )
                .child(
                    canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
                        .absolute()
                        .size_full(),
                )
                .on_click(cx.listener(|this, _, window, cx| this.new_discovery(window, cx)))
                .into_any_element(),
        )
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
        let running = overview
            .running
            .iter()
            .any(|thread| thread.id == summary.id);
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
            } else if running {
                this.child(
                    div()
                        .flex_none()
                        .px(px(4.))
                        .child(pulsing_dot(("chat-sidebar-pulse", index))),
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

    /// The quiet button at the bottom: "2 agents running" or "1 done".
    fn render_tray_button(
        &self,
        overview: &ThreadsOverview,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (running, finished) = (overview.running.len(), overview.finished.len());
        if running + finished == 0 {
            return None;
        }
        let bounds = self.tray_button.clone();
        let this = cx.entity().downgrade();
        Some(
            div()
                .relative()
                .flex_none()
                .mt(px(TRAY_GAP))
                .w_full()
                .flex()
                .flex_col()
                .child(
                    TrayButton::new("chat-tray-button", running, finished)
                        .open(self.tray_open)
                        .on_click(move |_, window, cx| {
                            let _ = this.update(cx, |this, cx| this.toggle_tray(window, cx));
                        }),
                )
                .child(
                    canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
                        .absolute()
                        .size_full(),
                )
                .into_any_element(),
        )
    }

    /// The task of a tray item: the pane's, or (a review found after a
    /// restart) one made from what the overview knows.
    fn tray_task(&self, item: &TrayItem, overview: &ThreadsOverview, cx: &App) -> TaskSummary {
        if let Some(task) = self.pane.read(cx).task(&item.id) {
            return task.clone();
        }
        let finished = overview.finished.iter().find(|thread| thread.id == item.id);
        let started = finished.map_or_else(SystemTime::now, |thread| thread.started_at.into());
        let mut task = TaskSummary::new(item.title.clone(), started);
        if let Some(thread) = finished {
            task.state = if thread.success {
                TaskState::Finished
            } else {
                TaskState::Failed
            };
            task.outcome = thread.error.clone();
            task.finished_at = Some(thread.finished_at.into());
        }
        task
    }

    fn render_tray_item(
        &self,
        index: usize,
        item: TrayItem,
        overview: &ThreadsOverview,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let task = self.tray_task(&item, overview, cx);
        let this = cx.entity().downgrade();
        let (open, stop, dismiss) = (this.clone(), this.clone(), this);
        let (open_id, stop_id, dismiss_id, measured) = (
            item.id.clone(),
            item.id.clone(),
            item.id.clone(),
            item.id.clone(),
        );
        let all_bounds = self.tray_item_bounds.clone();
        let row = TaskRow::new(("chat-tray-item", index), &task)
            .selected(overview.current == item.id)
            .on_open(move |_, window, cx| {
                let _ = open.update(cx, |this, cx| this.open_from_tray(&open_id, window, cx));
            })
            .on_stop(move |_, _, cx| {
                let _ = stop.update(cx, |this, cx| this.stop(&stop_id, cx));
            })
            .on_dismiss(move |_, _, cx| {
                let _ = dismiss.update(cx, |this, cx| this.dismiss(&dismiss_id, cx));
            });
        div()
            .relative()
            .flex_none()
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(row)
            .child(
                canvas(
                    move |b, _, _| {
                        let mut all = all_bounds.borrow_mut();
                        all.retain(|(id, _)| *id != measured);
                        all.push((measured.clone(), b));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .into_any_element()
    }

    /// The tray: grows upward from the button, over the list.
    fn render_tray(
        &self,
        overview: &ThreadsOverview,
        k: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.tray_open {
            return None;
        }
        let items = tray_items(overview);
        if items.is_empty() {
            return None;
        }
        self.tray_item_bounds.borrow_mut().clear();
        let limit = (f32::from(self.height.get()) * TRAY_MAX_SHARE).max(80.);
        let children: Vec<AnyElement> = items
            .into_iter()
            .enumerate()
            .map(|(index, item)| self.render_tray_item(index, item, overview, cx))
            .collect();
        let button = self.tray_button.clone();
        Some(
            oyl_ui::raised(cx)
                .id("chat-tray")
                .key_context(TRAY_CONTEXT)
                .track_focus(&self.tray_focus)
                .on_action(cx.listener(Self::on_close_tray))
                .on_mouse_down_out(
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        // A press on the button toggles the tray there.
                        if !button.get().contains(&event.position) {
                            this.close_tray(window, cx);
                        }
                    }),
                )
                .occlude()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(TRAY_BUTTON_HEIGHT + TRAY_GAP))
                .rounded(k.radius_m)
                .p(px(TRAY_PADDING))
                .overflow_hidden()
                .font_family(k.ui_font.clone())
                .child(
                    div()
                        .id("chat-tray-items")
                        .max_h(px(limit - 2. * TRAY_PADDING))
                        .overflow_y_scroll()
                        .v_flex()
                        .children(children),
                )
                .with_animation(
                    ("chat-tray-open", self.tray_opened),
                    Animation::new(TRAY_OPEN).with_easing(gpui_kit::ease_out_quint()),
                    move |tray, delta| tray.max_h(px(limit * delta)).opacity(delta),
                )
                .into_any_element(),
        )
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

/// The small accent dot of a running turn, breathing.
fn pulsing_dot(id: impl Into<gpui_kit::ElementId>) -> PulseDot {
    PulseDot::new(id)
}

impl Render for ChatSidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let k = cx.tokens().clone();
        let overview = self.overview(cx);
        let now = Utc::now();
        let mut list: Vec<AnyElement> = Vec::new();
        if !overview.recent.is_empty() {
            list.push(
                oyl_ui::eyebrow("Recent", cx)
                    .px(px(10.))
                    .pt(px(14.))
                    .pb(px(6.))
                    .into_any_element(),
            );
            for (index, summary) in overview.recent.iter().enumerate() {
                list.push(self.render_recent(index, summary, &overview, now, &k, cx));
            }
        }
        let menu = self.render_menu(&k, cx);
        let button = self.render_tray_button(&overview, cx);
        let tray = self.render_tray(&overview, &k, cx);
        let height = self.height.clone();
        div()
            .id("chat-sidebar")
            .key_context("ChatSidebar")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_escape))
            .relative()
            .size_full()
            .v_flex()
            .font_family(k.ui_font.clone())
            .text_color(k.text)
            .child(
                canvas(move |b, _, _| height.set(b.size.height), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(self.render_new(&overview, &k, cx))
            .children(self.render_discovery(&overview, &k, cx))
            .child(
                div()
                    .id("chat-sidebar-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .v_flex()
                    .children(list),
            )
            .children(button)
            .children(tray)
            .children(menu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::FinishedThread;
    use chrono::TimeZone as _;

    fn at(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + seconds, 0)
            .single()
            .unwrap()
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
    fn the_tray_button_counts_running_and_finished_agents() {
        assert_eq!(tray_label(0, 0), None, "hidden when there is nothing");
        assert_eq!(tray_label(1, 0).as_deref(), Some("1 agent running"));
        assert_eq!(tray_label(3, 0).as_deref(), Some("3 agents running"));
        assert_eq!(tray_label(0, 2).as_deref(), Some("2 done"));
        assert_eq!(tray_label(1, 1).as_deref(), Some("1 running · 1 done"));
    }

    #[test]
    fn the_tray_lists_running_turns_then_finished_ones() {
        let running = |id: &str, started: i64| RunningThread {
            id: id.into(),
            title: format!("{id} title"),
            activity: "Thinking…".into(),
            started_at: at(started),
            stopping: false,
        };
        let finished = |id: &str| FinishedThread {
            id: id.into(),
            title: format!("{id} title"),
            success: true,
            files_changed: 3,
            error: None,
            outcome: "Done · 3 files changed".into(),
            started_at: at(0),
            finished_at: at(30),
        };
        let overview = ThreadsOverview {
            running: vec![running("old", 0), running("new", 10)],
            finished: vec![finished("first"), finished("second")],
            ..ThreadsOverview::default()
        };
        let items = tray_items(&overview);
        let ids: Vec<&str> = items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, vec!["new", "old", "second", "first"]);
        assert!(items[0].running && !items[2].running);
        assert_eq!(items[2].line, "Done · 3 files changed");
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
