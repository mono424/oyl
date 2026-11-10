# Oyl — plan

Oyl is a native paper-writing IDE. A paper is a folder on disk, written in
Typst or LaTeX, and written *with* an agent: KISS Sorcar by default, any other
harness by choice. Oyl is opinionated and sets itself up.


## Principles

1. **A paper is a folder.** No database, no import/export step. Everything
   Oyl adds lives in `oyl.toml` and a hidden `.oyl/` inside the folder.
   Any folder with a `main.typ` or `main.tex` opens as a paper.
2. **Zero setup.** Typst is compiled in-process. LaTeX uses what is installed
   (tectonic, latexmk, pdflatex) and fetches tectonic when nothing is. The
   agent is found, not configured: Oyl looks for `claude` and `codex` and
   wires Sorcar to whichever exists, using the subscription already logged in.
3. **Opinionated.** Always autosave. Always keep history. One layout, three
   modes. One accent color. No settings screen full of knobs.
4. **Zen.** No sidebar, no tab strip, no status bar. Navigation is the command
   palette. Chrome fades when you write.
5. **The agent edits files; Oyl watches the folder.** Agent edits, your
   edits and edits from any other tool all arrive the same way, so every
   harness works and nothing is special-cased.
6. **Open harness.** A harness is one trait. Sorcar, Claude Code and Codex are
   three implementations of it.

## Design

Not a clone of the reference screenshot: it borrowed only "dark, calm, rounded".

- **Window:** frosted glass. The desktop shows through, blurred, under a warm
  tinted veil. Light and dark are both first-class; the toggle sits in the
  island, and "System" follows macOS.
- **The paper is the only opaque thing.** Pages are true white sheets with a
  soft shadow, floating directly on the glass — no panel behind them, free
  scrolling, pinch/⌘-scroll zoom.
- **The source sits on the glass too.** No editor box: text on a slightly
  calmer veil, faint line numbers, amber caret.
- **The header sits flat in the window:** one row at the very top, level with
  the traffic lights, no pill, no border, no line under it — paper name and
  engine on the left, the mode switch centered, compile state, theme toggle and
  ⌘K on the right. Content starts below it; nothing overlaps it.
- **No lines between things.** Write has no divider. A small pill appears
  between source and paper only while the pointer is over the paper; dragging
  it resizes.
- **Floating things are cards:** the chat composer, the command palette, the
  overlays.
- **One accent:** amber — lamplight on paper. Used for the caret, selection,
  the active mode, citations, and the agent's presence. Nothing else is colored
  except meaning (error, warning, success).
- **Type:** system UI font; Menlo for source; a serif (Charter) for the few
  display moments.

Tokens live in `crates/oyl-ui` (`cx.tokens()`); no crate uses color literals.

### Modes

Two tabs, centered in the header:

| Tab | What is on screen | Key |
| --- | --- | --- |
| **Write** | Left: source. Right: the paper, floating. No divider; a hover pill resizes. | ⌘1 |
| **Agent** | Left: the agent and its threads. Right: the paper. | ⌘2 |

Older settings that stored "split", "read" or the full-width "write" open in
Write; "agent" opens in Agent.

The **source** sits in a darker card (8px gaps to the window, the header and the
column boundary). Its text is clipped at the card's inner padding.

The **agent** is the second tab, **Agent** (⌘2): Write's layout with the agent's
card in place of the source — thread on top, composer at the bottom, harness
and model picker inside the composer — and the paper on the right, so the
agent's edits are seen landing. ⌘L jumps to it and back; a dot on the tab shows
a running turn from any tab. A quiet sidebar on the left of the Agent tab
lists the paper's recent threads for quick questions. Long-running tasks keep
going in the background: a button at the bottom of the sidebar reads "N agents
running" and opens a tray of them (live activity, elapsed time, stop) to check
back in; a toast says when one finishes off-screen. Several threads can run at
once.

Every agent task works on its own **branch**: a private copy of the paper in
`.oyl/branches/`, never the user's files. When it finishes and the user has
not changed the files it touched, its changes are applied automatically (a
normal, undoable version). If the user changed them meanwhile, the user's
changes are merged into the branch first and Oyl asks: review the result,
pick per conflict, apply or discard. While a task runs, the Agent tab's paper
shows that task's branch, so its work is visible before it lands.

Everything else is an overlay opened from the palette or a key, never a
permanent panel:

- **Command palette** ⌘K — switch paper, open file, jump to section, insert
  citation, insert figure, every command.
- **Papers** ⌘O — the library: recent papers, new paper, open folder.
- **Sources** ⇧⌘R — the bibliography.
- **History** ⌘⇧H — versions, diff, restore.
- **Outline** ⌘⇧O, **Figures** ⌘⇧F, **Problems** ⌘⇧M.

## Architecture

Cargo workspace, Rust 1.99.1 (pinned in `rust-toolchain.toml`; the GPUI
snapshot needs it). UI is GPUI through `gpui-kit 0.9.1` (re-exports gpui,
gpui-base, gpui-component, assets).

```
crates/
  oyl-model     no UI  FROZEN shared types (the contract)
  oyl-core      no UI  project, library, settings, templates, outline,
                         stats, export, folder watching
  oyl-sources   no UI  bibliography, cite keys, DOI/arXiv/URL lookup,
                         citation scanning, source PDFs, figures
  oyl-history   no UI  versions of the folder, diff, restore
  oyl-typeset   no UI  Compiler: embedded Typst, external LaTeX, PDF raster
  oyl-agent     no UI  detection, Registry, Sorcar / Claude Code / Codex
  oyl-ui        GPUI   tokens, light/dark, glass shapes
  oyl-editor    GPUI   EditorPane: buffers, highlighting, cite completion
  oyl-preview   GPUI   PreviewPane: floating pages, zoom, lazy raster
  oyl-chat      GPUI   ChatPane: thread, streaming, tools, composer
  oyl-panels    GPUI   overlays: library, sources, history, outline, figures
  oyl-app       bin    shell, modes, island, palette, wiring → `oyl`
```

`oyl-model` is the contract — written first, additive-only afterwards. Every
phase-1 crate depends on it (and on `oyl-ui`) and on no other workspace
crate, so the crates are built in parallel without ever breaking each other:

- `oyl_model::*` — `Engine`, `Diagnostic`, `SourceLocation`, `OutlineItem`,
  `CiteCandidate`, `Author`.
- `oyl_model::typeset` — `CompileRequest`, `CompileOutput`,
  `RenderedDocument` (lazy per-page raster), `PageImage`, `ToolchainStatus`.
- `oyl_model::agent` — `Harness`, `HarnessInfo`, `TurnRequest`,
  `AgentEvent`, `TurnHandle`.

### Data flow

```
 keystroke ─► EditorPane ─► Edited ─┬─► (Typst) compile with unsaved overlay ─► PreviewPane
                                    └─► autosave 400ms ─► (LaTeX) compile from disk ─► PreviewPane
 agent turn ─► Harness ─► AgentEvent stream ─► ChatPane
                  └─► writes files ─► folder watcher ─► EditorPane.reload ─► compile
 before/after each turn ─► history snapshot (author: Agent)
```

### A paper folder

```
my-paper/
  oyl.toml          name, engine, main file, bibliography, citation style
  main.typ | main.tex
  refs.bib            the sources — the single source of truth for citations
  figures/            images, imported or pasted
  sources/            PDFs of cited papers (linked from .oyl/sources.json)
  AGENTS.md           how an agent should work on this paper (also CLAUDE.md)
  .oyl/             history/, chat/, build/, sources.json   (git-ignored)
```

### Typesetting

- **Typst** runs in-process (`typst` 0.15): instant, incremental, compiles the
  unsaved buffer on every keystroke, packages fetched from Typst Universe on
  demand, fonts from the system plus the embedded set. Pages are rasterized by
  `typst-render`, lazily, at the zoom on screen. Click-to-source through the
  layout frames.
- **LaTeX** runs out-of-process into `.oyl/build/`: tectonic if present,
  else latexmk, else pdflatex; tectonic is downloaded into Oyl's app-support
  folder when none exists. The log is parsed into diagnostics. The PDF is
  rasterized in-process by `hayro` (pure Rust), through the same
  `RenderedDocument` interface, so the preview does not know the difference.
- New papers default to **Typst**.

### Agents

- **Sorcar** (default): the `kiss-agent-framework` Python package, run through
  `uv` in an isolated environment (uv itself is fetched if missing), driven by
  a small bridge script that Oyl ships and that streams JSON events. Model:
  `cc/opus` when `claude` is installed, else `codex/default` when `codex` is,
  else the first provider with an API key in the environment. Sorcar's
  git-worktree isolation is turned off: it edits the folder in place and Oyl
  keeps the history.
- **Claude Code** direct: `claude -p --output-format stream-json`.
- **Codex** direct: `codex exec --json`.
- CLIs are found on `PATH`, in the usual install locations, and through a
  login shell, because an app launched from the Dock has almost no `PATH`.
- Every turn is given the paper's context (engine, main file, active file,
  selection, current compile errors, the bibliography rules) and the same
  writing rules: stay in the paper's format, cite only keys that exist in
  `refs.bib`, never invent references, keep the paper compiling.

## Features

Baseline is the document editor of an earlier web product; Oyl must cover
all of it and goes further where a desktop app can.

| Area | Web baseline | Oyl |
| --- | --- | --- |
| Format | Markdown, LaTeX | Typst, LaTeX; multi-file projects |
| Preview | LaTeX→Markdown approximation | Real compile, real pages, click-to-source |
| Sources | Add from course PDF / upload / by hand | `refs.bib` + add by DOI, arXiv id, URL, BibTeX paste, PDF drop, by hand |
| Cite keys | Auto `stark2019`, `stark2019b` | Same rule |
| Citing | `\cite{` / `[@` autocomplete, Cite button, cited-count, warn on removing a cited paper | Same, for `\cite…{` and Typst `@key`; unknown keys flagged |
| Source PDFs | Open at page, cite page | Open the linked PDF; page cite |
| Images | Paste, drop, picker; re-encode large/HEIC | Same → `figures/`, inserts a figure block with caption and label |
| History | Auto versions, You/AI, +/−, diff, restore | Same, stored in `.oyl/history`; a version before and after every agent turn |
| Saving | Autosave, merge external edits | Autosave 400ms; external and agent edits merged into the open buffer |
| Export | PDF, source, zip, `.bib` | PDF, zip of sources, `.bib` |
| Stats | Word count, save state | Word count, page count, compile state |
| AI | External ChatGPT over MCP, direct apply | Built-in agent chat, streaming, tool activity, per-turn undo |
| Outline | — | Sections, jump, in the palette |
| Templates | One starter | Article, conference two-column, thesis, notes — per engine |
| Diagnostics | — | Compiler errors inline and listed; "Fix with agent" |
| Setup | — | Automatic: Typst built in, LaTeX detected or fetched, agent detected |

## Build order

- **Phase 0 — foundation (done by the lead):** workspace, toolchain pin,
  contracts, design tokens, blurred window proven.
- **Phase 1 — eight modules in parallel**, one agent each, one crate each:
  core, sources, history, typeset, agent, editor, preview, chat. Each crate
  builds and is tested on its own; UI crates ship an example binary that shows
  the pane alone.
- **Phase 2 — two in parallel:** panels (library, sources, history, outline,
  figures, problems) and the app shell (modes, island, palette, compile loop,
  watcher, history hooks, first-run setup).
- **Phase 3 — integration (lead):** wire panels into the shell, build, run,
  look at it, fix.

## Rules for everyone working on this

- Stay in your crate. `oyl-model` and `oyl-ui` are frozen. Do not edit the
  root `Cargo.toml` or another crate; add dependencies in your own `Cargo.toml`.
- Read the real API before using it: sources are in
  `~/.cargo/registry/src/*/{gpui-pre-0.3.12,gpui-base-0.9.1,gpui-component-0.9.1,gpui-kit-0.9.1}`.
  The API differs from older GPUI/gpui-component; do not write it from memory.
- Colors come from `cx.tokens()`. No literals outside `oyl-ui`.
- No placeholder features: what is drawn works.
