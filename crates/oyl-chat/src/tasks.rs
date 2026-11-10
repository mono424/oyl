//! Each thread's latest turn as an [`oyl_tasks::TaskSummary`]: the card
//! pinned at the top of a long-running thread, and for a discovery run the
//! board beside the conversation.

use std::collections::HashMap;
use std::time::SystemTime;

use gpui_kit::component::StyledExt as _;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, Entity, div, px};
use oyl_model::agent::{AgentEvent, HarnessInfo, TurnRequest};
use oyl_tasks::{DiscoveryBoard, TaskCard, TaskSummary};
use oyl_ui::Tokens;

use super::{ChatPane, DISCOVER};

/// What a discovery thread shows under its card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadView {
    /// The discovery board (the default while the run goes on).
    Board,
    /// The conversation.
    Conversation,
}

/// The pane's record of tasks, by thread.
#[derive(Default)]
pub(super) struct Tasks {
    pub(super) summaries: HashMap<String, TaskSummary>,
    pub(super) boards: HashMap<String, Entity<DiscoveryBoard>>,
    /// What the user chose to see, per discovery thread.
    pub(super) views: HashMap<String, ThreadView>,
}

impl ChatPane {
    /// The task of thread `thread`'s latest turn.
    pub fn task(&self, thread: &str) -> Option<&TaskSummary> {
        self.tasks.summaries.get(thread)
    }

    /// The discovery board of thread `thread`, if it is a discovery.
    pub fn board(&self, thread: &str) -> Option<&Entity<DiscoveryBoard>> {
        self.tasks.boards.get(thread)
    }

    /// Whether the thread on screen shows its task card.
    pub fn shows_task_card(&self) -> bool {
        self.tasks
            .summaries
            .get(&self.thread.id)
            .is_some_and(wants_card)
    }

    /// What the thread on screen shows under its card: `None` when it is
    /// no discovery.
    pub fn thread_view(&self) -> Option<ThreadView> {
        let thread = &self.thread.id;
        self.tasks.boards.get(thread)?;
        Some(self.tasks.views.get(thread).copied().unwrap_or_else(|| {
            let running = self
                .tasks
                .summaries
                .get(thread)
                .is_some_and(TaskSummary::is_running);
            if running {
                ThreadView::Board
            } else {
                ThreadView::Conversation
            }
        }))
    }

    /// Shows the board or the conversation of the discovery on screen.
    pub fn set_thread_view(&mut self, view: ThreadView, cx: &mut Context<Self>) {
        self.tasks.views.insert(self.thread.id.clone(), view);
        cx.notify();
    }

    /// A turn starts in `thread`: its task begins.
    pub(super) fn begin_task(
        &mut self,
        thread: &str,
        info: &HarnessInfo,
        request: &TurnRequest,
        title: String,
        cx: &mut Context<Self>,
    ) {
        let task = TaskSummary::for_turn(info, request, SystemTime::now()).with_title(title);
        if request.capability.as_deref() == Some(DISCOVER)
            && !self.tasks.boards.contains_key(thread)
        {
            let board = cx.new(DiscoveryBoard::new);
            self.tasks.boards.insert(thread.to_string(), board);
        }
        self.tasks.views.remove(thread);
        self.sync_board(thread, &task, cx);
        self.tasks.summaries.insert(thread.to_string(), task);
    }

    /// An event of `thread`'s turn.
    pub(super) fn task_event(&mut self, thread: &str, event: &AgentEvent, cx: &mut Context<Self>) {
        let Some(task) = self.tasks.summaries.get_mut(thread) else {
            return;
        };
        task.apply(event);
        let task = task.clone();
        self.sync_board(thread, &task, cx);
    }

    /// Stop was pressed for `thread`'s turn.
    pub(super) fn task_stopping(&mut self, thread: &str) {
        if let Some(task) = self.tasks.summaries.get_mut(thread) {
            task.request_stop();
        }
    }

    /// `thread`'s turn ended without the task seeing a terminal event (the
    /// stream closed, the run was abandoned): it ends as the turn did.
    pub(super) fn end_task(
        &mut self,
        thread: &str,
        success: bool,
        reason: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self.tasks.summaries.get_mut(thread) else {
            return;
        };
        if !task.is_running() {
            return;
        }
        let event = match reason {
            Some(message) if !success => AgentEvent::Failed { message },
            _ => AgentEvent::Completed {
                success,
                summary: String::new(),
                session: None,
            },
        };
        task.apply(&event);
        let task = task.clone();
        self.sync_board(thread, &task, cx);
    }

    /// A thread is gone: so is its task.
    pub(super) fn forget_task(&mut self, thread: &str) {
        self.tasks.summaries.remove(thread);
        self.tasks.boards.remove(thread);
        self.tasks.views.remove(thread);
    }

    /// Feeds a discovery's board. Only the board of the thread on screen
    /// watches its folder: a board out of sight lets go of it (see
    /// [`ChatPane::boards_follow`]).
    fn sync_board(&mut self, thread: &str, task: &TaskSummary, cx: &mut Context<Self>) {
        if thread != self.thread.id {
            return;
        }
        if let Some(board) = self.tasks.boards.get(thread) {
            board.update(cx, |board, cx| board.sync(task, cx));
        }
    }

    /// Another thread is on screen: the board left behind stops watching,
    /// the one now shown follows its task again.
    pub(super) fn boards_follow(&mut self, previous: &str, cx: &mut Context<Self>) {
        if previous != self.thread.id
            && let Some(board) = self.tasks.boards.get(previous)
        {
            board.update(cx, |board, cx| board.watch(None, cx));
        }
        let shown = self.thread.id.clone();
        if let Some(task) = self.tasks.summaries.get(&shown).cloned() {
            self.sync_board(&shown, &task, cx);
        }
    }

    /// At the top of the thread on screen: its task card (long-running or
    /// reporting progress), then for a discovery the Board / Conversation
    /// toggle.
    pub(super) fn render_task_header(
        &self,
        k: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let thread = self.thread.id.clone();
        let task = self
            .tasks
            .summaries
            .get(&thread)
            .filter(|task| wants_card(task))?;
        let stop = thread.clone();
        let this = cx.entity().downgrade();
        let card = TaskCard::new("chat-task-card", task).on_stop(move |_, _, cx| {
            let _ = this.update(cx, |pane, cx| pane.stop_thread(&stop, cx));
        });
        let toggle = self.thread_view().map(|view| {
            let options = [
                (ThreadView::Board, "Board"),
                (ThreadView::Conversation, "Conversation"),
            ];
            let (accent, accent_soft, faint, soft) =
                (k.accent, k.accent_soft, k.text_faint, k.text_soft);
            div()
                .h_flex()
                .gap(px(2.))
                .children(options.into_iter().map(|(option, label)| {
                    let selected = view == option;
                    div()
                        .id(("chat-thread-view", option as usize))
                        .h(px(22.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .rounded(k.radius_s)
                        .text_xs()
                        .cursor_pointer()
                        .map(|this| {
                            if selected {
                                this.bg(accent_soft).text_color(accent)
                            } else {
                                this.text_color(faint)
                                    .hover(move |style| style.text_color(soft))
                            }
                        })
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.set_thread_view(option, cx)),
                        )
                        .child(label)
                }))
        });
        Some(
            div()
                .flex_shrink_0()
                .v_flex()
                .gap(px(6.))
                .px(px(14.))
                .pb(px(6.))
                .child(card)
                .children(toggle)
                .into_any_element(),
        )
    }

    /// The discovery board of the thread on screen, when it is the view.
    pub(super) fn render_board(&self) -> Option<AnyElement> {
        if self.thread_view() != Some(ThreadView::Board) {
            return None;
        }
        let board = self.tasks.boards.get(&self.thread.id)?.clone();
        Some(
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_hidden()
                .px(px(14.))
                .child(board)
                .into_any_element(),
        )
    }
}

/// Whether a task gets the card: a long-running capability, or a turn
/// that reported progress.
fn wants_card(task: &TaskSummary) -> bool {
    task.is_long_running() || task.progress.is_some()
}
