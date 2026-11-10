//! A source's PDF, read inside Oyl: a second preview pane in a wide card,
//! titled with the cite key, with "Cite page N" for the page in view.

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, IconName, Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Render, Window, div, px,
};
use oyl_preview::PreviewPane;
use oyl_ui::prelude::*;

/// What the viewer asks of the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PdfViewerEvent {
    /// Insert a citation of `key` at page `page` (1-based) and go back to
    /// the source.
    CitePage {
        /// The source's cite key.
        key: String,
        /// The page in view.
        page: u32,
    },
    /// Close the viewer.
    Close,
}

/// The page to cite when the preview shows page `current` (0-based).
pub fn cite_page(current: usize) -> u32 {
    u32::try_from(current)
        .unwrap_or(u32::MAX - 1)
        .saturating_add(1)
}

/// A source's PDF.
pub struct PdfViewer {
    key: String,
    path: PathBuf,
    preview: Entity<PreviewPane>,
    error: Option<String>,
    loaded: bool,
}

impl EventEmitter<PdfViewerEvent> for PdfViewer {}

impl Focusable for PdfViewer {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.preview.focus_handle(cx)
    }
}

impl PdfViewer {
    /// Opens the PDF of `key` at `path`; it is read off the main thread.
    pub fn new(key: String, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let preview = cx.new(|cx| PreviewPane::new(window, cx));
        let file = path.clone();
        cx.spawn(async move |this, cx| {
            let opened = cx
                .background_spawn(async move {
                    let bytes = std::fs::read(&file)
                        .map_err(|err| format!("{} could not be read: {err}", file.display()))?;
                    oyl_typeset::rasterize_pdf(bytes)
                        .map_err(|err| format!("This PDF could not be opened: {err:#}"))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loaded = true;
                match opened {
                    Ok(document) => this
                        .preview
                        .update(cx, |preview, cx| preview.set_document(Some(document), cx)),
                    Err(message) => this.error = Some(message),
                }
                cx.notify();
            });
        })
        .detach();
        Self {
            key,
            path,
            preview,
            error: None,
            loaded: false,
        }
    }

    /// The source's cite key.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The page in view, 1-based.
    pub fn page(&self, cx: &App) -> u32 {
        cite_page(self.preview.read(cx).current_page())
    }

    /// Whether the PDF has been read (successfully or not).
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// Pages of the PDF.
    pub fn page_count(&self, cx: &App) -> usize {
        self.preview.read(cx).page_count()
    }
}

impl Render for PdfViewer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let k = cx.tokens().clone();
        let page = self.page(cx);
        let has_pages = self.page_count(cx) > 0;
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let body = match &self.error {
            Some(error) => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.))
                .text_sm()
                .text_color(k.danger)
                .child(error.clone())
                .into_any_element(),
            None => div()
                .flex_1()
                .min_h_0()
                .child(self.preview.clone())
                .into_any_element(),
        };
        div()
            .v_flex()
            .size_full()
            .child(
                div()
                    .h_flex()
                    .flex_none()
                    .gap(px(8.))
                    .h(px(44.))
                    .px(px(14.))
                    .border_b_1()
                    .border_color(k.hairline)
                    .child(oyl_ui::accent_chip(self.key.clone(), cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(k.text_faint)
                            .child(name),
                    )
                    .child(
                        Button::new("pdf-cite-page")
                            .primary()
                            .xsmall()
                            .label(format!("Cite page {page}"))
                            .disabled(!has_pages)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let page = this.page(cx);
                                cx.emit(PdfViewerEvent::CitePage {
                                    key: this.key.clone(),
                                    page,
                                });
                            })),
                    )
                    .child(
                        Button::new("pdf-close")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip("Close (Esc)")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(PdfViewerEvent::Close))),
                    ),
            )
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_are_cited_from_one() {
        assert_eq!(cite_page(0), 1);
        assert_eq!(cite_page(11), 12);
        assert_eq!(cite_page(usize::MAX), u32::MAX);
    }
}
