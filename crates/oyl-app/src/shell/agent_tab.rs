//! The Agent tab: the agent (and its threads) on the left, the paper on
//! the right. ⌘L goes there from any other tab and back again; Esc in the
//! composer goes back too.

use gpui_kit::{Context, Window};
use oyl_core::LayoutMode;

use super::Shell;

impl Shell {
    /// Shows the Agent tab and puts the caret in the composer, remembering
    /// the tab to come back to.
    pub(crate) fn enter_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.paper.is_none() {
            return;
        }
        if self.mode != LayoutMode::Agent {
            self.set_mode(LayoutMode::Agent, window, cx);
        }
        self.chat
            .update(cx, |chat, cx| chat.focus_composer(window, cx));
    }

    /// Back from the Agent tab to the tab the user came from.
    pub(crate) fn leave_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode != LayoutMode::Agent {
            self.focus_content(window, cx);
            return;
        }
        let back = match self.previous_mode {
            LayoutMode::Agent => LayoutMode::Split,
            other => other,
        };
        self.set_mode(back, window, cx);
        self.focus_content(window, cx);
    }

    /// ⌘L.
    pub(crate) fn toggle_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == LayoutMode::Agent {
            self.leave_agent(window, cx);
        } else {
            self.enter_agent(window, cx);
        }
    }

    /// Whether the Agent tab is on screen.
    pub(crate) fn agent_shown(&self) -> bool {
        self.mode == LayoutMode::Agent
    }

    /// Esc came from the composer (or the chat pane): back to the tab the
    /// user came from.
    pub(crate) fn leave_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_agent(window, cx);
    }
}
