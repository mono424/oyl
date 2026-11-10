//! Oyl's source editor.
//!
//! [`EditorPane`] holds the open files of a paper and shows one at a time,
//! with no chrome and no background of its own: the app puts it in a card
//! (in zen mode with roomier margins). It is written for prose:
//! soft-wrapped, generously leaded Menlo, faint line numbers, an amber caret.
//!
//! What it does for the app:
//!
//! - **Saving**: 400 ms after the last change, an atomic write (temporary
//!   file and rename); ⌘S saves at once. A failed write keeps the text and
//!   says so under it ([`EditorEvent::SaveFailed`]).
//! - **Outside changes** ([`EditorPane::reload_from_disk`]): the editor's own
//!   writes are recognized and ignored; a clean buffer takes the new text
//!   undoably, keeping cursor and scroll; a dirty one is three-way merged.
//! - **Highlighting**: LaTeX and BibTeX through the built-in highlighter,
//!   Typst through `typst-syntax`, both in Oyl's restrained theme.
//! - **Completion**: citation keys inside `\cite{…}` and its relatives and
//!   after `@` in Typst; labels for `\ref{…}`; environments, sectioning
//!   commands and Typst `#figure`/`#table`/`#cite` snippets; headings.
//! - **Diagnostics** ([`EditorPane::set_diagnostics`]): a squiggle under the
//!   spot, the message on hover.
//! - **Events** ([`EditorEvent`]) for edits, saves, the cursor, and pasted
//!   images or dropped files, which the app stores and then inserts with
//!   [`EditorPane::insert_at_cursor`].
//!
//! Keys inside the pane: ⌘S save, ⌘/ toggle line comments, ⌘F search (the
//! editor's own).

mod comments;
mod completion;
mod diagnostics;
mod io;
mod language;
mod merge;
mod pane;
mod style;
mod syntax;

pub use pane::{
    AUTOSAVE_DELAY, CURSOR_DEBOUNCE, EditorEvent, EditorPane, KEY_CONTEXT, Save, ToggleLineComment,
};
