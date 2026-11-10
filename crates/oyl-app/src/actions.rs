//! Every command of the shell as a GPUI action, with its key, and the
//! native application menu that mirrors them.

use gpui_kit::component::input::{Copy, Cut, Paste, Redo, Search, SelectAll, Undo};
use gpui_kit::{App, KeyBinding, Menu, MenuItem, OsAction};

gpui_kit::actions!(
    oyl,
    [
        /// Opens the command palette (⌘K).
        CommandPalette,
        /// Opens the library of papers (⌘O).
        OpenLibrary,
        /// Opens the library at the new-paper form (⌘N).
        NewPaper,
        /// Picks a folder to open as a paper.
        OpenFolder,
        /// The source on the left, the paper on the right (⌘1).
        WriteMode,
        /// The agent on the left, the paper on the right (⌘2).
        AgentMode,
        /// Flips between light and dark.
        ToggleAppearance,
        /// Always light.
        UseLightAppearance,
        /// Always dark.
        UseDarkAppearance,
        /// Follow macOS.
        FollowSystemAppearance,
        /// Goes to the Agent tab (caret in the composer), or back to the tab
        /// it was opened from (⌘L).
        FocusAgent,
        /// Starts a new conversation with the agent.
        NewChat,
        /// Stops the agent's running turn.
        StopAgent,
        /// Shows where the cursor is in the preview (⇧⌘J).
        RevealInPreview,
        /// Exports the compiled paper as a PDF (⇧⌘E).
        ExportPdf,
        /// Exports the paper's sources as a zip archive.
        ExportSource,
        /// Exports the bibliography.
        ExportBibliography,
        /// Shows the paper folder in Finder.
        RevealInFinder,
        /// Zooms the preview in (⌘=).
        ZoomIn,
        /// Zooms the preview out (⌘−).
        ZoomOut,
        /// Fits the page width to the preview (⌘0).
        ZoomToFit,
        /// Closes the window (⌘W).
        CloseWindow,
        /// Minimizes the window (⌘M).
        Minimize,
        /// Zooms the window (the green traffic light).
        ZoomWindow,
        /// Hides Oyl (⌘H).
        Hide,
        /// Hides the other applications (⌥⌘H).
        HideOthers,
        /// Shows every application again.
        ShowAll,
        /// Quits Oyl (⌘Q).
        Quit,
        /// Closes the floating card on screen (Esc).
        DismissOverlay,
        /// The bibliography (⇧⌘R).
        ShowSources,
        /// Versions of the paper (⇧⌘H).
        ShowHistory,
        /// The headings (⇧⌘O).
        ShowOutline,
        /// The images in `figures/` (⇧⌘F).
        ShowFigures,
        /// The compiler's errors and warnings (⇧⌘M).
        ShowProblems,
        /// Polishes the editor's selection (⌘B, bound in the editor only).
        PolishSelection,
        /// Fact-checks the editor's selection (⌘I, bound in the editor only).
        FactCheckSelection,
        /// Asks the AI to do something with the editor's selection (⇧⌘B,
        /// bound in the editor only).
        AskAiAboutSelection,
        /// Signs this Mac in to Oyl Cloud (the device flow).
        SignInToCloud,
        /// Signs out of Oyl Cloud (papers stay on the Mac).
        SignOutOfCloud,
        /// Turns "Sync to Oyl Cloud" on or off for the open paper.
        ToggleCloudSync,
        /// Pulls and pushes the open paper now.
        SyncNow,
        /// Lists the account's papers in the library, to download one.
        OpenFromCloud,
    ]
);

/// The key context of the shell's root element.
pub const SHELL_CONTEXT: &str = "OylShell";
/// The key context of the overlay card.
pub const OVERLAY_CONTEXT: &str = "OylOverlay";
/// The key context of the Agent tab's sidebar and card.
pub const AGENT_CONTEXT: &str = "OylAgentTab";

/// Binds the shell's keys. A global key (no context) counts as bound at the
/// focused element itself, so it wins over any pane's binding of the same
/// key: the shell binds globally only keys no pane uses (⌘B and ⇧⌘B are
/// the editor's). Keys a deeper context must be able to take (⌘N) are bound
/// in the shell's own context instead.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-k", CommandPalette, None),
        KeyBinding::new("cmd-o", OpenLibrary, None),
        // In the shell's context rather than global, so that the Agent
        // tab's ⌘N (a new thread, bound deeper) wins inside it.
        KeyBinding::new("cmd-n", NewPaper, Some(SHELL_CONTEXT)),
        KeyBinding::new("cmd-1", WriteMode, None),
        KeyBinding::new("cmd-2", AgentMode, None),
        KeyBinding::new("cmd-l", FocusAgent, None),
        KeyBinding::new("cmd-shift-j", RevealInPreview, None),
        KeyBinding::new("cmd-shift-e", ExportPdf, None),
        KeyBinding::new("cmd-=", ZoomIn, None),
        KeyBinding::new("cmd--", ZoomOut, None),
        KeyBinding::new("cmd-0", ZoomToFit, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-shift-r", ShowSources, None),
        KeyBinding::new("cmd-shift-h", ShowHistory, None),
        KeyBinding::new("cmd-shift-o", ShowOutline, None),
        KeyBinding::new("cmd-shift-f", ShowFigures, None),
        KeyBinding::new("cmd-shift-m", ShowProblems, None),
        KeyBinding::new("escape", DismissOverlay, Some(OVERLAY_CONTEXT)),
        // Inside the Agent tab ⌘N starts a thread, not a paper.
        KeyBinding::new("cmd-n", NewChat, Some(AGENT_CONTEXT)),
    ]);
}

/// The native menu bar: Oyl, File, Edit, View, Paper, Agent, Window.
pub fn menus() -> Vec<Menu> {
    vec![
        Menu::new("Oyl").items([
            MenuItem::action("Command Palette…", CommandPalette),
            MenuItem::separator(),
            MenuItem::action("Sign In to Oyl Cloud…", SignInToCloud),
            MenuItem::action("Sign Out of Oyl Cloud", SignOutOfCloud),
            MenuItem::separator(),
            MenuItem::action("Hide Oyl", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Oyl", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Paper…", NewPaper),
            MenuItem::action("Open Library…", OpenLibrary),
            MenuItem::action("Open Folder…", OpenFolder),
            MenuItem::action("Open from Oyl Cloud…", OpenFromCloud),
            MenuItem::separator(),
            MenuItem::action("Save", oyl_editor::Save),
            MenuItem::separator(),
            MenuItem::action("Export PDF…", ExportPdf),
            MenuItem::action("Export Source (.zip)…", ExportSource),
            MenuItem::action("Export Bibliography…", ExportBibliography),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Find…", Search),
            MenuItem::action("Toggle Comment", oyl_editor::ToggleLineComment),
            MenuItem::separator(),
            MenuItem::action("Polish Selection", PolishSelection),
            MenuItem::action("Fact-Check Selection", FactCheckSelection),
            MenuItem::action("Ask AI About Selection…", AskAiAboutSelection),
        ]),
        Menu::new("View").items([
            MenuItem::action("Write", WriteMode),
            MenuItem::action("Agent", AgentMode),
            MenuItem::separator(),
            MenuItem::action("Zoom In", ZoomIn),
            MenuItem::action("Zoom Out", ZoomOut),
            MenuItem::action("Fit Page Width", ZoomToFit),
            MenuItem::separator(),
            MenuItem::action("Toggle Light/Dark", ToggleAppearance),
            MenuItem::action("Follow System Appearance", FollowSystemAppearance),
            MenuItem::separator(),
            MenuItem::action("Command Palette…", CommandPalette),
        ]),
        Menu::new("Paper").items([
            MenuItem::action("Sources", ShowSources),
            MenuItem::action("History", ShowHistory),
            MenuItem::action("Outline", ShowOutline),
            MenuItem::action("Figures", ShowFigures),
            MenuItem::action("Problems", ShowProblems),
            MenuItem::separator(),
            MenuItem::action("Reveal Cursor in Preview", RevealInPreview),
            MenuItem::action("Reveal in Finder", RevealInFinder),
            MenuItem::separator(),
            MenuItem::action("Sync to Oyl Cloud", ToggleCloudSync),
            MenuItem::action("Sync Now", SyncNow),
        ]),
        Menu::new("Agent").items([
            MenuItem::action("Ask the Agent", FocusAgent),
            MenuItem::action("New Chat", NewChat),
            MenuItem::action("Stop the Agent", StopAgent),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", ZoomWindow),
        ]),
    ]
}
