//! Drawing the shell: the header row, the three modes, the source card, the
//! agent's card sliding in from the right, the split handle.

use std::time::Instant;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{IconName, Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, DispatchPhase, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Render, SharedString, Window, canvas, div, px, relative,
};
use oyl_core::LayoutMode;
use oyl_ui::prelude::*;

use super::{Drag, Shell};
use crate::actions::*;
use crate::layout::{self, MODES};
use crate::status::Tone;

impl Shell {
    /// The pointer moved anywhere in the window: dividers being dragged,
    /// the split handle appearing over the paper.
    pub(crate) fn pointer_moved(
        &mut self,
        position: Point<Pixels>,
        pressed: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(drag) = self.drag {
            if !pressed {
                self.end_drag(cx);
                return;
            }
            let Drag::Split = drag;
            let bounds = self.content_bounds.get();
            let ratio = layout::split_from_drag(
                self.mode,
                f32::from(position.x),
                f32::from(bounds.origin.x),
                f32::from(bounds.size.width),
            );
            self.set_split_ratio(ratio, cx);
            return;
        }
        let over = self.handle.preview_bounds.get().contains(&position)
            || self.handle.hit_bounds.get().contains(&position);
        self.show_handle(over, cx);
    }

    /// Fades the split handle in or out.
    pub(crate) fn show_handle(&mut self, show: bool, cx: &mut Context<Self>) {
        let show = show || self.drag == Some(Drag::Split);
        if show != self.handle.fade.is_shown() {
            self.handle.fade.set(show, Instant::now());
            cx.notify();
        }
    }

    /// Ends a divider drag.
    pub(crate) fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            cx.notify();
        }
    }

    /// The header row: part of the window, in the row of the traffic
    /// lights. Its empty area drags the window.
    fn render_header(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let drag_area = div().id("header-drag").absolute().inset_0().on_mouse_down(
            MouseButton::Left,
            |event: &MouseDownEvent, window, _| {
                if event.click_count >= 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            },
        );
        let contents = self.render_header_contents(window, cx);
        div()
            .id("oyl-header")
            .relative()
            .flex_none()
            .w_full()
            .h(px(layout::HEADER_HEIGHT))
            // The one bit of glass: the desktop shows through a little at the
            // top, fading into the solid body by the header's bottom edge.
            .bg(gpui_kit::linear_gradient(
                180.,
                gpui_kit::linear_color_stop(cx.tokens().veil, 0.),
                gpui_kit::linear_color_stop(cx.tokens().body, 1.),
            ))
            .child(drag_area)
            .child(contents)
            .into_any_element()
    }

    fn render_header_contents(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // "Compiling…" appears once a compile outlasts its grace period.
        if self.compile.running_since.is_some() {
            window.request_animation_frame();
        }
        let k = cx.tokens().clone();
        let hover = k.surface_hover;

        let left = self.paper.as_ref().map(|paper| {
            let name: SharedString = paper.project.name().to_string().into();
            div()
                .id("header-left")
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(layout::HEADER_LEFT))
                .h_flex()
                .gap(px(6.))
                .occlude()
                .child(
                    div()
                        .id("header-paper")
                        .h(px(26.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .rounded(k.radius_s)
                        .max_w(px(layout::HEADER_NAME_MAX))
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(k.text)
                        .child(div().truncate().child(name))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.show_library(false, window, cx)),
                        ),
                )
                .child(oyl_ui::chip(paper.engine().label(), cx))
        });

        let running = self.agent.running;
        let center = self.paper.is_some().then(|| {
            let modes = div()
                .id("header-modes")
                .h_flex()
                .gap(px(1.))
                .p(px(2.))
                .rounded_full()
                .bg(k.surface)
                .occlude()
                .children(MODES.into_iter().map(|mode| {
                    let active = self.mode == mode;
                    let (accent, accent_soft, soft) = (k.accent, k.accent_soft, k.text_soft);
                    // A turn running anywhere shows on the Agent tab.
                    let dot = (mode == LayoutMode::Agent && running)
                        .then(|| oyl_tasks::PulseDot::new("agent-pulse").into_any_element());
                    div()
                        .id(SharedString::from(format!(
                            "mode-{}",
                            layout::mode_label(mode)
                        )))
                        .h(px(24.))
                        .min_w(px(layout::MODE_TAB_MIN_WIDTH))
                        .px(px(12.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(6.))
                        .rounded_full()
                        .text_xs()
                        .cursor_pointer()
                        .when(active, |this| this.bg(accent_soft).text_color(accent))
                        .when(!active, |this| {
                            this.text_color(soft).hover(move |style| style.bg(hover))
                        })
                        .child(layout::mode_label(mode))
                        .children(dot)
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.set_mode(mode, window, cx)),
                        )
                }));
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .right_0()
                .flex()
                .items_center()
                .justify_center()
                .child(modes)
        });

        let status = self.paper.as_ref().map(|_| {
            let status = self.status();
            let color = match status.tone {
                Tone::Plain => k.text_soft,
                Tone::Danger => k.danger,
                Tone::Faint => k.text_faint,
            };
            let has_problems = self.compile.errors > 0;
            let short = oyl_chat::thread::ellipsize(&status.text, layout::STATUS_MAX_CHARS);
            let full: SharedString = status.text.clone().into();
            let cut = short != status.text;
            div()
                .id("header-status")
                .h(px(24.))
                .px(px(8.))
                .flex()
                .items_center()
                .rounded_full()
                .text_xs()
                .text_color(color)
                .when(cut, |this| {
                    this.tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                })
                .when(has_problems, |this| {
                    this.cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.show_panel(super::PanelKind::Problems, window, cx)
                        }))
                })
                .child(short)
        });
        let preparing = self.agent.preparing.as_ref().map(|(_, line)| {
            let full: SharedString = line.clone().into();
            div()
                .id("header-setup")
                .px(px(4.))
                .text_xs()
                .text_color(k.text_faint)
                .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                .child("Setting up agent…")
        });
        let cloud = self.cloud_glyph(cx).map(|glyph| {
            let tooltip: SharedString = glyph.tooltip.into();
            div()
                .id("header-cloud")
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .on_click(cx.listener(|this, _, window, cx| this.cloud_glyph_clicked(window, cx)))
                .child(
                    gpui_kit::component::Icon::new(glyph.icon)
                        .xsmall()
                        .text_color(glyph.color),
                )
        });
        let right = div()
            .id("header-right")
            .absolute()
            .top_0()
            .bottom_0()
            .right(px(layout::HEADER_RIGHT))
            .h_flex()
            .gap(px(4.))
            .occlude()
            .children(preparing)
            .children(cloud)
            .children(status)
            .child(
                Button::new("header-theme")
                    .ghost()
                    .xsmall()
                    .icon(if k.dark {
                        IconName::Sun
                    } else {
                        IconName::Moon
                    })
                    .tooltip(if k.dark { "Light" } else { "Dark" })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.on_toggle_appearance(&ToggleAppearance, window, cx)
                    })),
            );

        div()
            .absolute()
            .inset_0()
            .children(center)
            .children(left)
            .child(right)
            .into_any_element()
    }

    /// The split handle: a slim capsule centered on the boundary between
    /// source and paper, there only while the pointer is over the paper or
    /// the handle (and while dragging).
    fn render_handle(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let now = Instant::now();
        if self.handle.fade.is_animating(now) {
            window.request_animation_frame();
        }
        let opacity = self.handle.fade.opacity(now);
        let k = cx.tokens();
        let color = if self.drag == Some(Drag::Split) {
            k.text_soft
        } else {
            k.text_faint
        };
        let hit = self.handle.hit_bounds.clone();
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(-layout::HANDLE_HIT_WIDTH / 2.))
            .w(px(layout::HANDLE_HIT_WIDTH))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .id("split-handle")
                    .relative()
                    .w(px(layout::HANDLE_HIT_WIDTH))
                    .h(px(layout::HANDLE_HIT_HEIGHT))
                    .flex()
                    .items_center()
                    .justify_center()
                    .occlude()
                    .cursor_col_resize()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            if event.click_count >= 2 {
                                this.drag = None;
                                this.set_split_ratio(layout::SPLIT_DEFAULT, cx);
                            } else {
                                this.drag = Some(Drag::Split);
                                this.show_handle(true, cx);
                            }
                            cx.notify();
                        }),
                    )
                    .child(
                        canvas(move |bounds, _, _| hit.set(bounds), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    )
                    .child(
                        div()
                            .w(px(layout::HANDLE_WIDTH))
                            .h(px(layout::HANDLE_HEIGHT))
                            .rounded_full()
                            .bg(color)
                            .opacity(opacity),
                    ),
            )
            .into_any_element()
    }

    /// A card on the left of the window (the source's, or the agent's in
    /// the Agent tab); its bounds are kept for the self-test.
    fn left_card(
        &self,
        id: &'static str,
        content: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let bounds = self.card_bounds.clone();
        oyl_ui::card(cx)
            .id(id)
            .relative()
            .size_full()
            .overflow_hidden()
            .child(
                canvas(move |b, _, _| bounds.set(b), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(content)
            .into_any_element()
    }

    /// The agent's card: the thread (at a readable width) over the
    /// composer.
    fn agent_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let chat = div().size_full().flex().justify_center().child(
            div()
                .h_full()
                .w_full()
                .max_w(px(layout::AGENT_READING_WIDTH))
                .child(self.chat.clone()),
        );
        self.left_card("oyl-agent", chat.into_any_element(), cx)
    }

    /// The preview, filling exactly the box it is given; above it, a quiet
    /// chip when it shows a thread's branch instead of the paper.
    fn render_preview(&self, cx: &App) -> AnyElement {
        let preview_bounds = self.handle.preview_bounds.clone();
        let k = cx.tokens();
        let chip = self.preview_branch(cx).map(|(_, title)| {
            div()
                .absolute()
                .top(px(10.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    oyl_ui::raised(cx)
                        .id("oyl-branch-chip")
                        .rounded(k.radius_m)
                        .px(px(10.))
                        .py(px(4.))
                        .text_xs()
                        .text_color(k.text_soft)
                        .max_w(px(360.))
                        .truncate()
                        .child(branch_chip_label(&title)),
                )
        });
        div()
            .relative()
            .size_full()
            .child(
                canvas(
                    move |bounds, _, _| preview_bounds.set(bounds),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(self.preview.clone())
            .children(chip)
            .into_any_element()
    }

    fn render_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let content_bounds = self.content_bounds.clone();
        let measure = canvas(
            move |bounds, _, _| content_bounds.set(bounds),
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let gap = px(layout::CARD_GAP);
        let ratio = layout::clamp_columns(
            self.mode,
            self.settings.split_ratio,
            f32::from(self.content_bounds.get().size.width),
        );
        let body = {
            {
                let left = if self.mode == LayoutMode::Agent {
                    // [sidebar] [agent card], one key context for ⌘N.
                    div()
                        .key_context(AGENT_CONTEXT)
                        .size_full()
                        .flex()
                        .flex_row()
                        .gap(gap)
                        .child(
                            div()
                                .flex_none()
                                .w(px(layout::SIDEBAR_WIDTH))
                                .h_full()
                                .child(self.sidebar.clone()),
                        )
                        .child(div().flex_1().min_w_0().h_full().child(self.agent_card(cx)))
                        .into_any_element()
                } else {
                    self.left_card("oyl-source", self.editor.clone().into_any_element(), cx)
                };
                let handle = self.render_handle(window, cx);
                div()
                    .size_full()
                    .flex()
                    .flex_row()
                    .child(
                        div()
                            .h_full()
                            .flex_none()
                            .w(relative(ratio))
                            .p(gap)
                            .child(left),
                    )
                    .child(
                        div()
                            .relative()
                            .h_full()
                            .flex_1()
                            .min_w_0()
                            .child(self.render_preview(cx))
                            .child(handle),
                    )
                    .into_any_element()
            }
        };
        div()
            .relative()
            .size_full()
            .child(measure)
            .child(body)
            .into_any_element()
    }

    /// Nothing open: the library is the window's content.
    fn render_welcome(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let viewport = window.viewport_size();
        let height = viewport.height - px(layout::HEADER_HEIGHT + 2. * layout::LIBRARY_MARGIN);
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                oyl_ui::raised(cx)
                    .w(px(layout::LIBRARY_WIDTH))
                    .max_w(viewport.width - px(2. * layout::PANEL_MARGIN))
                    .max_h(height)
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(self.library_view.clone()),
            )
            .into_any_element()
    }

    /// A paper is being opened and none is on screen yet.
    fn render_opening(&self, cx: &mut Context<Self>) -> AnyElement {
        let k = cx.tokens();
        let name = self
            .opening
            .as_ref()
            .and_then(|(path, _)| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .font_family(k.serif_font.clone())
            .text_xl()
            .text_color(k.text_faint)
            .child(format!("Opening {name}…"))
            .into_any_element()
    }

    /// Follows the pointer everywhere in the window (dividers, the header,
    /// the split handle), whatever element is under it.
    fn pointer_tracker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let moves = cx.weak_entity();
        let ups = cx.weak_entity();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let moves = moves.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase != DispatchPhase::Capture {
                        return;
                    }
                    let pressed = event.pressed_button == Some(MouseButton::Left);
                    let _ = moves.update(cx, |this, cx| {
                        this.pointer_moved(event.position, pressed, cx)
                    });
                });
                let ups = ups.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                        let _ = ups.update(cx, |this, cx| this.end_drag(cx));
                    }
                });
            },
        )
        .absolute()
        .size_full()
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let k = cx.tokens().clone();
        let content = if self.paper.is_some() {
            self.render_mode(window, cx)
        } else if self.opening.is_some() {
            self.render_opening(cx)
        } else {
            self.render_welcome(window, cx)
        };
        let header = self.render_header(window, cx);
        let dragging = self.drag;
        div()
            .id("oyl-shell")
            .key_context(SHELL_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_quit))
            .on_action(cx.listener(Self::on_command_palette))
            .on_action(cx.listener(Self::on_open_library))
            .on_action(cx.listener(Self::on_new_paper))
            .on_action(cx.listener(Self::on_open_folder))
            .on_action(cx.listener(Self::on_write_mode))
            .on_action(cx.listener(Self::on_agent_mode))
            .on_action(cx.listener(Self::on_toggle_appearance))
            .on_action(cx.listener(Self::on_light))
            .on_action(cx.listener(Self::on_dark))
            .on_action(cx.listener(Self::on_system))
            .on_action(cx.listener(Self::on_focus_agent))
            .on_action(cx.listener(Self::on_new_chat))
            .on_action(cx.listener(Self::on_stop_agent))
            .on_action(cx.listener(Self::on_reveal_in_preview))
            .on_action(cx.listener(Self::on_export_pdf))
            .on_action(cx.listener(Self::on_export_source))
            .on_action(cx.listener(Self::on_export_bibliography))
            .on_action(cx.listener(Self::on_reveal_in_finder))
            .on_action(cx.listener(Self::on_zoom_in))
            .on_action(cx.listener(Self::on_zoom_out))
            .on_action(cx.listener(Self::on_zoom_to_fit))
            .on_action(cx.listener(Self::on_show_sources))
            .on_action(cx.listener(Self::on_show_history))
            .on_action(cx.listener(Self::on_show_outline))
            .on_action(cx.listener(Self::on_show_figures))
            .on_action(cx.listener(Self::on_show_problems))
            .on_action(cx.listener(Self::on_polish_selection))
            .on_action(cx.listener(Self::on_fact_check_selection))
            .on_action(cx.listener(Self::on_ask_ai_about_selection))
            .on_action(cx.listener(Self::on_close_window))
            .on_action(cx.listener(Self::on_minimize))
            .on_action(cx.listener(Self::on_zoom_window))
            .on_action(cx.listener(Self::on_escape))
            .on_action(cx.listener(Self::on_leave_chat))
            .on_action(cx.listener(Self::on_sign_in_to_cloud))
            .on_action(cx.listener(Self::on_sign_out_of_cloud))
            .on_action(cx.listener(Self::on_toggle_cloud_sync))
            .on_action(cx.listener(Self::on_sync_now))
            .on_action(cx.listener(Self::on_open_from_cloud))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .font_family(k.ui_font.clone())
            .text_color(k.text)
            .when(dragging == Some(Drag::Split), |this| {
                this.cursor_col_resize()
            })
            .child(self.pointer_tracker(cx))
            .child(header)
            // Solid below the header: the same look over any desktop.
            .child(div().relative().flex_1().min_h_0().w_full().bg(k.body).child(content))
            .child(self.overlays.clone())
    }
}

/// "Showing “Tighten the abstract”'s branch".
pub(crate) fn branch_chip_label(title: &str) -> String {
    format!(
        "Showing “{}”'s branch",
        oyl_chat::thread::ellipsize(title, 40)
    )
}
