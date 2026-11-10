<p align="center"><img src="assets/logo.png" width="160" alt="Oyl logo"></p>

<h1 align="center">Oyl</h1>

<p align="center"><b>A quiet, native paper-writing IDE where an AI agent writes with you.</b></p>

Oyl is a macOS app for writing papers in **Typst** or **LaTeX**. A paper is just
a folder on your disk — no account, no database, no import step. You write in a
calm editor on frosted glass, the real pages float beside it and update as you
type, and an AI agent can edit the paper with you:
[KISS Sorcar](https://github.com/ksenxx/kiss_ai) by default, Claude Code or
Codex directly, or any other framework you plug in.

Oyl is opinionated and sets itself up. Typst is compiled in-process, LaTeX uses
whatever is installed (or fetches tectonic), and the agent is wired to the
Claude Code or Codex login already on your machine — no API keys needed.

> **Status: early, in active development.** The building blocks are in place
> and tested; the app shell is being reworked right now (the Agent tab), so the
> `oyl-app` crate may not compile on every commit. macOS only for now.

## Features

**Writing**
- Four tabs, no sidebar: **Write** (the source, full width), **Split** (source
  and paper side by side), **Read** (the paper alone), **Agent** (the agent and
  the paper).
- Source in a darker card on frosted glass; real pages beside it, rasterized
  lazily and sharp at any zoom. Click a page to jump to its source.
- Typst compiles on every keystroke (a few milliseconds incremental); LaTeX
  compiles on save via latexmk, tectonic or pdflatex into a hidden build folder.
- Syntax highlighting, completion for citations (`\cite{…}`, `@key`), labels
  and environments, inline compiler errors, autosave, three-way merge when a
  file changes underneath you.
- Light and dark, one amber accent, a command palette (⌘K) for everything.

**AI**
- **Agent** tab: chat with an agent that edits the paper's files in place, with
  its running tasks and recent threads beside it and the paper updating as it
  works. Several tasks can run at once.
- **⌘B — polish:** select text and have spelling, grammar and flow fixed in
  place, one undo away.
- **⇧⌘B — fact check:** claims in the selection are verified (web search and
  your bibliography); wrong or doubtful ones are underlined with what is wrong
  and a suggested rewrite, which you can refine with your own remark.
- Every agent turn is versioned before and after, and can be undone.
- Harnesses are pluggable: one trait, three built in (Sorcar, Claude Code,
  Codex), detected automatically.

**Sources, figures, history**
- Add references by DOI, arXiv id, link, pasted BibTeX or title search; drop a
  PDF to identify and attach it. `refs.bib` stays the single source of truth.
- Paste or drop images into `figures/`; a figure block is inserted for you.
- Automatic version history of the whole folder with word-level diffs and
  restore; nothing is ever lost.
- Outline, problems list, export to PDF, a source zip or the bibliography.

## Getting started

Requirements: macOS 13 or newer, the Xcode command line tools, and
[Rust](https://rustup.rs) (the exact toolchain is pinned in
`rust-toolchain.toml` and installed automatically). For the AI features, log in
to [Claude Code](https://claude.com/claude-code) or Codex once; Oyl finds them.
LaTeX is optional — Typst works out of the box.

```sh
git clone https://github.com/mono424/oyl && cd oyl
scripts/bundle.sh --open              # build Oyl.app and launch it
```

Other ways to run it:

```sh
cargo run -p oyl-app -- path/to/paper   # open a paper folder directly
cargo run -p oyl-app -- --self-test     # drive a real window through the main paths
```

The first time you use Sorcar, Oyl prepares its Python environment in the
background (a one-time download of a few hundred MB through
[uv](https://docs.astral.sh/uv/)).

## Keyboard

| Keys | Action |
| --- | --- |
| ⌘K | Command palette |
| ⌘O / ⌘N | Library / new paper |
| ⌘1 ⌘2 ⌘3 ⌘4 | Write · Split · Read · Agent |
| ⌘L | Jump to the agent and back |
| ⌘B / ⇧⌘B | Polish / fact-check the selection |
| ⇧⌘R ⇧⌘H ⇧⌘O ⇧⌘F ⇧⌘M | Sources · History · Outline · Figures · Problems |
| ⇧⌘J | Show the cursor's position in the paper |
| ⇧⌘E | Export PDF |
| ⌘= ⌘− ⌘0 | Zoom the paper |
| ⌘/ | Toggle comment |

## A paper folder

```
my-paper/
  oyl.toml        name, engine, entry file, bibliography, citation style
  main.typ        or main.tex (more files are fine)
  refs.bib        the sources
  figures/        images
  sources/        PDFs of cited works
  AGENTS.md       how an agent should work on this paper (CLAUDE.md points to it)
  .oyl/           history, chats, build output — keep it out of git
```

Any folder with a `main.typ` or `main.tex` opens as a paper.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
```

| Crate | What it is |
| --- | --- |
| `oyl-model` | Shared types: papers, diagnostics, the typesetting and agent contracts |
| `oyl-core` | Paper folders: manifest, library, settings, templates, outline, export, watching |
| `oyl-sources` | Bibliography, citations, lookups (Crossref, arXiv), source PDFs, figures |
| `oyl-history` | Versions of a paper folder, diffs, restore |
| `oyl-typeset` | Embedded Typst, external LaTeX, PDF rasterizing |
| `oyl-agent` | Harness detection; the Sorcar, Claude Code and Codex harnesses |
| `oyl-ui` | Design tokens, light/dark, glass shapes |
| `oyl-editor` · `oyl-preview` · `oyl-chat` · `oyl-panels` | The panes |
| `oyl-app` | The application shell |

The UI is built with [GPUI](https://gpui.rs) through
[GPUI Kit](https://gpui-kit.com). See [PLAN.md](PLAN.md) for the design and
architecture.
