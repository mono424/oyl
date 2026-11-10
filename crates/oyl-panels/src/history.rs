//! The history panel: versions of the paper, what each changed, restore.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use chrono::Local;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Icon, IconName, Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, EventEmitter, FocusHandle, Focusable, FontWeight, HighlightStyle,
    ListAlignment, ListState, Pixels, StyledText, Subscription, Window, div, list, px,
};
use oyl_history::{FileStatus, History, LineKind, Version, VersionId};
use oyl_model::Author;
use oyl_ui::Tokens;
use oyl_ui::prelude::*;

use crate::model::history::{
    DiffLineRow, DiffRow, HistoryRow, VersionRow, day_label, diff_rows, diff_summary,
    history_rows_now, line_counts, status_label,
};
use crate::ui::{self, PAD_X, Tone};
use crate::{Confirm, Dismiss, KEY_CONTEXT, SelectNext, SelectPrev};

/// What the history panel tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryEvent {
    /// The paper was brought back to a version: reload the open files.
    Restored {
        /// The number of the version brought back.
        version_number: u32,
    },
    /// The panel asks to be closed (Esc, or its close button).
    Close,
}

/// Which changes the diff shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Compare {
    /// What the version changed against the one before it.
    ThisVersion,
    /// What changed from the version to the files now.
    ToNow,
}

/// The diff of the selected version.
enum DiffView {
    Loading,
    Ready {
        rows: Rc<Vec<DiffRow>>,
        summary: String,
    },
    Failed(String),
}

/// The width of the version list beside a diff.
const LIST_WIDTH: f32 = 360.;
/// The width of the diff beside the list.
const DIFF_WIDTH: f32 = 680.;

/// The versions of the paper, newest first and grouped by day; selecting
/// one shows what it changed (or how it differs from now), and any version
/// can be restored — which adds a new version, so nothing is lost.
///
/// Call [`refresh`](Self::refresh) when the history may have grown (after
/// saves and agent turns); subscribe to [`HistoryEvent`]s. The card is
/// wider while a diff is shown: see [`preferred_width`](Self::preferred_width).
pub struct HistoryPanel {
    root: PathBuf,
    focus: FocusHandle,
    history: Option<Arc<History>>,
    loaded: bool,
    error: Option<String>,
    versions: Vec<Version>,
    rows: Rc<Vec<HistoryRow>>,
    list: ListState,
    selected: Option<VersionId>,
    compare: Compare,
    diff: Option<DiffView>,
    diff_list: ListState,
    diff_generation: u64,
    load_generation: u64,
    confirm_restore: bool,
    restoring: bool,
    notice: Option<(Tone, String)>,
    _subscriptions: Vec<Subscription>,
}

impl HistoryPanel {
    /// The history of the paper at `root`, read in the background.
    pub fn new(root: PathBuf, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::init(cx);
        let mut panel = HistoryPanel {
            root,
            focus: cx.focus_handle(),
            history: None,
            loaded: false,
            error: None,
            versions: Vec::new(),
            rows: Rc::new(Vec::new()),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            selected: None,
            compare: Compare::ThisVersion,
            diff: None,
            diff_list: ListState::new(0, ListAlignment::Top, px(600.)),
            diff_generation: 0,
            load_generation: 0,
            confirm_restore: false,
            restoring: false,
            notice: None,
            _subscriptions: vec![cx.observe_global::<oyl_ui::OylTheme>(|_, cx| cx.notify())],
        };
        panel.refresh(cx);
        panel
    }

    /// The width the card should have: wider while a version's changes are
    /// shown beside the list. Observe the panel (`cx.observe`) to follow it.
    pub fn preferred_width(&self) -> Pixels {
        if self.selected.is_some() {
            px(LIST_WIDTH + DIFF_WIDTH + 1.)
        } else {
            px(420.)
        }
    }

    /// Reads the versions again (and the shown diff).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.load_generation += 1;
        let generation = self.load_generation;
        let root = self.root.clone();
        let existing = self.history.clone();
        let task = cx.background_spawn(async move {
            let history = match existing {
                Some(history) => history,
                None => Arc::new(History::open(&root).map_err(|e| e.to_string())?),
            };
            let versions = history.versions().map_err(|e| e.to_string())?;
            Ok::<_, String>((history, versions))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if generation != this.load_generation {
                    return;
                }
                this.loaded = true;
                match result {
                    Ok((history, versions)) => {
                        this.history = Some(history);
                        this.error = None;
                        this.set_versions(versions, cx);
                    }
                    Err(message) => this.error = Some(message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The versions, newest first.
    pub fn versions(&self) -> &[Version] {
        &self.versions
    }

    /// The number of the selected version.
    pub fn selected_number(&self) -> Option<u32> {
        let id = self.selected?;
        self.versions.iter().find(|v| v.id == id).map(|v| v.number)
    }

    /// Whether restoring waits for confirmation.
    pub fn is_confirming_restore(&self) -> bool {
        self.confirm_restore
    }

    /// The rows of the diff shown, once loaded.
    pub fn diff_rows(&self) -> Option<&[DiffRow]> {
        match &self.diff {
            Some(DiffView::Ready { rows, .. }) => Some(rows),
            _ => None,
        }
    }

    /// Selects the version with this number and shows its changes.
    pub fn select_number(&mut self, number: u32, cx: &mut Context<Self>) {
        if let Some(id) = self.versions.iter().find(|v| v.number == number).map(|v| v.id) {
            self.select(Some(id), cx);
        }
    }

    // -- State --------------------------------------------------------------

    fn set_versions(&mut self, versions: Vec<Version>, cx: &mut Context<Self>) {
        self.rows = Rc::new(history_rows_now(&versions));
        self.versions = versions;
        if let Some(id) = self.selected
            && !self.versions.iter().any(|v| v.id == id)
        {
            self.selected = None;
            self.diff = None;
            self.confirm_restore = false;
        }
        ui::sync_list(&self.list, self.rows.len(), true);
        if self.selected.is_some() {
            self.load_diff(cx);
        }
        cx.notify();
    }

    fn row_index(&self, id: VersionId) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| matches!(row, HistoryRow::Version(v) if v.id == id))
    }

    fn select(&mut self, id: Option<VersionId>, cx: &mut Context<Self>) {
        if self.selected == id {
            return;
        }
        self.selected = id;
        self.confirm_restore = false;
        self.notice = None;
        match id {
            Some(id) => {
                if let Some(ix) = self.row_index(id) {
                    self.list.scroll_to_reveal_item(ix);
                }
                self.load_diff(cx);
            }
            None => {
                self.diff = None;
                self.diff_generation += 1;
            }
        }
        cx.notify();
    }

    pub(crate) fn set_compare(&mut self, compare: Compare, cx: &mut Context<Self>) {
        if self.compare != compare {
            self.compare = compare;
            self.load_diff(cx);
            cx.notify();
        }
    }

    fn load_diff(&mut self, cx: &mut Context<Self>) {
        let (Some(id), Some(history)) = (self.selected, self.history.clone()) else {
            return;
        };
        self.diff_generation += 1;
        let generation = self.diff_generation;
        let compare = self.compare;
        if !matches!(self.diff, Some(DiffView::Ready { .. })) {
            self.diff = Some(DiffView::Loading);
        }
        let task = cx.background_spawn(async move {
            let files = match compare {
                Compare::ThisVersion => history.diff(id),
                Compare::ToNow => history.diff_to_working(id),
            }
            .map_err(|e| e.to_string())?;
            Ok::<_, String>((diff_rows(&files), diff_summary(&files)))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if generation != this.diff_generation {
                    return;
                }
                this.diff = Some(match result {
                    Ok((rows, summary)) => {
                        ui::sync_list(&this.diff_list, rows.len(), false);
                        DiffView::Ready {
                            rows: Rc::new(rows),
                            summary,
                        }
                    }
                    Err(message) => DiffView::Failed(message),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn move_selection(&mut self, direction: isize, cx: &mut Context<Self>) {
        let rows = self.rows.clone();
        let current = self.selected.and_then(|id| self.row_index(id));
        let next = ui::step_selection(rows.len(), current, direction, |ix| {
            matches!(rows[ix], HistoryRow::Version(_))
        });
        if let Some(HistoryRow::Version(row)) = next.and_then(|ix| rows.get(ix)) {
            self.select(Some(row.id), cx);
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if self.selected.is_none() {
            self.move_selection(1, cx);
        } else if self.confirm_restore {
            self.restore(cx);
        } else {
            self.confirm_restore = true;
            cx.notify();
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.confirm_restore {
            self.confirm_restore = false;
            cx.notify();
        } else if self.selected.is_some() {
            self.select(None, cx);
        } else {
            cx.emit(HistoryEvent::Close);
        }
    }

    /// Brings the paper back to the selected version (after confirmation).
    fn restore(&mut self, cx: &mut Context<Self>) {
        let (Some(id), Some(history)) = (self.selected, self.history.clone()) else {
            return;
        };
        if self.restoring {
            return;
        }
        let Some(number) = self.selected_number() else {
            return;
        };
        let latest_before = self.versions.first().map(|v| v.number);
        self.restoring = true;
        cx.notify();
        let task = cx.background_spawn(async move {
            let restored = history.restore(id).map_err(|e| e.to_string())?;
            let versions = history.versions().map_err(|e| e.to_string())?;
            Ok::<_, String>((restored, versions))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.restoring = false;
                this.confirm_restore = false;
                match result {
                    Ok((restored, versions)) => {
                        let wrote = Some(restored.number) != latest_before;
                        this.notice = Some((
                            Tone::Quiet,
                            if wrote {
                                format!(
                                    "Restored version {number}, as version {}. Nothing was lost.",
                                    restored.number
                                )
                            } else {
                                format!("The paper already is as in version {number}.")
                            },
                        ));
                        this.selected = None;
                        this.diff = None;
                        this.set_versions(versions, cx);
                        if wrote {
                            cx.emit(HistoryEvent::Restored {
                                version_number: number,
                            });
                        }
                    }
                    Err(message) => this.notice = Some((Tone::Error, message)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Records the paper as it is now ("Save a version now").
    fn save_now(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let existing = self.history.clone();
        let task = cx.background_spawn(async move {
            let history = match existing {
                Some(history) => history,
                None => Arc::new(History::open(&root).map_err(|e| e.to_string())?),
            };
            let made = history
                .snapshot(Author::You, Some("Saved by hand"))
                .map_err(|e| e.to_string())?;
            let versions = history.versions().map_err(|e| e.to_string())?;
            Ok::<_, String>((history, made.is_some(), versions))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok((history, made, versions)) => {
                        this.history = Some(history);
                        this.notice = (!made).then(|| {
                            (
                                Tone::Quiet,
                                "Nothing to keep yet: the paper has no text files.".to_string(),
                            )
                        });
                        this.set_versions(versions, cx);
                    }
                    Err(message) => this.notice = Some((Tone::Error, message)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // -- Rendering ----------------------------------------------------------

    fn render_row(&mut self, ix: usize, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else {
            return div().into_any_element();
        };
        let inner = match row {
            HistoryRow::Day(label) => ui::group_caption(label, cx).into_any_element(),
            HistoryRow::Version(version) => {
                let selected = self.selected == Some(version.id);
                self.render_version(ix, &version, selected, cx)
            }
        };
        let last = ix + 1 == self.rows.len();
        div()
            .px(px(PAD_X - 10.))
            .when(last, |d| d.pb(px(16.)))
            .child(inner)
            .into_any_element()
    }

    fn render_version(
        &self,
        ix: usize,
        version: &VersionRow,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let k = cx.tokens().clone();
        let id = version.id;
        let counts = counts_element(version.added, version.removed, &k);
        let who = author_element(version.author, version.quiet, &k);
        let row = ui::row(("version", ix), selected, cx).on_click(cx.listener(move |this, _, _, cx| {
            this.select(Some(id), cx)
        }));
        if version.quiet {
            return row
                .py(px(4.))
                .h_flex()
                .gap(px(8.))
                .text_size(px(12.))
                .child(
                    div()
                        .flex_none()
                        .text_color(k.text_soft)
                        .child(version.time.clone()),
                )
                .child(who)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(k.text_faint)
                        .child(version.files.clone()),
                )
                .child(counts)
                .into_any_element();
        }
        let detail = match &version.label {
            Some(label) => label.clone(),
            None => format!("Version {}", version.number),
        };
        let meta = if version.label.is_some() {
            format!("Version {} · {}", version.number, version.files)
        } else {
            version.files.clone()
        };
        row.v_flex()
            .gap(px(1.))
            .child(
                div()
                    .h_flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(k.text)
                            .child(version.time.clone()),
                    )
                    .child(who)
                    .when(version.latest, |line| {
                        line.child(oyl_ui::chip("Latest", cx).h(px(17.)).text_size(px(10.5)))
                    })
                    .child(div().flex_1())
                    .child(counts),
            )
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(k.text_soft)
                    .line_clamp(2)
                    .child(detail),
            )
            .when(!meta.is_empty(), |col| {
                col.child(
                    div()
                        .text_size(px(11.5))
                        .text_color(k.text_faint)
                        .truncate()
                        .child(meta),
                )
            })
            .into_any_element()
    }

    fn render_diff_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        let k = cx.tokens().clone();
        let Some(version) = self
            .selected
            .and_then(|id| self.versions.iter().find(|v| v.id == id))
        else {
            return div().into_any_element();
        };
        let when = {
            let local = version.updated.with_timezone(&Local);
            let day = day_label(local.date_naive(), Local::now().date_naive());
            format!("{day} {}", local.format("%H:%M"))
        };
        let heading = div()
            .h_flex()
            .gap(px(8.))
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!("Version {}", version.number)),
            )
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(k.text_faint)
                    .child(format!("{when} · {}", version.author.label())),
            );
        let switch = ui::segmented(
            "history-compare",
            &["Changes in this version", "Compared to now"],
            match self.compare {
                Compare::ThisVersion => 0,
                Compare::ToNow => 1,
            },
            {
                let this = cx.entity().downgrade();
                Rc::new(move |choice, _, cx| {
                    let compare = if choice == 0 {
                        Compare::ThisVersion
                    } else {
                        Compare::ToNow
                    };
                    this.update(cx, |this, cx| this.set_compare(compare, cx)).ok();
                })
            },
            cx,
        );
        let restore: AnyElement = if self.confirm_restore {
            div()
                .v_flex()
                .gap(px(8.))
                .child(ui::notice(
                    Tone::Quiet,
                    format!(
                        "Bring the paper back to version {}? Nothing is lost: restoring adds a new version, and the paper as it is now stays in the history.",
                        version.number
                    ),
                    cx,
                ))
                .child(
                    div()
                        .h_flex()
                        .justify_end()
                        .gap(px(6.))
                        .child(
                            Button::new("restore-cancel")
                                .ghost()
                                .small()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_restore = false;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("restore-confirm")
                                .primary()
                                .small()
                                .loading(self.restoring)
                                .label(format!("Restore version {}", version.number))
                                .on_click(cx.listener(|this, _, _, cx| this.restore(cx))),
                        ),
                )
                .into_any_element()
        } else {
            Button::new("restore")
                .small()
                .icon(IconName::Undo2)
                .label("Restore this version")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.confirm_restore = true;
                    cx.notify();
                }))
                .into_any_element()
        };
        let mut restore_slot = Some(restore);
        let body: AnyElement = match &self.diff {
            None | Some(DiffView::Loading) => div()
                .flex_1()
                .h_flex()
                .justify_center()
                .pt(px(40.))
                .child(Spinner::new().color(k.text_faint))
                .into_any_element(),
            Some(DiffView::Failed(message)) => div()
                .p(px(PAD_X))
                .child(ui::notice(Tone::Error, message.clone(), cx))
                .into_any_element(),
            Some(DiffView::Ready { rows, .. }) if rows.is_empty() => div()
                .p(px(PAD_X))
                .text_size(px(13.))
                .text_color(k.text_faint)
                .child(match self.compare {
                    Compare::ThisVersion => "This version changed no text.",
                    Compare::ToNow => "The paper is the same as in this version.",
                })
                .into_any_element(),
            Some(DiffView::Ready { .. }) => div()
                .flex_1()
                .min_h_0()
                .relative()
                .child(list(self.diff_list.clone(), cx.processor(Self::render_diff_row)).size_full())
                .vertical_scrollbar(&self.diff_list)
                .into_any_element(),
        };
        let summary = match &self.diff {
            Some(DiffView::Ready { summary, rows }) if !rows.is_empty() => Some(summary.clone()),
            _ => None,
        };
        div()
            .v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .child(
                div()
                    .v_flex()
                    .flex_none()
                    .gap(px(10.))
                    .px(px(PAD_X))
                    .pb(px(10.))
                    .child(heading)
                    .child(
                        div()
                            .h_flex()
                            .gap(px(10.))
                            .child(switch)
                            .child(div().flex_1())
                            .children((!self.confirm_restore).then(|| restore_slot.take()).flatten()),
                    )
                    .children(restore_slot.take())
                    .children(summary.map(|summary| {
                        div()
                            .text_size(px(12.))
                            .text_color(k.text_faint)
                            .child(summary)
                    })),
            )
            .child(oyl_ui::hairline(cx))
            .child(body)
            .into_any_element()
    }

    fn render_diff_row(&mut self, ix: usize, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let k = cx.tokens().clone();
        let Some(DiffView::Ready { rows, .. }) = &self.diff else {
            return div().into_any_element();
        };
        let Some(row) = rows.get(ix).cloned() else {
            return div().into_any_element();
        };
        let last = ix + 1 == rows.len();
        let element = match row {
            DiffRow::File {
                path,
                status,
                added,
                removed,
            } => {
                let (chip_bg, chip_fg) = match status {
                    FileStatus::Added => (k.success_soft, k.success),
                    FileStatus::Deleted => (k.danger_soft, k.danger),
                    FileStatus::Modified => (k.surface_hover, k.text_soft),
                };
                div()
                    .pt(px(if ix == 0 { 10. } else { 18. }))
                    .px(px(PAD_X - 6.))
                    .child(
                        div()
                            .h_flex()
                            .gap(px(8.))
                            .px(px(8.))
                            .py(px(5.))
                            .rounded(k.radius_s)
                            .bg(k.surface)
                            .child(
                                oyl_ui::chip(status_label(status), cx)
                                    .bg(chip_bg)
                                    .text_color(chip_fg),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(k.mono_font.clone())
                                    .text_size(px(12.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(path),
                            )
                            .child(counts_element(added, removed, &k)),
                    )
                    .into_any_element()
            }
            DiffRow::Hunk(label) => div()
                .px(px(PAD_X))
                .pt(px(10.))
                .pb(px(3.))
                .text_size(px(11.5))
                .text_color(k.text_faint)
                .child(label)
                .into_any_element(),
            DiffRow::Line(line) => diff_line(&line, &k),
        };
        div()
            .when(last, |d| d.pb(px(20.)))
            .child(element)
            .into_any_element()
    }
}

/// "+12 −3" with the meaning colors.
fn counts_element(added: u32, removed: u32, k: &Tokens) -> AnyElement {
    if line_counts(added, removed).is_empty() {
        return div().into_any_element();
    }
    div()
        .h_flex()
        .flex_none()
        .gap(px(5.))
        .text_size(px(11.5))
        .when(added > 0, |d| {
            d.child(div().text_color(k.success).child(format!("+{added}")))
        })
        .when(removed > 0, |d| {
            d.child(div().text_color(k.danger).child(format!("−{removed}")))
        })
        .into_any_element()
}

/// "You", "Agent" (in the accent, with its mark), "Oyl".
fn author_element(author: Author, quiet: bool, k: &Tokens) -> AnyElement {
    let size = if quiet { 12. } else { 12.5 };
    match author {
        Author::Agent => div()
            .h_flex()
            .flex_none()
            .gap(px(4.))
            .text_size(px(size))
            .text_color(k.accent)
            .child(Icon::new(IconName::Bot).xsmall().text_color(k.accent))
            .child("Agent")
            .into_any_element(),
        Author::You => div()
            .flex_none()
            .text_size(px(size))
            .text_color(if quiet { k.text_faint } else { k.text_soft })
            .child("You")
            .into_any_element(),
        Author::System => div()
            .flex_none()
            .text_size(px(size))
            .text_color(k.text_faint)
            .child("Oyl")
            .into_any_element(),
    }
}

/// One line of a hunk: line numbers, a sign, and the text wrapped, tinted
/// by its kind, with the changed words emphasized.
fn diff_line(line: &DiffLineRow, k: &Tokens) -> AnyElement {
    let (bg, sign, word_bg, word_fg) = match line.kind {
        LineKind::Add => (Some(k.success_soft), "+", k.success_soft, k.success),
        LineKind::Remove => (Some(k.danger_soft), "−", k.danger_soft, k.danger),
        LineKind::Context => (None, "", k.surface, k.text),
    };
    let number = |n: Option<u32>| {
        div()
            .w(px(34.))
            .flex_none()
            .text_right()
            .pr(px(6.))
            .text_color(k.text_faint)
            .child(n.map(|n| n.to_string()).unwrap_or_default())
    };
    let text = if line.text.is_empty() {
        " ".to_string()
    } else {
        line.text.clone()
    };
    let highlights: Vec<_> = line
        .changes
        .iter()
        .filter(|range| range.start < range.end && range.end <= text.len())
        .map(|range| {
            (
                range.clone(),
                HighlightStyle {
                    background_color: Some(word_bg),
                    color: Some(word_fg),
                    ..HighlightStyle::default()
                },
            )
        })
        .collect();
    let styled = StyledText::new(text).with_highlights(highlights);
    div()
        .h_flex()
        .items_start()
        .mx(px(PAD_X - 6.))
        .px(px(2.))
        .py(px(1.))
        .rounded(px(3.))
        .when_some(bg, |row, bg| row.bg(bg))
        .font_family(k.mono_font.clone())
        .text_size(px(12.))
        .line_height(px(19.))
        .child(number(line.old))
        .child(number(line.new))
        .child(
            div()
                .w(px(14.))
                .flex_none()
                .text_color(match line.kind {
                    LineKind::Add => k.success,
                    LineKind::Remove => k.danger,
                    LineKind::Context => k.text_faint,
                })
                .child(sign),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(match line.kind {
                    LineKind::Context => k.text_soft,
                    _ => k.text,
                })
                .child(styled)
                .when(line.missing_newline, |col| {
                    col.child(
                        div()
                            .font_family(k.ui_font.clone())
                            .text_size(px(11.))
                            .text_color(k.text_faint)
                            .child("No line break at the end"),
                    )
                }),
        )
        .into_any_element()
}

impl Render for HistoryPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let k = cx.tokens().clone();
        let this = cx.entity().downgrade();
        let count = (!self.versions.is_empty()).then(|| self.versions.len().to_string());
        let header = ui::header(
            "History",
            count,
            Vec::new(),
            Rc::new(move |_, cx| {
                this.update(cx, |_, cx| cx.emit(HistoryEvent::Close)).ok();
            }),
            cx,
        );
        let notice = self
            .notice
            .clone()
            .map(|(tone, text)| div().px(px(PAD_X)).pb(px(8.)).child(ui::notice(tone, text, cx)));
        let list_column = if !self.loaded {
            div().flex_1().into_any_element()
        } else if let Some(error) = &self.error {
            div()
                .px(px(PAD_X))
                .child(ui::notice(Tone::Error, error.clone(), cx))
                .into_any_element()
        } else if self.versions.is_empty() {
            ui::empty_state(
                "No versions yet. Oyl keeps one every time the paper changes.",
                Some(
                    Button::new("history-save-now")
                        .small()
                        .label("Save a version now")
                        .on_click(cx.listener(|this, _, _, cx| this.save_now(cx)))
                        .into_any_element(),
                ),
                cx,
            )
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
        let showing_diff = self.selected.is_some();
        let left = div()
            .v_flex()
            .h_full()
            .min_h_0()
            .when(showing_diff, |col| col.w(px(LIST_WIDTH)).flex_none())
            .when(!showing_diff, |col| col.flex_1())
            .children(notice)
            .child(list_column);
        div()
            .id("oyl-history")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .v_flex()
            .font_family(k.ui_font.clone())
            .text_color(k.text)
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &Confirm, _, cx| this.confirm(cx)))
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.dismiss(cx)))
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .h_flex()
                    .items_start()
                    .child(left)
                    .when(showing_diff, |row| {
                        row.child(div().w(px(1.)).h_full().bg(k.hairline))
                            .child(self.render_diff_pane(cx))
                    }),
            )
    }
}

impl Focusable for HistoryPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<HistoryEvent> for HistoryPanel {}
