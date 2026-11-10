//! The editor's colors: Oyl's tokens instead of the component library's.
//!
//! The styled `Editor` projects the component theme onto its state on every
//! render (foreground, muted line numbers, background, highlight theme).
//! Oyl wants different ones (faint line numbers, a transparent ground, its
//! own syntax theme), and wants them per editor rather than by rewriting the
//! global component theme. [`StyledEditor`] wraps the editor element and puts
//! Oyl's style on the state after the editor has rendered and before its
//! text is laid out and painted, which is when the style is read.

use std::sync::Arc;

use gpui_kit::base::input::{DiagnosticColors, EditorState, InputEditorStyle};
use gpui_kit::{
    AnyElement, App, Bounds, Element, ElementId, Entity, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, Pixels, Window,
};
use oyl_ui::Tokens;

use crate::syntax::OylSyntaxTheme;

/// The editor style for the active tokens.
pub(crate) fn editor_style(tokens: &Tokens, syntax: Arc<OylSyntaxTheme>) -> InputEditorStyle {
    let clear = Hsla::transparent_black();
    InputEditorStyle {
        foreground: tokens.text,
        // Line numbers, placeholders.
        muted_foreground: tokens.text_faint,
        // No box: the text sits on the glass.
        background: clear,
        // Indent guides are off; nothing else draws with this.
        border: clear,
        selection: tokens.selection,
        caret: tokens.caret,
        diagnostics: DiagnosticColors {
            error: tokens.danger,
            warning: tokens.warning,
            info: tokens.text_soft,
            hint: tokens.text_faint,
        },
        highlight_styles: syntax,
        editor_invisible: Some(tokens.text_faint),
        editor_active_line: Some(tokens.line_highlight),
        editor_gutter_background: Some(clear),
        fold_icon_renderer: None,
    }
}

/// An editor element that paints with a given style.
pub(crate) struct StyledEditor {
    child: AnyElement,
    state: Entity<EditorState>,
    style: InputEditorStyle,
}

impl StyledEditor {
    pub(crate) fn new(
        child: AnyElement,
        state: Entity<EditorState>,
        style: InputEditorStyle,
    ) -> Self {
        Self {
            child,
            state,
            style,
        }
    }
}

impl IntoElement for StyledEditor {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for StyledEditor {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // Rendering the editor projects the component theme onto the state.
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        // The text element reads the style while it prepaints and paints.
        let style = self.style.clone();
        self.state
            .update(cx, |state, _| state.set_editor_style(style));
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}
