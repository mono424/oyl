//! Agent turns on branches: every turn works in a private copy of the
//! paper ([`oyl_history::Branch`]); when it ends its changes come back by
//! themselves when the paper did not change the same files meanwhile, and
//! through a review in the thread otherwise.

use std::path::{Path, PathBuf};

use gpui_kit::prelude::*;
use gpui_kit::{Context, Window};
use oyl_chat::{
    BranchView, ReviewChoice, ReviewConflict, ReviewFile, ReviewLine, ReviewLineKind, ReviewView,
};
use oyl_history::{
    Branch, BranchState, Choice, ConflictId, ConflictKind, FinishOutcome, HistoryError, LineKind,
    Resolution, Review,
};

use super::{Shell, ToastKind};
use crate::persist::BranchThread;

/// What the review card adds when the paper changed again before Apply.
pub(crate) const REVIEW_UPDATED: &str = "The paper changed again — review updated";

/// The branch of one thread.
pub(crate) struct ThreadBranch {
    pub(crate) branch: Branch,
    /// The turn that made it.
    pub(crate) turn: String,
    pub(crate) stage: Stage,
    /// Its turn ended off screen: say how it went once known.
    pub(crate) tell_when_done: bool,
}

/// Where a thread's branch is.
pub(crate) enum Stage {
    /// The agent works in it.
    Working,
    /// Its turn ended; its changes are being brought back.
    Finishing,
    /// It waits for the user to apply or discard the merge.
    Review(Review),
    /// The agent is gone (Oyl quit) before its work came back.
    Orphaned,
}

impl Shell {
    /// The branch the preview shows instead of the paper: in the Agent tab,
    /// the thread on screen's, while its turn runs or waits for a review.
    /// Its working copy and the thread's title.
    pub(crate) fn preview_branch(&self, cx: &gpui_kit::App) -> Option<(PathBuf, String)> {
        if self.mode != oyl_core::LayoutMode::Agent {
            return None;
        }
        let paper = self.paper.as_ref()?;
        let chat = self.chat.read(cx);
        let thread = chat.thread();
        let branch = paper.branches.get(&thread.id)?;
        Some((branch.branch.path().to_path_buf(), thread.title()))
    }

    /// `path` as it is in the paper when it is in a branch's working copy.
    pub(crate) fn paper_path(&self, path: &Path) -> PathBuf {
        let Some(paper) = self.paper.as_ref() else {
            return path.to_path_buf();
        };
        paper
            .branches
            .values()
            .find_map(|branch| {
                path.strip_prefix(branch.branch.path())
                    .ok()
                    .map(|relative| paper.root().join(relative))
            })
            .unwrap_or_else(|| path.to_path_buf())
    }

    /// A turn's branch exists: remember it (and finish it at once when the
    /// turn ended before).
    pub(crate) fn branch_created(
        &mut self,
        thread: String,
        turn: String,
        branch: Branch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ended = self.hook.borrow_mut().pending.remove(&turn) == Some(true);
        let Some(paper) = self.paper.as_mut().filter(|paper| {
            paper
                .history
                .as_ref()
                .is_some_and(|history| branch.path().starts_with(history.root()))
        }) else {
            return;
        };
        paper.branch_threads.branches.insert(
            branch.id().to_string(),
            BranchThread {
                thread: thread.clone(),
                turn: turn.clone(),
            },
        );
        paper.branches.insert(
            thread.clone(),
            ThreadBranch {
                branch,
                turn,
                stage: Stage::Working,
                tell_when_done: false,
            },
        );
        self.save_branch_threads(cx);
        self.history_changed(cx);
        if ended {
            self.finish_branch(&thread, window, cx);
        } else {
            self.request_compile(window, cx);
        }
        cx.notify();
    }

    /// The turn of `thread` ended: bring its branch's changes back (off the
    /// main thread). False when the thread has no branch.
    pub(crate) fn turn_ended_on_branch(
        &mut self,
        thread: &str,
        turn: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let owned = self
            .paper
            .as_ref()
            .and_then(|paper| paper.branches.get(thread))
            .is_some_and(|branch| branch.turn == turn);
        if owned {
            self.finish_branch(thread, window, cx);
        }
        owned
    }

    /// Brings thread `thread`'s branch back into the paper (the Finish of an
    /// orphaned branch too).
    pub(crate) fn finish_branch(
        &mut self,
        thread: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(paper) = self.paper.as_mut() else {
            return;
        };
        let epoch = paper.epoch;
        let Some(entry) = paper.branches.get_mut(thread) else {
            return;
        };
        entry.stage = Stage::Finishing;
        let branch = entry.branch.clone();
        let thread = thread.to_string();
        cx.spawn_in(window, async move |this, cx| {
            let label = branch.label().to_string();
            let outcome = cx
                .background_spawn(async move { branch.finish(&label) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.paper.as_ref().map(|paper| paper.epoch) == Some(epoch) {
                    this.branch_finished(&thread, outcome, window, cx);
                }
            });
        })
        .detach();
    }

    fn branch_finished(
        &mut self,
        thread: &str,
        outcome: oyl_history::Result<FinishOutcome>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = self.thread_title(thread, cx);
        let tell = self
            .paper
            .as_ref()
            .and_then(|paper| paper.branches.get(thread))
            .is_some_and(|branch| branch.tell_when_done);
        match outcome {
            Ok(FinishOutcome::NoChanges) => {
                self.forget_branch(thread, None, cx);
                if tell {
                    self.tell_finished(
                        thread,
                        format!("“{title}” finished"),
                        ToastKind::Success,
                        window,
                        cx,
                    );
                }
            }
            Ok(FinishOutcome::Applied { version, files }) => {
                let turn =
                    self.forget_branch(thread, version.map(|version| version.id.to_string()), cx);
                self.paper_changed_by_agent(window, cx);
                let line = oyl_chat::applied_line(files.len());
                if let Some(turn) = turn.clone() {
                    self.chat.update(cx, |chat, cx| {
                        chat.set_branch_view(
                            thread,
                            Some(BranchView::Applied {
                                turn,
                                files: files.len(),
                            }),
                            cx,
                        )
                    });
                }
                if tell {
                    let message = format!("“{title}” finished · {}", line.to_lowercase());
                    self.tell_finished(thread, message, ToastKind::Success, window, cx);
                } else if let Some(turn) = turn {
                    self.toast_with_action(
                        ToastKind::Success,
                        line,
                        "Undo",
                        std::rc::Rc::new(
                            move |shell: &mut Shell,
                                  window: &mut Window,
                                  cx: &mut Context<Shell>| {
                                shell.undo_turn(turn.clone(), window, cx)
                            },
                        ),
                        window,
                        cx,
                    );
                }
            }
            Ok(FinishOutcome::NeedsReview(review)) => {
                self.show_review(thread, review, None, cx);
                if tell {
                    self.tell_finished(
                        thread,
                        format!("“{title}” needs review"),
                        ToastKind::Info,
                        window,
                        cx,
                    );
                }
            }
            Err(err) => {
                if let Some(entry) = self
                    .paper
                    .as_mut()
                    .and_then(|paper| paper.branches.get_mut(thread))
                {
                    entry.stage = Stage::Orphaned;
                }
                self.chat.update(cx, |chat, cx| {
                    chat.set_branch_view(thread, Some(BranchView::Orphaned), cx)
                });
                self.toast(
                    ToastKind::Error,
                    format!("The agent's changes could not be brought into the paper: {err}"),
                    window,
                    cx,
                );
            }
        }
        self.request_compile(window, cx);
        cx.notify();
    }

    /// The user applies thread `thread`'s review.
    pub(crate) fn apply_branch_review(
        &mut self,
        thread: &str,
        choices: &[(String, ReviewChoice)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(paper) = self.paper.as_ref() else {
            return;
        };
        let epoch = paper.epoch;
        let Some(entry) = paper.branches.get(thread) else {
            return;
        };
        let files = match &entry.stage {
            Stage::Review(review) => review.files.len(),
            _ => return,
        };
        let resolution = resolution(choices);
        let branch = entry.branch.clone();
        let thread = thread.to_string();
        cx.spawn_in(window, async move |this, cx| {
            let applied = cx
                .background_spawn(async move { branch.apply(&resolution) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.paper.as_ref().map(|paper| paper.epoch) != Some(epoch) {
                    return;
                }
                match applied {
                    Ok(version) => {
                        let changed = version.is_some();
                        let turn =
                            this.forget_branch(&thread, version.map(|v| v.id.to_string()), cx);
                        this.paper_changed_by_agent(window, cx);
                        // Keeping the paper's side everywhere changes nothing.
                        let view = turn
                            .filter(|_| changed)
                            .map(|turn| BranchView::Applied { turn, files });
                        this.chat
                            .update(cx, |chat, cx| chat.set_branch_view(&thread, view, cx));
                    }
                    Err(HistoryError::PaperChanged(review)) => {
                        this.show_review(&thread, *review, Some(REVIEW_UPDATED.to_string()), cx);
                    }
                    Err(err) => {
                        let view = this.current_review_view(&thread, None);
                        this.chat
                            .update(cx, |chat, cx| chat.set_branch_view(&thread, view, cx));
                        this.toast(
                            ToastKind::Error,
                            format!("The review could not be applied: {err}"),
                            window,
                            cx,
                        );
                    }
                }
                this.request_compile(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// The user discards thread `thread`'s branch.
    pub(crate) fn discard_thread_branch(
        &mut self,
        thread: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(paper) = self.paper.as_ref() else {
            return;
        };
        let epoch = paper.epoch;
        let Some(branch) = paper.branches.get(thread).map(|entry| entry.branch.clone()) else {
            return;
        };
        let thread = thread.to_string();
        cx.spawn_in(window, async move |this, cx| {
            let discarded = cx.background_spawn(async move { branch.discard() }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.paper.as_ref().map(|paper| paper.epoch) != Some(epoch) {
                    return;
                }
                match discarded {
                    Ok(()) | Err(HistoryError::BranchNotFound(_)) => {
                        this.forget_branch(&thread, None, cx);
                        this.chat
                            .update(cx, |chat, cx| chat.set_branch_view(&thread, None, cx));
                    }
                    Err(err) => {
                        let view = this.current_review_view(&thread, None);
                        this.chat
                            .update(cx, |chat, cx| chat.set_branch_view(&thread, view, cx));
                        this.toast(
                            ToastKind::Error,
                            format!("The agent's changes could not be discarded: {err}"),
                            window,
                            cx,
                        );
                    }
                }
                this.request_compile(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// After a restart: reviews come back in their threads, branches whose
    /// agent is gone offer Finish or Discard.
    pub(crate) fn restore_branches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(paper) = self.paper.as_ref() else {
            return;
        };
        let Some(history) = paper.history.clone() else {
            return;
        };
        let epoch = paper.epoch;
        let record = paper.branch_threads.clone();
        cx.spawn_in(window, async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    let mut found: Vec<(BranchThread, Branch, Option<Review>)> = Vec::new();
                    for info in history.branches()? {
                        let Some(owner) = record.branches.get(&info.id.to_string()).cloned() else {
                            continue;
                        };
                        let branch = history.branch(info.id)?;
                        let review = match info.state {
                            BranchState::Review => Some(branch.review()?),
                            BranchState::Working => None,
                        };
                        found.push((owner, branch, review));
                    }
                    Ok::<_, HistoryError>(found)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.paper.as_ref().map(|paper| paper.epoch) != Some(epoch) {
                    return;
                }
                match found {
                    Ok(found) => {
                        for (owner, branch, review) in found {
                            this.restore_branch(owner, branch, review, cx);
                        }
                    }
                    Err(err) => this.toast(
                        ToastKind::Error,
                        format!("The agents' unfinished work could not be read: {err}"),
                        window,
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn restore_branch(
        &mut self,
        owner: BranchThread,
        branch: Branch,
        review: Option<Review>,
        cx: &mut Context<Self>,
    ) {
        let Some(paper) = self.paper.as_mut() else {
            return;
        };
        let thread = owner.thread.clone();
        let (stage, view) = match review {
            Some(review) => {
                let view = BranchView::Review(review_view(&review, None));
                (Stage::Review(review), view)
            }
            None => (Stage::Orphaned, BranchView::Orphaned),
        };
        paper.branches.insert(
            thread.clone(),
            ThreadBranch {
                branch,
                turn: owner.turn,
                stage,
                tell_when_done: false,
            },
        );
        self.chat
            .update(cx, |chat, cx| chat.set_branch_view(&thread, Some(view), cx));
    }

    /// A finished-off-screen turn whose branch is still being brought back:
    /// its toast waits for the outcome. False when there is no such branch.
    pub(crate) fn defer_finished_toast(&mut self, thread: &str) -> bool {
        match self
            .paper
            .as_mut()
            .and_then(|paper| paper.branches.get_mut(thread))
        {
            Some(entry) if matches!(entry.stage, Stage::Working | Stage::Finishing) => {
                entry.tell_when_done = true;
                true
            }
            _ => false,
        }
    }

    fn show_review(
        &mut self,
        thread: &str,
        review: Review,
        note: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let view = BranchView::Review(review_view(&review, note));
        if let Some(entry) = self
            .paper
            .as_mut()
            .and_then(|paper| paper.branches.get_mut(thread))
        {
            entry.stage = Stage::Review(review);
        }
        self.chat
            .update(cx, |chat, cx| chat.set_branch_view(thread, Some(view), cx));
    }

    fn current_review_view(&self, thread: &str, note: Option<String>) -> Option<BranchView> {
        let entry = self.paper.as_ref()?.branches.get(thread)?;
        match &entry.stage {
            Stage::Review(review) => Some(BranchView::Review(review_view(review, note))),
            Stage::Orphaned => Some(BranchView::Orphaned),
            Stage::Working | Stage::Finishing => None,
        }
    }

    /// Drops thread `thread`'s branch from the records; with the version
    /// it applied, remembers it as its turn's (for Undo). Returns the turn.
    fn forget_branch(
        &mut self,
        thread: &str,
        version: Option<String>,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let paper = self.paper.as_mut()?;
        let entry = paper.branches.remove(thread)?;
        paper
            .branch_threads
            .branches
            .remove(&entry.branch.id().to_string());
        if let Some(version) = version {
            paper.turns.turns.insert(entry.turn.clone(), version);
            let record = paper.turns.clone();
            let root = paper.root().to_path_buf();
            cx.background_spawn(async move {
                if let Err(err) = record.save(&root) {
                    log::warn!("saving the agent's turns: {err:#}");
                }
            })
            .detach();
        }
        self.save_branch_threads(cx);
        Some(entry.turn)
    }

    fn save_branch_threads(&self, cx: &mut Context<Self>) {
        let Some(paper) = self.paper.as_ref() else {
            return;
        };
        let record = paper.branch_threads.clone();
        let root = paper.root().to_path_buf();
        cx.background_spawn(async move {
            if let Err(err) = record.save(&root) {
                log::warn!("saving the agents' branches: {err:#}");
            }
        })
        .detach();
    }

    /// The paper's files changed under the editor by an applied branch.
    pub(crate) fn paper_changed_by_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reload_open_buffers(window, cx);
        self.history_changed(cx);
        self.refresh_index(cx);
        self.schedule_words(cx);
    }

    fn thread_title(&self, thread: &str, cx: &gpui_kit::App) -> String {
        let chat = self.chat.read(cx);
        if chat.thread().id == thread {
            return chat.thread().title();
        }
        chat.threads_overview()
            .recent
            .into_iter()
            .find(|summary| summary.id == thread)
            .map(|summary| summary.title)
            .unwrap_or_else(|| "The agent's task".to_string())
    }

    fn tell_finished(
        &mut self,
        thread: &str,
        message: String,
        kind: ToastKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = thread.to_string();
        self.toast_with_action(
            kind,
            message,
            "Open",
            std::rc::Rc::new(
                move |shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>| {
                    shell.open_thread(&id, window, cx)
                },
            ),
            window,
            cx,
        );
    }
}

/// The resolution the user's choices make.
pub(crate) fn resolution(choices: &[(String, ReviewChoice)]) -> Resolution {
    let mut resolution = Resolution::new();
    for (id, choice) in choices {
        let Ok(id) = serde_json::from_value::<ConflictId>(serde_json::Value::String(id.clone()))
        else {
            continue;
        };
        let choice = match choice {
            ReviewChoice::Yours => Choice::Main,
            ReviewChoice::Agents => Choice::Agent,
            ReviewChoice::Both => Choice::Both,
        };
        resolution.set(id, choice);
    }
    resolution
}

/// A review in the chat pane's terms.
pub(crate) fn review_view(review: &Review, note: Option<String>) -> ReviewView {
    let files = review
        .files
        .iter()
        .map(|file| {
            let mut lines = Vec::new();
            for (index, hunk) in file.diff.hunks.iter().enumerate() {
                if index > 0 {
                    lines.push(ReviewLine {
                        kind: ReviewLineKind::Gap,
                        text: String::new(),
                    });
                }
                lines.extend(hunk.lines.iter().map(|line| ReviewLine {
                    kind: match line.kind {
                        LineKind::Context => ReviewLineKind::Context,
                        LineKind::Add => ReviewLineKind::Added,
                        LineKind::Remove => ReviewLineKind::Removed,
                    },
                    text: line.text.clone(),
                }));
            }
            ReviewFile {
                path: file.diff.path.clone(),
                added: file.diff.added,
                removed: file.diff.removed,
                binary: file.binary,
                lines,
            }
        })
        .collect();
    let conflicts = review
        .conflicts
        .iter()
        .map(|conflict| ReviewConflict {
            id: conflict.id.to_string(),
            file: conflict.file.clone(),
            place: conflict_place(conflict.kind),
            lines: matches!(conflict.kind, ConflictKind::Lines { .. }),
            main: conflict.main.clone(),
            agent: conflict.agent.clone(),
        })
        .collect();
    ReviewView {
        files,
        conflicts,
        note,
    }
}

/// Where a conflict is, in words.
fn conflict_place(kind: ConflictKind) -> String {
    match kind {
        ConflictKind::Lines { lines } => match lines.count {
            0 => format!("before line {}", lines.start),
            1 => format!("line {}", lines.start),
            n => format!("lines {}–{}", lines.start, lines.start + n - 1),
        },
        ConflictKind::File => "the whole file".to_string(),
        ConflictKind::DeletedByAgent => "deleted by the agent".to_string(),
        ConflictKind::DeletedInMain => "deleted in the paper".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oyl_history::LineSpan;

    #[test]
    fn conflict_places_read_plainly() {
        let lines = |start, count| ConflictKind::Lines {
            lines: LineSpan { start, count },
        };
        assert_eq!(conflict_place(lines(12, 3)), "lines 12–14");
        assert_eq!(conflict_place(lines(7, 1)), "line 7");
        assert_eq!(conflict_place(lines(4, 0)), "before line 4");
        assert_eq!(conflict_place(ConflictKind::File), "the whole file");
    }

    #[test]
    fn choices_become_a_resolution() {
        let id = "0123456789abcdef".to_string();
        let resolution = resolution(&[(id.clone(), ReviewChoice::Both)]);
        let conflict: ConflictId =
            serde_json::from_value(serde_json::Value::String(id)).expect("an id");
        assert_eq!(resolution.choice(&conflict), Choice::Both);
    }
}
