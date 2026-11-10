//! What became of a thread's branch, under its thread: the changes applied
//! (with Undo), or the review of a merge with the paper's own changes
//! (files, diffs, conflicts with a choice each, Apply / Discard), or an
//! agent that stopped before finishing (Finish / Discard).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, SharedString, div, px};
use oyl_ui::Tokens;

use super::{ChatEvent, ChatPane};

/// The most diff lines a file shows in the review.
const MAX_DIFF_LINES: usize = 200;
/// The most lines each side of a conflict shows.
const MAX_SIDE_LINES: usize = 12;
/// What the review card says first.
pub const REVIEW_INTRO: &str =
    "The paper changed while this ran. Your changes were merged into the agent's work.";
/// The key the review of changes from Oyl Cloud is kept under in the
/// pane's records (never a thread id). [`ChatEvent::ApplyReview`] and
/// [`ChatEvent::DiscardBranch`] carry it as their thread.
pub const CLOUD_REVIEW: &str = "oyl-cloud";
/// The title of the card of changes from Oyl Cloud.
pub const CLOUD_REVIEW_TITLE: &str = "Changes from Oyl Cloud";
/// What that card says first.
pub const CLOUD_REVIEW_INTRO: &str =
    "These lines changed here too since the last sync. Pick what to keep, then apply.";

/// The words of a review card: an agent's, or the cloud's.
struct ReviewWords {
    /// Element ids start with this.
    prefix: &'static str,
    /// Above the intro, if any.
    title: Option<&'static str>,
    /// Where the changes come from ("From MacBook Pro").
    source: Option<String>,
    intro: &'static str,
    /// The other side of a conflict.
    theirs: &'static str,
}

/// What became of a thread's branch, as the app tells the pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BranchView {
    /// The agent's changes were written into the paper.
    Applied {
        /// The turn they came from (Undo undoes it).
        turn: String,
        /// How many files were written.
        files: usize,
    },
    /// The paper changed the same files meanwhile: a merge to review.
    Review(ReviewView),
    /// The agent stopped (Oyl quit) before its work was brought back.
    Orphaned,
}

/// A merge to review, in the pane's own terms.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReviewView {
    /// Every file applying would change, in path order.
    pub files: Vec<ReviewFile>,
    /// The places both sides changed differently.
    pub conflicts: Vec<ReviewConflict>,
    /// A line above the files ("The paper changed again — review updated").
    pub note: Option<String>,
}

/// One file of a review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewFile {
    /// Relative to the paper folder.
    pub path: PathBuf,
    /// Lines added.
    pub added: u32,
    /// Lines removed.
    pub removed: u32,
    /// Compared as a whole (an image, a PDF): no lines.
    pub binary: bool,
    /// The diff, hunk after hunk.
    pub lines: Vec<ReviewLine>,
}

/// One line of a review's diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewLine {
    /// Context, added or removed.
    pub kind: ReviewLineKind,
    /// The line, without its line ending.
    pub text: String,
}

/// What a diff line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewLineKind {
    /// Unchanged, for context.
    Context,
    /// Only in the result.
    Added,
    /// Only in the paper as it is.
    Removed,
    /// Between hunks.
    Gap,
}

/// A conflict of a review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewConflict {
    /// Stable across reviews of the same conflict.
    pub id: String,
    /// Relative to the paper folder.
    pub file: PathBuf,
    /// Where, in words ("lines 12–14", "the whole file").
    pub place: String,
    /// Whether both sides can be kept (text lines).
    pub lines: bool,
    /// The paper's side.
    pub main: String,
    /// The agent's side.
    pub agent: String,
}

/// How a conflict is settled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ReviewChoice {
    /// The paper's side.
    Yours,
    /// The agent's side (the default).
    #[default]
    Agents,
    /// The paper's lines, then the agent's.
    Both,
}

/// The pane's record of thread branches.
#[derive(Default)]
pub(super) struct BranchViews {
    pub(super) views: HashMap<String, BranchView>,
    /// Per thread, the choices made, by conflict id (kept across updated
    /// reviews).
    pub(super) choices: HashMap<String, HashMap<String, ReviewChoice>>,
    /// Files whose diff is open, per thread.
    pub(super) open_files: HashMap<String, HashSet<PathBuf>>,
    /// Threads whose Apply, Discard or Finish was asked for and not
    /// answered yet.
    pub(super) busy: HashSet<String>,
    /// Where the cloud's review comes from ("From MacBook Pro").
    pub(super) cloud_source: Option<String>,
}

impl ChatPane {
    /// Shows what became of thread `thread`'s branch under it (`None`:
    /// nothing). A review keeps the choices already made for conflicts it
    /// still has. For a thread not on screen, a review counts as finished
    /// unseen ("Needs review").
    pub fn set_branch_view(
        &mut self,
        thread: &str,
        view: Option<BranchView>,
        cx: &mut Context<Self>,
    ) {
        self.branches.busy.remove(thread);
        match view {
            Some(view) => {
                if matches!(view, BranchView::Review(_)) {
                    self.mark_needs_review(thread);
                }
                self.branches.views.insert(thread.to_string(), view);
            }
            None => {
                self.branches.views.remove(thread);
                self.branches.choices.remove(thread);
                self.branches.open_files.remove(thread);
            }
        }
        cx.notify();
    }

    /// What the thread's branch footer shows.
    pub fn branch_view(&self, thread: &str) -> Option<&BranchView> {
        self.branches.views.get(thread)
    }

    /// Shows changes from Oyl Cloud that wait for a review (`None` hides
    /// the card), whichever thread is on screen, under the title "Changes
    /// from Oyl Cloud". `source` says where they come from ("From MacBook
    /// Pro"). Choices made for conflicts it still has are kept. Its Apply
    /// and Discard are [`ChatEvent::ApplyReview`] and
    /// [`ChatEvent::DiscardBranch`] with [`CLOUD_REVIEW`] as the thread.
    pub fn set_cloud_review(
        &mut self,
        review: Option<ReviewView>,
        source: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.branches.busy.remove(CLOUD_REVIEW);
        match review {
            Some(review) => {
                self.branches
                    .views
                    .insert(CLOUD_REVIEW.to_string(), BranchView::Review(review));
                self.branches.cloud_source = source;
            }
            None => {
                self.branches.views.remove(CLOUD_REVIEW);
                self.branches.choices.remove(CLOUD_REVIEW);
                self.branches.open_files.remove(CLOUD_REVIEW);
                self.branches.cloud_source = None;
            }
        }
        cx.notify();
    }

    /// The review of changes from Oyl Cloud on screen, if any.
    pub fn cloud_review(&self) -> Option<&ReviewView> {
        match self.branches.views.get(CLOUD_REVIEW) {
            Some(BranchView::Review(review)) => Some(review),
            _ => None,
        }
    }

    /// Above the composer, whatever thread is on screen: changes from Oyl
    /// Cloud waiting for a review.
    pub(super) fn render_cloud_review(
        &self,
        k: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let review = self.cloud_review()?;
        let busy = self.branches.busy.contains(CLOUD_REVIEW);
        let card = oyl_ui::surface(cx)
            .id("cloud-review")
            .flex_shrink_0()
            .max_h(px(360.))
            .overflow_y_scroll()
            .mx(px(14.))
            .mb(px(6.))
            .p(px(10.))
            .v_flex()
            .gap(px(8.))
            .text_xs()
            .text_color(k.text_soft);
        let words = ReviewWords {
            prefix: "cloud-review",
            title: Some(CLOUD_REVIEW_TITLE),
            source: self.branches.cloud_source.clone(),
            intro: CLOUD_REVIEW_INTRO,
            theirs: "Cloud",
        };
        Some(self.render_review(CLOUD_REVIEW, review, busy, card, &words, k, cx))
    }

    /// Settles conflict `conflict` of thread `thread`'s review.
    pub fn set_review_choice(
        &mut self,
        thread: &str,
        conflict: &str,
        choice: ReviewChoice,
        cx: &mut Context<Self>,
    ) {
        self.branches
            .choices
            .entry(thread.to_string())
            .or_default()
            .insert(conflict.to_string(), choice);
        cx.notify();
    }

    /// The choice for every conflict of thread `thread`'s review.
    pub fn review_choices(&self, thread: &str) -> Vec<(String, ReviewChoice)> {
        let Some(BranchView::Review(review)) = self.branches.views.get(thread) else {
            return Vec::new();
        };
        let made = self.branches.choices.get(thread);
        review
            .conflicts
            .iter()
            .map(|conflict| {
                let choice = made
                    .and_then(|made| made.get(&conflict.id))
                    .copied()
                    .unwrap_or_default();
                // Both is only for lines.
                let choice = if choice == ReviewChoice::Both && !conflict.lines {
                    ReviewChoice::Agents
                } else {
                    choice
                };
                (conflict.id.clone(), choice)
            })
            .collect()
    }

    /// Asks the app to apply thread `thread`'s review with the choices made.
    pub fn apply_review(&mut self, thread: &str, cx: &mut Context<Self>) {
        if !matches!(self.branches.views.get(thread), Some(BranchView::Review(_)))
            || !self.branches.busy.insert(thread.to_string())
        {
            return;
        }
        let choices = self.review_choices(thread);
        cx.emit(ChatEvent::ApplyReview {
            thread: thread.to_string(),
            choices,
        });
        cx.notify();
    }

    /// Asks the app to discard thread `thread`'s branch.
    pub fn discard_branch(&mut self, thread: &str, cx: &mut Context<Self>) {
        if !self.branches.views.contains_key(thread)
            || !self.branches.busy.insert(thread.to_string())
        {
            return;
        }
        cx.emit(ChatEvent::DiscardBranch {
            thread: thread.to_string(),
        });
        cx.notify();
    }

    /// Asks the app to bring an orphaned branch's work back.
    fn finish_orphan(&mut self, thread: &str, cx: &mut Context<Self>) {
        if !self.branches.busy.insert(thread.to_string()) {
            return;
        }
        cx.emit(ChatEvent::FinishBranch {
            thread: thread.to_string(),
        });
        cx.notify();
    }

    /// Whether thread `thread` waits for its branch to be settled (no new
    /// turn starts there until then).
    pub(super) fn branch_pending(&self, thread: &str) -> bool {
        matches!(
            self.branches.views.get(thread),
            Some(BranchView::Review(_) | BranchView::Orphaned)
        )
    }

    fn toggle_file(&mut self, thread: &str, path: PathBuf, cx: &mut Context<Self>) {
        let open = self
            .branches
            .open_files
            .entry(thread.to_string())
            .or_default();
        if !open.remove(&path) {
            open.insert(path);
        }
        cx.notify();
    }

    /// Under the thread on screen: what became of its branch.
    pub(super) fn render_branch_footer(
        &self,
        k: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let thread = self.thread.id.clone();
        let view = self.branches.views.get(&thread)?;
        let busy = self.branches.busy.contains(&thread);
        let card = oyl_ui::surface(cx)
            .flex_shrink_0()
            .mx(px(14.))
            .mb(px(6.))
            .p(px(10.))
            .v_flex()
            .gap(px(8.))
            .text_xs()
            .text_color(k.text_soft);
        let element = match view {
            BranchView::Applied { turn, files } => {
                let turn = turn.clone();
                card.child(
                    div()
                        .h_flex()
                        .gap(px(8.))
                        .child(div().flex_1().child(applied_line(*files)))
                        .child(
                            Button::new("chat-branch-undo")
                                .ghost()
                                .xsmall()
                                .label("Undo")
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.undo_turn(&turn, cx)),
                                ),
                        ),
                )
                .into_any_element()
            }
            BranchView::Orphaned => {
                let (finish, discard) = (thread.clone(), thread.clone());
                card.child(
                    "The agent stopped before its work was brought into the paper. Finish applies what it did; Discard drops it.",
                )
                .child(
                    div()
                        .h_flex()
                        .gap(px(6.))
                        .child(div().flex_1())
                        .child(
                            Button::new("chat-branch-discard")
                                .ghost()
                                .xsmall()
                                .label("Discard")
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.discard_branch(&discard, cx)
                                })),
                        )
                        .child(
                            Button::new("chat-branch-finish")
                                .custom(accent_variant(k, cx))
                                .xsmall()
                                .label("Finish")
                                .loading(busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.finish_orphan(&finish, cx)
                                })),
                        ),
                )
                .into_any_element()
            }
            BranchView::Review(review) => {
                let words = ReviewWords {
                    prefix: "chat-review",
                    title: None,
                    source: None,
                    intro: REVIEW_INTRO,
                    theirs: "Agent's",
                };
                self.render_review(&thread, review, busy, card, &words, k, cx)
            }
        };
        Some(element)
    }

    #[allow(clippy::too_many_arguments)]
    fn render_review<C: ParentElement + IntoElement>(
        &self,
        thread: &str,
        review: &ReviewView,
        busy: bool,
        card: C,
        words: &ReviewWords,
        k: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let prefix = words.prefix;
        let open = self.branches.open_files.get(thread);
        let choices: HashMap<String, ReviewChoice> =
            self.review_choices(thread).into_iter().collect();
        let files = review.files.iter().enumerate().map(|(index, file)| {
            let expanded = open.is_some_and(|open| open.contains(&file.path));
            let (toggle_thread, path) = (thread.to_string(), file.path.clone());
            let hover = k.surface_hover;
            div()
                .v_flex()
                .child(
                    div()
                        .id(SharedString::from(format!("{prefix}-file-{index}")))
                        .h_flex()
                        .h(px(24.))
                        .px(px(6.))
                        .gap(px(8.))
                        .rounded(k.radius_s)
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_file(&toggle_thread, path.clone(), cx)
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(k.mono_font.clone())
                                .text_color(k.text)
                                .child(file.path.display().to_string()),
                        )
                        .map(|this| {
                            if file.binary {
                                this.child(div().text_color(k.text_faint).child("binary"))
                            } else {
                                this.child(
                                    div()
                                        .text_color(k.success)
                                        .child(format!("+{}", file.added)),
                                )
                                .child(
                                    div()
                                        .text_color(k.danger)
                                        .child(format!("−{}", file.removed)),
                                )
                            }
                        }),
                )
                .when(expanded && !file.lines.is_empty(), |this| {
                    this.child(render_diff(&file.lines, k))
                })
        });
        let conflicts = review
            .conflicts
            .iter()
            .enumerate()
            .map(|(index, conflict)| {
                let current = choices.get(&conflict.id).copied().unwrap_or_default();
                let mut options = vec![
                    (ReviewChoice::Yours, "Yours"),
                    (ReviewChoice::Agents, words.theirs),
                ];
                if conflict.lines {
                    options.push((ReviewChoice::Both, "Both"));
                }
                let pills = options.into_iter().map(|(choice, label)| {
                    let (pill_thread, id) = (thread.to_string(), conflict.id.clone());
                    let selected = current == choice;
                    let (accent, accent_soft, faint, soft) =
                        (k.accent, k.accent_soft, k.text_faint, k.text_soft);
                    div()
                        .id(SharedString::from(format!(
                            "{prefix}-choice-{index}-{label}"
                        )))
                        .h(px(20.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .rounded(k.radius_s)
                        .cursor_pointer()
                        .map(|this| {
                            if selected {
                                this.bg(accent_soft).text_color(accent)
                            } else {
                                this.text_color(faint)
                                    .hover(move |style| style.text_color(soft))
                            }
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_review_choice(&pill_thread, &id, choice, cx)
                        }))
                        .child(label)
                });
                div()
                    .v_flex()
                    .gap(px(4.))
                    .p(px(8.))
                    .rounded(k.radius_m)
                    .bg(k.warning_soft)
                    .child(
                        div()
                            .h_flex()
                            .gap(px(6.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(k.text)
                                    .child(format!(
                                        "{} · {}",
                                        conflict.file.display(),
                                        conflict.place
                                    )),
                            )
                            .child(div().h_flex().gap(px(2.)).children(pills)),
                    )
                    .child(render_side("Yours", &conflict.main, k))
                    .child(render_side(words.theirs, &conflict.agent, k))
            });
        let (apply, discard) = (thread.to_string(), thread.to_string());
        card.children(words.title.map(|title| {
            div()
                .h_flex()
                .gap(px(6.))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(k.text)
                        .child(title),
                )
                .children(
                    words
                        .source
                        .clone()
                        .map(|source| div().text_color(k.text_faint).child(source)),
                )
        }))
        .child(div().text_color(k.text).child(words.intro))
        .children(
            review
                .note
                .clone()
                .map(|note| div().text_color(k.warning).child(note)),
        )
        .child(div().v_flex().children(files))
        .children(conflicts)
        .child(
            div()
                .h_flex()
                .gap(px(6.))
                .child(div().flex_1())
                .child(
                    Button::new(SharedString::from(format!("{prefix}-discard")))
                        .ghost()
                        .xsmall()
                        .label("Discard")
                        .disabled(busy)
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.discard_branch(&discard, cx)),
                        ),
                )
                .child(
                    Button::new(SharedString::from(format!("{prefix}-apply")))
                        .custom(accent_variant(k, cx))
                        .xsmall()
                        .label("Apply")
                        .loading(busy)
                        .on_click(cx.listener(move |this, _, _, cx| this.apply_review(&apply, cx))),
                ),
        )
        .into_any_element()
    }
}

/// "Applied · 3 files".
pub fn applied_line(files: usize) -> String {
    match files {
        1 => "Applied · 1 file".to_string(),
        n => format!("Applied · {n} files"),
    }
}

fn accent_variant(k: &Tokens, cx: &gpui_kit::App) -> ButtonCustomVariant {
    ButtonCustomVariant::new(cx)
        .color(k.accent)
        .foreground(k.on_accent)
        .hover(k.accent.opacity(0.88))
        .active(k.accent.opacity(0.76))
}

/// A file's diff lines, mono, added and removed lines tinted.
fn render_diff(lines: &[ReviewLine], k: &Tokens) -> AnyElement {
    let shown = lines.len().min(MAX_DIFF_LINES);
    div()
        .v_flex()
        .my(px(2.))
        .py(px(4.))
        .rounded(k.radius_s)
        .bg(k.surface)
        .font_family(k.mono_font.clone())
        .children(lines[..shown].iter().map(|line| {
            let (sign, color, bg) = match line.kind {
                ReviewLineKind::Added => ("+", k.success, Some(k.success_soft)),
                ReviewLineKind::Removed => ("−", k.danger, Some(k.danger_soft)),
                ReviewLineKind::Context => (" ", k.text_faint, None),
                ReviewLineKind::Gap => ("⋯", k.text_faint, None),
            };
            div()
                .h_flex()
                .px(px(6.))
                .gap(px(6.))
                .when_some(bg, |this, bg| this.bg(bg))
                .child(div().w(px(10.)).flex_none().text_color(color).child(sign))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(k.text)
                        .child(line.text.clone()),
                )
        }))
        .when(lines.len() > shown, |this| {
            this.child(
                div()
                    .px(px(6.))
                    .text_color(k.text_faint)
                    .child(format!("{} more lines", lines.len() - shown)),
            )
        })
        .into_any_element()
}

/// One side of a conflict, labelled.
fn render_side(label: &'static str, text: &str, k: &Tokens) -> AnyElement {
    let lines: Vec<&str> = text.lines().collect();
    let shown = lines.len().min(MAX_SIDE_LINES);
    let mut body: Vec<String> = lines[..shown]
        .iter()
        .map(|line| (*line).to_string())
        .collect();
    if lines.is_empty() {
        body.push("(nothing)".to_string());
    }
    if lines.len() > shown {
        body.push(format!("⋯ {} more lines", lines.len() - shown));
    }
    div()
        .v_flex()
        .gap(px(2.))
        .child(div().text_color(k.text_faint).child(label))
        .child(
            div()
                .v_flex()
                .px(px(6.))
                .py(px(4.))
                .rounded(k.radius_s)
                .bg(k.surface)
                .font_family(k.mono_font.clone())
                .text_color(k.text)
                .children(body.into_iter().map(|line| div().truncate().child(line))),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applied_lines_count_files() {
        assert_eq!(applied_line(1), "Applied · 1 file");
        assert_eq!(applied_line(3), "Applied · 3 files");
    }
}
