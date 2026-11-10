//! The overlay host: one floating card at a time over the window.
//!
//! Everything that is not the source, the paper or the agent opens here:
//! the command palette (top), the library (center) and the panels (right
//! side). A card is dismissed by Esc and by a click outside it, and focus
//! goes back to where it was before the card opened. Showing a card while
//! another is up replaces it (and focus still returns to where it was before
//! the first one).
//!
//! To show a panel:
//!
//! ```ignore
//! shell.overlays().update(cx, |host, cx| {
//!     host.show_panel("sources", view.into(), Some(focus), px(420.), window, cx)
//! });
//! ```

use gpui_kit::prelude::*;
use gpui_kit::{
    Action, AnyView, Context, EventEmitter, FocusHandle, MouseButton, Pixels, Render, SharedString,
    Window, div, px,
};
use oyl_ui::prelude::*;

use crate::actions::{DismissOverlay, OVERLAY_CONTEXT};
use crate::layout;

/// Where a card floats.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Placement {
    /// Along the right edge, nearly the window's height (panels).
    Right {
        /// The card's width.
        width: Pixels,
    },
    /// In the middle of the window (the library).
    Center {
        /// The card's width.
        width: Pixels,
    },
    /// Centered near the top (the command palette).
    Top {
        /// The card's width.
        width: Pixels,
    },
}

/// What the host tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverlayEvent {
    /// The card with this id went away (Esc, a click outside, or
    /// [`OverlayHost::dismiss`]). Focus has been given back already.
    Dismissed(SharedString),
}

struct Shown {
    id: SharedString,
    view: AnyView,
    placement: Placement,
    /// What Esc in a text field inside the card asks of the view (a panel
    /// first closes what is open inside it); `None` takes the card down.
    escape: Option<Box<dyn Action>>,
}

/// Hosts the one floating card of the window.
pub struct OverlayHost {
    shown: Option<Shown>,
    /// Focus before the first card of a run of cards was shown.
    restore: Option<FocusHandle>,
    /// Tracked by the card itself, so Esc reaches the host even when the
    /// view inside takes no focus.
    focus_handle: FocusHandle,
}

impl EventEmitter<OverlayEvent> for OverlayHost {}

impl OverlayHost {
    /// An empty host.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            shown: None,
            restore: None,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Shows `view` in a card. `focus` is focused (the card itself when
    /// `None`). A card already up is replaced.
    pub fn show(
        &mut self,
        id: impl Into<SharedString>,
        view: AnyView,
        focus: Option<FocusHandle>,
        placement: Placement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.shown.is_none() {
            self.restore = window.focused(cx);
        }
        let target = focus.clone().unwrap_or_else(|| self.focus_handle.clone());
        self.shown = Some(Shown {
            id: id.into(),
            view,
            placement,
            escape: None,
        });
        target.focus(window, cx);
        cx.notify();
    }

    /// Makes Esc in a text field inside the card dispatch `action` to the
    /// view instead of taking the card down (the panels' `Dismiss`, which
    /// clears a filter first and asks to close only after).
    pub fn set_escape_action(&mut self, action: Box<dyn Action>) {
        if let Some(shown) = self.shown.as_mut() {
            shown.escape = Some(action);
        }
    }

    /// Changes the width of the card with this id (a panel that widens).
    pub fn set_width(&mut self, id: &str, width: Pixels, cx: &mut Context<Self>) {
        let Some(shown) = self.shown.as_mut().filter(|shown| shown.id.as_ref() == id) else {
            return;
        };
        let placement = match shown.placement {
            Placement::Right { .. } => Placement::Right { width },
            Placement::Center { .. } => Placement::Center { width },
            Placement::Top { .. } => Placement::Top { width },
        };
        if placement != shown.placement {
            shown.placement = placement;
            cx.notify();
        }
    }

    /// Shows `view` as a panel along the right edge, `width` wide and nearly
    /// as tall as the window. This is how the overlay panels plug in.
    pub fn show_panel(
        &mut self,
        id: impl Into<SharedString>,
        view: AnyView,
        focus: Option<FocusHandle>,
        width: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show(id, view, focus, Placement::Right { width }, window, cx);
    }

    /// Takes the card down and gives focus back to where it was.
    pub fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shown) = self.shown.take() else {
            return;
        };
        if let Some(restore) = self.restore.take() {
            restore.focus(window, cx);
        }
        cx.emit(OverlayEvent::Dismissed(shown.id));
        cx.notify();
    }

    /// Takes the card down if it is the one with this id.
    pub fn dismiss_if(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_showing(id) {
            self.dismiss(window, cx);
        }
    }

    /// The id of the card up, if any.
    pub fn current(&self) -> Option<&SharedString> {
        self.shown.as_ref().map(|shown| &shown.id)
    }

    /// Whether the card with this id is up.
    pub fn is_showing(&self, id: &str) -> bool {
        self.current().is_some_and(|current| current.as_ref() == id)
    }

    fn on_dismiss(&mut self, _: &DismissOverlay, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(window, cx);
    }

    /// Esc in a text field inside the card that the field did not need.
    fn on_input_escape(
        &mut self,
        _: &gpui_kit::component::input::Escape,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.shown.as_ref().and_then(|shown| shown.escape.as_ref()) {
            Some(action) => window.dispatch_action(action.boxed_clone(), cx),
            None => self.dismiss(window, cx),
        }
    }

    /// Esc in the command palette with an empty query.
    fn on_cancel(
        &mut self,
        _: &gpui_kit::base::actions::Cancel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss(window, cx);
    }
}

impl Render for OverlayHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(shown) = &self.shown else {
            return div().id("oyl-overlays");
        };
        let viewport = window.viewport_size();
        let card = oyl_ui::raised(cx)
            .id("oyl-overlay-card")
            .key_context(OVERLAY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_dismiss))
            .on_action(cx.listener(Self::on_input_escape))
            .on_action(cx.listener(Self::on_cancel))
            .occlude()
            .overflow_hidden()
            .flex()
            .flex_col()
            .text_color(cx.tokens().text)
            .child(shown.view.clone());

        let card = match shown.placement {
            Placement::Right { width } => div()
                .absolute()
                .top(px(layout::PANEL_TOP))
                .right(px(layout::PANEL_MARGIN))
                .bottom(px(layout::PANEL_MARGIN))
                .w(width)
                .flex()
                .child(card.size_full()),
            Placement::Center { width } => div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    card.w(width)
                        .max_w(viewport.width - px(2. * layout::PANEL_MARGIN))
                        .max_h(viewport.height - px(2. * layout::LIBRARY_MARGIN)),
                ),
            Placement::Top { width } => div()
                .absolute()
                .top(px(layout::PALETTE_TOP))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    card.w(width)
                        .max_w(viewport.width - px(2. * layout::PANEL_MARGIN)),
                ),
        };

        div()
            .id("oyl-overlays")
            .absolute()
            .inset_0()
            .child(
                div()
                    .id("oyl-overlay-backdrop")
                    .absolute()
                    .inset_0()
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| this.dismiss(window, cx)),
                    ),
            )
            .child(card)
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{
        App, Bounds, Entity, FocusHandle, Focusable, Point, TestAppContext, WindowBounds,
        WindowOptions, size,
    };

    use super::*;

    /// A window with a focusable "editor" and the host on top.
    struct Window_ {
        editor: FocusHandle,
        panel: Entity<Panel>,
        host: Entity<OverlayHost>,
    }

    impl Render for Window_ {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .relative()
                .child(div().id("editor").size_full().track_focus(&self.editor))
                .child(self.host.clone())
        }
    }

    struct Panel {
        focus: FocusHandle,
    }

    impl Focusable for Panel {
        fn focus_handle(&self, _: &App) -> FocusHandle {
            self.focus.clone()
        }
    }

    impl Render for Panel {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("panel")
                .track_focus(&self.focus)
                .size_full()
                .child("Panel")
        }
    }

    fn open(cx: &mut TestAppContext) -> (gpui_kit::AnyWindowHandle, Entity<Window_>) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            oyl_ui::install(oyl_ui::Appearance::Light, cx);
            crate::actions::bind_keys(cx);
        });
        cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Point::default(),
                        size: size(px(1000.), px(700.)),
                    })),
                    ..Default::default()
                },
                cx,
                |_, cx| {
                    cx.new(|cx| Window_ {
                        editor: cx.focus_handle(),
                        panel: cx.new(|cx| Panel {
                            focus: cx.focus_handle(),
                        }),
                        host: cx.new(OverlayHost::new),
                    })
                },
            )
            .unwrap()
        })
    }

    fn draw(window: gpui_kit::AnyWindowHandle, cx: &mut TestAppContext) {
        cx.update_window(window, |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        })
        .unwrap();
        cx.run_until_parked();
    }

    #[gpui::test]
    fn a_panel_takes_focus_and_gives_it_back(cx: &mut TestAppContext) {
        let (window, root) = open(cx);
        let (editor, panel, host) = root.read_with(cx, |r, _| {
            (r.editor.clone(), r.panel.clone(), r.host.clone())
        });
        cx.update_window(window, |_, window, cx| editor.focus(window, cx))
            .unwrap();
        draw(window, cx);

        cx.update_window(window, |_, window, cx| {
            host.update(cx, |host, cx| {
                let focus = panel.read(cx).focus.clone();
                host.show_panel(
                    "sources",
                    panel.clone().into(),
                    Some(focus),
                    px(420.),
                    window,
                    cx,
                )
            })
        })
        .unwrap();
        draw(window, cx);
        let panel_focus = panel.read_with(cx, |p, _| p.focus.clone());
        let focused = cx
            .update_window(window, |_, window, _| panel_focus.is_focused(window))
            .unwrap();
        assert!(focused, "the panel is focused");
        assert!(host.read_with(cx, |h, _| h.is_showing("sources")));

        // Esc goes to the host's key context.
        cx.update_window(window, |_, window, cx| {
            window.dispatch_action(Box::new(DismissOverlay), cx)
        })
        .unwrap();
        draw(window, cx);
        assert!(!host.read_with(cx, |h, _| h.current().is_some()));
        let back = cx
            .update_window(window, |_, window, _| editor.is_focused(window))
            .unwrap();
        assert!(back, "focus went back to the editor");
    }

    #[gpui::test]
    fn replacing_a_card_keeps_the_first_focus_to_return_to(cx: &mut TestAppContext) {
        let (window, root) = open(cx);
        let (editor, panel, host) = root.read_with(cx, |r, _| {
            (r.editor.clone(), r.panel.clone(), r.host.clone())
        });
        cx.update_window(window, |_, window, cx| {
            editor.focus(window, cx);
            host.update(cx, |host, cx| {
                host.show(
                    "palette",
                    panel.clone().into(),
                    None,
                    Placement::Top { width: px(600.) },
                    window,
                    cx,
                );
                let focus = panel.read(cx).focus.clone();
                host.show_panel(
                    "library",
                    panel.clone().into(),
                    Some(focus),
                    px(500.),
                    window,
                    cx,
                );
            });
        })
        .unwrap();
        draw(window, cx);
        assert!(host.read_with(cx, |h, _| h.is_showing("library")));
        cx.update_window(window, |_, window, cx| {
            host.update(cx, |host, cx| host.dismiss_if("palette", window, cx))
        })
        .unwrap();
        assert!(
            host.read_with(cx, |h, _| h.is_showing("library")),
            "dismissing another id leaves the card up"
        );
        cx.update_window(window, |_, window, cx| {
            host.update(cx, |host, cx| host.dismiss(window, cx))
        })
        .unwrap();
        draw(window, cx);
        let back = cx
            .update_window(window, |_, window, _| editor.is_focused(window))
            .unwrap();
        assert!(back);
    }

    #[gpui::test]
    fn a_click_outside_dismisses(cx: &mut TestAppContext) {
        let (window, root) = open(cx);
        let (panel, host) = root.read_with(cx, |r, _| (r.panel.clone(), r.host.clone()));
        cx.update_window(window, |_, window, cx| {
            host.update(cx, |host, cx| {
                let focus = panel.read(cx).focus.clone();
                host.show_panel(
                    "sources",
                    panel.clone().into(),
                    Some(focus),
                    px(300.),
                    window,
                    cx,
                )
            })
        })
        .unwrap();
        draw(window, cx);
        let mut visual = gpui_kit::VisualTestContext::from_window(window, cx);
        // Far left: outside the right-hand panel.
        visual.simulate_click(
            gpui_kit::point(px(40.), px(300.)),
            gpui_kit::Modifiers::none(),
        );
        visual.run_until_parked();
        assert!(!host.read_with(&visual, |h, _| h.current().is_some()));
    }
}
