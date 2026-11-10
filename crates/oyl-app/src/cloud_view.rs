//! The Oyl Cloud sign-in card: the code to check in the browser, "Open
//! browser", and how the sign-in ended. The card only shows and asks; the
//! shell does the work ([`CloudCardEvent`]).

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, EventEmitter, FocusHandle, Focusable, Render, SharedString, Window, div, px,
};
use oyl_ui::prelude::*;

/// Where a sign-in is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignInStage {
    /// Asking the cloud for a code.
    Starting,
    /// The code is shown; waiting for the browser.
    Waiting {
        /// `BCDF-GHJK`.
        code: String,
        /// The page that approves it.
        url: String,
    },
    /// Declined in the browser.
    Declined,
    /// The code expired first.
    Expired,
    /// Something failed; in words.
    Failed(String),
}

/// What the card asks the shell to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloudCardEvent {
    /// Open the approval page in the browser.
    OpenBrowser,
    /// Stop waiting and close the card.
    Cancel,
    /// Start over with a new code.
    Retry,
}

/// The sign-in card.
pub struct CloudSignInView {
    stage: SignInStage,
    focus_handle: FocusHandle,
}

impl EventEmitter<CloudCardEvent> for CloudSignInView {}

impl Focusable for CloudSignInView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl CloudSignInView {
    /// A card at the start of a sign-in.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            stage: SignInStage::Starting,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Shows another stage.
    pub fn set_stage(&mut self, stage: SignInStage, cx: &mut Context<Self>) {
        if self.stage != stage {
            self.stage = stage;
            cx.notify();
        }
    }

    /// The stage on screen.
    pub fn stage(&self) -> &SignInStage {
        &self.stage
    }

    fn actions(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        let waiting = matches!(self.stage, SignInStage::Waiting { .. });
        let ended = matches!(
            self.stage,
            SignInStage::Declined | SignInStage::Expired | SignInStage::Failed(_)
        );
        div()
            .h_flex()
            .gap(px(8.))
            .when(waiting, |this| {
                this.child(
                    Button::new("cloud-open-browser")
                        .primary()
                        .small()
                        .label("Open browser")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(CloudCardEvent::OpenBrowser))),
                )
            })
            .when(ended, |this| {
                this.child(
                    Button::new("cloud-retry")
                        .primary()
                        .small()
                        .label("Try again")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(CloudCardEvent::Retry))),
                )
            })
            .child(
                Button::new("cloud-cancel")
                    .ghost()
                    .small()
                    .label(if ended { "Close" } else { "Cancel" })
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloudCardEvent::Cancel))),
            )
            .child(div().flex_1())
            .when(waiting, |this| {
                this.child(
                    Button::new("cloud-waiting")
                        .ghost()
                        .xsmall()
                        .loading(true)
                        .disabled(true)
                        .label("Waiting for approval…"),
                )
            })
    }
}

impl Render for CloudSignInView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let k = cx.tokens().clone();
        let body: gpui_kit::AnyElement = match &self.stage {
            SignInStage::Starting => div()
                .text_sm()
                .text_color(k.text_faint)
                .child("Asking Oyl Cloud for a code…")
                .into_any_element(),
            SignInStage::Waiting { code, .. } => {
                let code: SharedString = code.clone().into();
                div()
                    .v_flex()
                    .gap(px(10.))
                    .child(
                        div()
                            .id("cloud-code")
                            .py(px(12.))
                            .flex()
                            .justify_center()
                            .rounded(k.radius_m)
                            .bg(k.surface)
                            .border_1()
                            .border_color(k.hairline)
                            .font_family(k.mono_font.clone())
                            .text_2xl()
                            .text_color(k.text)
                            .child(code),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(k.text_soft)
                            .child("Open the page in your browser, check that it shows this code, and approve this Mac."),
                    )
                    .into_any_element()
            }
            SignInStage::Declined => {
                message("The sign-in was declined in the browser.", k.text_soft)
            }
            SignInStage::Expired => message(
                "The code expired before it was approved. Start again for a new one.",
                k.text_soft,
            ),
            SignInStage::Failed(why) => message(why, k.danger),
        };
        div()
            .id("oyl-cloud-sign-in")
            .track_focus(&self.focus_handle)
            .v_flex()
            .gap(px(14.))
            .p(px(22.))
            .text_color(k.text)
            .child(oyl_ui::eyebrow("Oyl Cloud", cx))
            .child(
                div()
                    .font_family(k.serif_font.clone())
                    .text_xl()
                    .child("Sign in to Oyl Cloud"),
            )
            .child(div().text_sm().text_color(k.text_soft).child(
                "Your papers stay on this Mac. The ones you choose also sync to your account.",
            ))
            .child(body)
            .child(self.actions(cx))
    }
}

fn message(text: &str, color: gpui_kit::Hsla) -> gpui_kit::AnyElement {
    div()
        .text_sm()
        .text_color(color)
        .child(text.to_string())
        .into_any_element()
}
