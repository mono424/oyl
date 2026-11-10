<p align="center"><img src="assets/icon/oyl-blob.png" width="180" alt="Oyl logo"></p>

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
- Two tabs: **Write** (the source and the paper side by side) and **Agent**
  (the agent and the paper).
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
- **⇧⌘B — ask AI:** tell the AI what to do with the selection ("make it more
  formal", "turn into a list") and it is rewritten in place.
- **⌘I — fact check:** claims in the selection are verified (web search and
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
| ⌘1 ⌘2 | Write · Agent |
| ⌘L | Jump to the agent and back |
| ⌘B · ⇧⌘B · ⌘I | Polish · ask AI · fact-check the selection |
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
| `oyl-sync` | Oyl Cloud sync of paper folders: device sign-in, push, pull through the history's branches |
| `oyl-typeset` | Embedded Typst, external LaTeX, PDF rasterizing |
| `oyl-agent` | Harness detection; the Sorcar, Claude Code and Codex harnesses |
| `oyl-ui` | Design tokens, light/dark, glass shapes |
| `oyl-editor` · `oyl-preview` · `oyl-chat` · `oyl-panels` | The panes |
| `oyl-app` | The application shell |

The UI is built with [GPUI](https://gpui.rs) through
[GPUI Kit](https://gpui-kit.com). See [PLAN.md](PLAN.md) for the design and
architecture.

## Oyl Cloud / web

The same repository also holds Oyl's web side: a browser app, its API and a
landing page, in a pnpm workspace next to the Cargo workspace. It is where your
courses, lecture material and writing live online, synced live to every
browser, and where an AI client (ChatGPT today) works with them through Oyl's
MCP server. Papers written in the desktop app sync to it
([docs/sync-protocol.md](docs/sync-protocol.md)).

What it does:

- **Courses and material.** Add PDF slides, LaTeX, Markdown or text notes to a
  course. PDFs stay as they are to read page by page; their text and pictures
  become searchable. LaTeX and TikZ render in the browser.
- **Documents.** Write a research proposal or an essay in Markdown or LaTeX
  with the course's papers beside it: cite with `\cite{key}` or `[@`, read a
  paper's PDF next to the text, add images, export PDF or `.tex` with a
  `references.bib`. Every change is kept in History and can be restored.
- **Homework.** Answer an exercise sheet part by part next to the sheet, with
  live Markdown, math and images, and export a clean PDF.
- **Sources.** Follow a bCourses course or a course website: new and changed
  lecture PDFs come in by themselves (a [hockeymask](https://github.com/mono424/hockeymask) mapping decides what to take).
- **Sites.** Upload a built website to a course and open it in place.
- **Papers.** Papers from Oyl on your Mac, synced with their history: read
  any revision's source in the browser. A Mac connects by approving its code
  on `/connect`.
- **Study with an AI.** Connect ChatGPT once over OAuth and study by voice from
  your own courses: it reads your material, keeps a per-course knowledge graph
  and a concept-by-concept map of what you understood, quizzes you, and saves
  each session with a recap. It can also edit your documents and homework.

### Stack

| Part | Technology |
| --- | --- |
| Web app (`apps/web`) | SolidJS, TypeScript, Vite, Tailwind CSS |
| Landing page (`apps/landing`) | Astro |
| API (`apps/api`) | Hono on Cloudflare Workers (a sp00ky Worker backend), Workers AI, the MCP server |
| Data and search | SurrealDB with vector and full-text search (`schema/`) |
| Live sync | sp00ky, with a local store in the browser |
| Course knowledge graph | SurrealDB Agent Memory |

### Running it

Use **Node.js 22.22 or newer**, **pnpm 10.20.0** and **Docker** for the local
stack. Cloudflare (document processing, embeddings) and a SurrealDB Agent
Memory Context (the knowledge graph) are needed for everything to work; see
[docs/setup.md](docs/setup.md#accounts-you-need).

```sh
pnpm install
cp apps/api/.dev.vars.example apps/api/.dev.vars   # then fill it in (docs/setup.md)
pnpm dev                                           # database, sync node, web app and API
```

| Service | Local URL |
| --- | --- |
| Web app | <http://localhost:5180> |
| API Worker | <http://localhost:8787> |
| Landing page (`pnpm dev:landing`) | <http://localhost:4322> |

| Command | What it does |
| --- | --- |
| `pnpm dev` | Start the local database, sync node, web app and API |
| `pnpm dev:landing` | Start the landing page (copy `apps/landing/.env.example` to `.env.local` first) |
| `pnpm type-check` | Check types across the pnpm workspace |
| `pnpm --filter @oyl/web build` | Build the web app |
| `pnpm build:landing` | Build the static landing page |
| `pnpm --filter @oyl/api test` | The API's tests |

The hosted app is meant to live at `https://oyl.stg.spky.cloud` (sp00ky project
`oyl`); see [docs/setup.md](docs/setup.md) for deployment, schema changes and
the MCP server. ChatGPT reaches the MCP server over HTTPS only; the setup guide
covers a tunnel for trying the local stack.

### Repository map

| Path | Contents |
| --- | --- |
| [`crates/`](crates) | The desktop app (Cargo workspace) |
| [`apps/web`](apps/web) | The web app |
| [`apps/api`](apps/api) | Document ingestion, search, the MCP server |
| [`apps/landing`](apps/landing) | The landing page |
| [`schema`](schema) | Database schema and migrations (sp00ky) |
| [`shared`](shared) | TypeScript shared by the web app and the API |
| [`scripts`](scripts) | `bundle.sh` (desktop app) and maintenance scripts (web) |
| [`dev`](dev) | sp00ky configuration of the local stack |
| [`assets`](assets) | The logo and app icon ([`assets/brand`](assets/brand) for the web) |
| [`docs`](docs) | [Setup and deployment](docs/setup.md), [design tokens on the web](docs/brand/README.md) |

Landing page photo by [Sanket Mishra on Unsplash](https://unsplash.com/photos/a-girl-sitting-on-a-bench-using-a-laptop-vvE2GOnGudg).
