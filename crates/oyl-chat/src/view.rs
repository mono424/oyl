//! Drawing one turn of the transcript: the user's bubble, the agent's
//! streamed Markdown, thinking, tool rows, file chips and the footer.
//!
//! Rows are drawn from a snapshot of the turn, so the virtual list can
//! render them without holding the pane borrowed; clicks go back to the
//! pane through a weak handle.

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::shimmer::ShimmerText;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{Disableable as _, Icon, IconName, Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, ClickEvent, SharedString, WeakEntity, Window, div, px, relative};
use oyl_ui::Tokens;
use oyl_ui::prelude::*;

use crate::pane::ChatPane;
use crate::thread::{
    Block, COLLAPSE_TOOLS_ABOVE, Segment, ToolCall, ToolGroupSummary, ToolKind, ToolState, Turn,
    TurnStatus, file_label, files_summary, format_cost, preview, segments, visible_tool_rows,
};

/// What is expanded in the transcript. Not persisted.
#[derive(Clone, Debug, Default)]
pub(crate) struct ViewState {
    pub open_tools: HashSet<String>,
    pub open_groups: HashSet<String>,
    pub open_thinking: HashSet<String>,
    pub full_output: HashSet<String>,
}

/// Which disclosure a click toggles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Disclosure {
    Tool,
    Group,
    Thinking,
    Output,
}

impl ViewState {
    pub fn toggle(&mut self, disclosure: Disclosure, key: &str) {
        let set = match disclosure {
            Disclosure::Tool => &mut self.open_tools,
            Disclosure::Group => &mut self.open_groups,
            Disclosure::Thinking => &mut self.open_thinking,
            Disclosure::Output => &mut self.full_output,
        };
        if !set.remove(key) {
            set.insert(key.to_string());
        }
    }
}

/// Everything one transcript row needs.
pub(crate) struct RowSnapshot {
    pub turn: Turn,
    pub ui: Rc<ViewState>,
    /// The turn is the one being run right now.
    pub live: bool,
    /// The user pressed Stop and the agent is winding down.
    pub stopping: bool,
    /// Some turn is running, so undo and retry wait.
    pub busy: bool,
    pub pane: WeakEntity<ChatPane>,
}

/// Lines of tool output shown before "Show more".
const OUTPUT_PREVIEW_LINES: usize = 12;
const OUTPUT_PREVIEW_CHARS: usize = 1600;
const DETAIL_PREVIEW_LINES: usize = 6;
const DETAIL_PREVIEW_CHARS: usize = 600;

fn id(parts: &[&str]) -> SharedString {
    SharedString::from(format!("chat-{}", parts.join("-")))
}

fn on_pane(
    pane: &WeakEntity<ChatPane>,
    action: impl Fn(&mut ChatPane, &mut gpui_kit::Context<ChatPane>) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let pane = pane.clone();
    move |_, _, cx| {
        let _ = pane.update(cx, |pane, cx| action(pane, cx));
    }
}

fn toggle(
    pane: &WeakEntity<ChatPane>,
    disclosure: Disclosure,
    key: String,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    on_pane(pane, move |pane, cx| {
        pane.toggle_disclosure(disclosure, &key, cx)
    })
}

fn tool_icon(kind: ToolKind) -> IconName {
    match kind {
        ToolKind::Edit => IconName::FileText,
        ToolKind::Read => IconName::Eye,
        ToolKind::Run => IconName::SquareTerminal,
        ToolKind::Search => IconName::Search,
        ToolKind::Web => IconName::Globe,
        ToolKind::Other => IconName::Asterisk,
    }
}

/// Draws one turn.
pub(crate) fn render_turn(row: RowSnapshot, cx: &mut App) -> AnyElement {
    let k = cx.tokens().clone();
    let turn = &row.turn;
    let mut body: Vec<AnyElement> = Vec::new();

    body.push(user_message(turn, &k, cx));

    let last = turn.blocks.len().saturating_sub(1);
    let live_block = |index: usize| row.live && !row.stopping && turn.is_running() && index == last;

    for segment in segments(&turn.blocks) {
        let element = match segment {
            Segment::Text(index) => match &turn.blocks[index] {
                Block::Text { text } => Some(markdown(
                    id(&["md", &turn.id, &index.to_string()]),
                    text,
                    live_block(index),
                    &k,
                )),
                _ => None,
            },
            Segment::Thinking(index) => match &turn.blocks[index] {
                Block::Thinking { text } => {
                    Some(thinking(&row, index, text, live_block(index), &k))
                }
                _ => None,
            },
            Segment::Status(index) => match &turn.blocks[index] {
                Block::Status { message } => Some(status_note(message, live_block(index), &k)),
                _ => None,
            },
            Segment::Tools { tools, files } => Some(tool_group(&row, &tools, &files, &k)),
            Segment::Files(files) => Some(file_chips(&row, &files, &k)),
        };
        body.extend(element);
    }

    if !turn.is_running()
        && !turn.has_text()
        && let Some(summary) = turn.summary.as_deref().filter(|s| !s.trim().is_empty())
    {
        body.push(markdown(id(&["summary", &turn.id]), summary, false, &k));
    }

    if row.live {
        let waiting = match turn.blocks.last() {
            None | Some(Block::FileChanged { .. }) => true,
            Some(Block::Tool(tool)) => tool.state != ToolState::Running,
            Some(Block::Text { .. } | Block::Thinking { .. } | Block::Status { .. }) => false,
        };
        if row.stopping {
            body.push(working_note("Stopping…", &k));
        } else if waiting {
            body.push(working_note("Working…", &k));
        }
    }

    if let Some(footer) = footer(&row, &k) {
        body.push(footer);
    }

    v_flex_full().gap(px(10.)).children(body).into_any_element()
}

fn v_flex_full() -> gpui_kit::Div {
    div().v_flex().w_full().min_w_0()
}

fn user_message(turn: &Turn, k: &Tokens, cx: &App) -> AnyElement {
    let mut meta: Vec<String> = turn.context.clone();
    meta.extend(turn.attachments.iter().map(|path| file_label(path)));
    div()
        .v_flex()
        .w_full()
        .items_end()
        .gap(px(4.))
        .child(
            oyl_ui::surface(cx)
                .max_w(relative(0.88))
                .px(px(12.))
                .py(px(8.))
                .text_sm()
                .text_color(k.text)
                .child(turn.prompt.clone()),
        )
        .when(!meta.is_empty(), |this| {
            this.child(
                div()
                    .max_w(relative(0.88))
                    .px(px(4.))
                    .text_xs()
                    .text_color(k.text_faint)
                    .truncate()
                    .child(meta.join(" · ")),
            )
        })
        .into_any_element()
}

fn markdown(id: SharedString, text: &str, streaming: bool, k: &Tokens) -> AnyElement {
    div()
        .w_full()
        .min_w_0()
        .text_sm()
        .text_color(k.text)
        .child(
            TextView::markdown(id, text.to_string())
                .selectable(true)
                .stream_fade(streaming),
        )
        .into_any_element()
}

fn working_note(label: &'static str, k: &Tokens) -> AnyElement {
    div()
        .h_flex()
        .h(px(22.))
        .text_sm()
        .child(ShimmerText::new(label).text_color(k.text_faint))
        .into_any_element()
}

fn thinking(row: &RowSnapshot, index: usize, text: &str, live: bool, k: &Tokens) -> AnyElement {
    if live {
        return working_note("Thinking…", k);
    }
    let key = format!("{}/{}", row.turn.id, index);
    let open = row.ui.open_thinking.contains(&key);
    let soft = k.text_soft;
    div()
        .v_flex()
        .w_full()
        .child(
            div()
                .id(id(&["thinking", &row.turn.id, &index.to_string()]))
                .h_flex()
                .h(px(22.))
                .gap(px(4.))
                .text_sm()
                .text_color(k.text_faint)
                .cursor_pointer()
                .hover(move |style| style.text_color(soft))
                .child("Thought")
                .child(
                    Icon::new(if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size(px(12.)),
                )
                .on_click(toggle(&row.pane, Disclosure::Thinking, key)),
        )
        .when(open, |this| {
            this.child(
                div()
                    .mt(px(2.))
                    .pl(px(10.))
                    .border_l_1()
                    .border_color(k.hairline)
                    .text_sm()
                    .text_color(k.text_faint)
                    .child(
                        TextView::markdown(
                            id(&["thought", &row.turn.id, &index.to_string()]),
                            text.to_string(),
                        )
                        .selectable(true),
                    ),
            )
        })
        .into_any_element()
}

fn status_note(message: &str, live: bool, k: &Tokens) -> AnyElement {
    div()
        .h_flex()
        .gap(px(6.))
        .text_xs()
        .text_color(k.text_faint)
        .child(oyl_ui::dot(k.text_faint))
        .child(if live {
            ShimmerText::new(message.to_string())
                .text_color(k.text_faint)
                .into_any_element()
        } else {
            div().child(message.to_string()).into_any_element()
        })
        .into_any_element()
}

fn tool_group(row: &RowSnapshot, tools: &[usize], files: &[usize], k: &Tokens) -> AnyElement {
    let calls: Vec<&ToolCall> = tools
        .iter()
        .filter_map(|&index| match row.turn.blocks.get(index) {
            Some(Block::Tool(tool)) => Some(tool),
            _ => None,
        })
        .collect();
    let summary = ToolGroupSummary::of(calls.iter().copied());
    let first = tools.first().copied().unwrap_or_default();
    let key = format!("{}/g{}", row.turn.id, first);
    let open = row.ui.open_groups.contains(&key);
    let collapsible = calls.len() > COLLAPSE_TOOLS_ABOVE;
    let running = summary.running > 0 && row.live && !row.stopping;
    let visible = visible_tool_rows(calls.len(), running, open);
    let hover = k.surface_hover;

    div()
        .v_flex()
        .w_full()
        .gap(px(1.))
        .when(collapsible, |this| {
            this.child(
                div()
                    .id(id(&["group", &row.turn.id, &first.to_string()]))
                    .h_flex()
                    .h(px(24.))
                    .gap(px(8.))
                    .px(px(6.))
                    .rounded(k.radius_s)
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size(px(13.))
                        .text_color(k.text_faint),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(k.text_soft)
                            .child(summary.label()),
                    )
                    .when(running, |this| {
                        this.child(Spinner::new().xsmall().color(k.text_faint))
                    })
                    .when(!running && summary.failed > 0, |this| {
                        this.child(oyl_ui::dot(k.danger))
                    })
                    .on_click(toggle(&row.pane, Disclosure::Group, key)),
            )
        })
        .children(
            calls[visible]
                .iter()
                .map(|tool| tool_row(row, tool, collapsible, k)),
        )
        .when(!files.is_empty(), |this| {
            this.child(div().pt(px(4.)).child(file_chips(row, files, k)))
        })
        .into_any_element()
}

fn tool_row(row: &RowSnapshot, tool: &ToolCall, nested: bool, k: &Tokens) -> AnyElement {
    let key = format!("{}/{}", row.turn.id, tool.id);
    let open = row.ui.open_tools.contains(&key);
    let full = row.ui.full_output.contains(&key);
    let has_body = tool.has_body();
    let hover = k.surface_hover;
    let spinning = tool.state == ToolState::Running && row.live && !row.stopping;

    let state: AnyElement = if spinning {
        Spinner::new()
            .xsmall()
            .color(k.text_faint)
            .into_any_element()
    } else {
        match tool.state {
            ToolState::Succeeded => Icon::new(IconName::Check)
                .size(px(12.))
                .text_color(k.success)
                .into_any_element(),
            ToolState::Failed => Icon::new(IconName::Close)
                .size(px(12.))
                .text_color(k.danger)
                .into_any_element(),
            ToolState::Running | ToolState::Abandoned => Icon::new(IconName::Minus)
                .size(px(12.))
                .text_color(k.text_faint)
                .into_any_element(),
        }
    };

    div()
        .v_flex()
        .w_full()
        .when(nested, |this| this.pl(px(14.)))
        .child(
            div()
                .id(id(&["tool", &row.turn.id, &tool.id]))
                .h_flex()
                .h(px(24.))
                .gap(px(8.))
                .px(px(6.))
                .rounded(k.radius_s)
                .when(has_body, |this| {
                    this.cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .on_click(toggle(&row.pane, Disclosure::Tool, key.clone()))
                })
                .child(
                    Icon::new(tool_icon(tool.kind()))
                        .size(px(13.))
                        .text_color(k.text_faint),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(k.text_soft)
                        .child(tool.title.clone()),
                )
                .child(state),
        )
        .when(open && has_body, |this| {
            this.child(tool_body(row, tool, &key, full, k))
        })
        .into_any_element()
}

fn tool_body(row: &RowSnapshot, tool: &ToolCall, key: &str, full: bool, k: &Tokens) -> AnyElement {
    let detail = tool
        .detail
        .as_deref()
        .filter(|detail| !detail.trim().is_empty())
        .map(|detail| preview(detail, DETAIL_PREVIEW_LINES, DETAIL_PREVIEW_CHARS).0);
    let output = tool
        .output
        .as_deref()
        .filter(|output| !output.trim().is_empty())
        .map(|output| {
            let (head, cut) = preview(output, OUTPUT_PREVIEW_LINES, OUTPUT_PREVIEW_CHARS);
            if full {
                (output.trim_end().to_string(), cut)
            } else {
                (head, cut)
            }
        });
    let soft = k.text_soft;

    div()
        .v_flex()
        .ml(px(27.))
        .mr(px(6.))
        .mt(px(2.))
        .mb(px(6.))
        .gap(px(6.))
        .p(px(8.))
        .min_w_0()
        .overflow_hidden()
        .rounded(k.radius_m)
        .bg(k.surface)
        .border_1()
        .border_color(k.hairline)
        .font_family(k.mono_font.clone())
        .text_xs()
        .text_color(k.text_soft)
        .when_some(detail, |this, detail| {
            this.child(div().text_color(k.text_faint).child(detail))
        })
        .when_some(output, |this, (text, cut)| {
            this.child(div().child(text)).when(cut, |this| {
                this.child(
                    div()
                        .id(id(&["more", &row.turn.id, &tool.id]))
                        .font_family(k.ui_font.clone())
                        .text_color(k.text_faint)
                        .cursor_pointer()
                        .hover(move |style| style.text_color(soft))
                        .child(if full { "Show less" } else { "Show more" })
                        .on_click(toggle(&row.pane, Disclosure::Output, key.to_string())),
                )
            })
        })
        .into_any_element()
}

fn file_chips(row: &RowSnapshot, files: &[usize], k: &Tokens) -> AnyElement {
    let paths: Vec<PathBuf> = files
        .iter()
        .filter_map(|&index| match row.turn.blocks.get(index) {
            Some(Block::FileChanged { path }) => Some(path.clone()),
            _ => None,
        })
        .collect();
    let active = k.surface_active;
    div()
        .h_flex()
        .flex_wrap()
        .gap(px(4.))
        .children(paths.into_iter().enumerate().map(|(position, path)| {
            let label = file_label(&path);
            div()
                .id(id(&["file", &row.turn.id, &files[position].to_string()]))
                .h_flex()
                .h(px(20.))
                .px(px(7.))
                .gap(px(4.))
                .rounded(k.radius_s)
                .bg(k.surface_hover)
                .text_xs()
                .text_color(k.text_soft)
                .cursor_pointer()
                .hover(move |style| style.bg(active))
                .child(
                    Icon::new(IconName::FileText)
                        .size(px(11.))
                        .text_color(k.text_faint),
                )
                .child(label)
                .on_click(on_pane(&row.pane, move |pane, cx| {
                    pane.open_file(path.clone(), cx)
                }))
        }))
        .into_any_element()
}

fn footer(row: &RowSnapshot, k: &Tokens) -> Option<AnyElement> {
    let turn = &row.turn;
    let files = files_summary(&turn.files_changed);
    let cost = turn.usage.cost_usd.and_then(format_cost);
    let can_undo = !turn.files_changed.is_empty() && !turn.undone;
    let turn_id = turn.id.clone();

    let undo = can_undo.then(|| {
        let turn_id = turn_id.clone();
        Button::new(id(&["undo", &turn.id]))
            .ghost()
            .xsmall()
            .icon(IconName::Undo2)
            .label("Undo")
            .text_color(k.text_soft)
            .disabled(row.busy)
            .on_click(on_pane(&row.pane, move |pane, cx| {
                pane.undo_turn(&turn_id, cx)
            }))
    });
    let retry = || {
        let turn_id = turn_id.clone();
        Button::new(id(&["retry", &turn.id]))
            .ghost()
            .xsmall()
            .icon(IconName::RefreshCw)
            .label("Retry")
            .text_color(k.text_soft)
            .on_click(on_pane(&row.pane, move |pane, cx| {
                pane.retry_turn(&turn_id, cx)
            }))
    };
    let nothing_to_say = files.is_none() && cost.is_none();
    let meta = move |label: Option<&'static str>, retry: Option<Button>| {
        div()
            .h_flex()
            .flex_wrap()
            .gap(px(8.))
            .text_xs()
            .text_color(k.text_faint)
            .children(label)
            .children(files)
            .children(cost)
            .children(turn.undone.then_some("Undone"))
            .children(retry)
            .children(undo)
    };

    let element = match &turn.status {
        TurnStatus::Running => return None,
        TurnStatus::Succeeded => {
            if nothing_to_say {
                return None;
            }
            meta(None, None).into_any_element()
        }
        TurnStatus::Stopped => meta(Some("Stopped"), None).into_any_element(),
        TurnStatus::Interrupted => {
            meta(Some("Interrupted before it finished"), Some(retry())).into_any_element()
        }
        TurnStatus::Failed { message } => div()
            .v_flex()
            .w_full()
            .gap(px(4.))
            .child(
                div()
                    .h_flex()
                    .items_start()
                    .gap(px(6.))
                    .child(
                        div().pt(px(2.)).child(
                            Icon::new(IconName::CircleAlert)
                                .size(px(13.))
                                .text_color(k.danger),
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(k.danger)
                            .child(message.clone()),
                    ),
            )
            .child(meta(None, Some(retry())))
            .into_any_element(),
    };
    Some(element)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disclosures_toggle() {
        let mut ui = ViewState::default();
        ui.toggle(Disclosure::Tool, "t/a");
        ui.toggle(Disclosure::Group, "t/g1");
        assert!(ui.open_tools.contains("t/a"));
        assert!(ui.open_groups.contains("t/g1"));
        ui.toggle(Disclosure::Tool, "t/a");
        assert!(!ui.open_tools.contains("t/a"));
        ui.toggle(Disclosure::Thinking, "t/0");
        ui.toggle(Disclosure::Output, "t/a");
        assert!(ui.open_thinking.contains("t/0"));
        assert!(ui.full_output.contains("t/a"));
    }
}
