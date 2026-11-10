# Setup and deployment

[Back to TalkToBolo](../README.md)

This guide covers the local stack, production deployment, and connecting
ChatGPT through the MCP server. Workspace packages and deployment resources use the internal
`studycall` name.

## How it fits together

```
 Browser (SolidJS + sp00ky)                         ChatGPT (web, iOS, Android)
   │  live queries / writes ──► SurrealDB ◄── SSP    │  voice, study tools
   │                              ▲                  │
   │  /api/* (Bearer: SurrealDB   │ system + user    │  /mcp (OAuth)
   │   session token)             │ queries          │
   │  job rows ──► SSP ──► /jobs/* │                  │
   ▼                              │                  ▼
 API (apps/api, a sp00ky Worker backend) ─────────────────────────────────
   /jobs/*           jobs from the `job` outbox: re-index, lecture progress
                     (apps/api/openapi.yml)
   /api/materials    PDF upload -> `materials` bucket (kept) -> Workflow: images
                     and text layer (unpdf) -> chunks with page numbers
                     -> embeddings (Workers AI) -> SurrealDB HNSW/BM25
                     -> course knowledge graph (Agent Memory, when configured)
   GET /api/materials/:id/pdf  the original PDF, for the in-app viewer
   /api/fetchers     sources: bCourses courses and course websites, checked on a
                     schedule (FetchSource Workflow) -> hockeymask mapping
                     -> new and changed PDFs and pages become materials
   /material-assets/:id  images extracted from PDFs
   /mcp              MCP server (Streamable HTTP) for ChatGPT
   /oauth/*          account linking for ChatGPT
```

| Path | What |
| --- | --- |
| [`schema/src/schema.surql`](../schema/src/schema.surql) | The sp00ky schema: users, courses, materials, study sessions (the `call` table, with the transcripts of earlier in-app calls), connections (`api_key`), access methods. `spky generate` turns it into `apps/web/src/schema.gen.ts`. |
| `apps/api/src/server-schema.surql` | Server-only tables (search chunks with embeddings and page numbers, extracted images, PDF parts) and their HNSW/full-text indexes. Applied by the Worker; see the comment at the top. |
| [`schema/buckets/materials.surql`](../schema/buckets/materials.surql) | The `materials` bucket: uploaded PDFs (in parts of at most 1 MiB) and the images extracted from them. Listed under `buckets:` in both sp00ky configs. |
| [`apps/web`](../apps/web) | SolidJS app. Every read is a named live query in `src/lib/query.ts`. |
| [`apps/api`](../apps/api) | Cloudflare Worker (Hono): ingest Workflow, search, assets, MCP. On Sp00ky Cloud it is the `api` Worker backend; `openapi.yml` lists its jobs. |
| [`apps/api/src/fetch`](../apps/api/src/fetch) | Sources: the Canvas and web page connectors, the [hockeymask](https://github.com/mono424/hockeymask) mapping that decides what to import, the import ledger and the `FetchSource` Workflow. See [Sources](#sources-bcourses-and-course-websites). |
| [`schema/src/outbox/api.surql`](../schema/src/outbox/api.surql) | The `job` outbox: the web app writes a row, the sync node calls the API with its payload. |
| `sp00ky.yml` / `dev/sp00ky.yml` | Staging (free plan, shared SurrealDB, the API as a Worker backend) / local development. |

## Accounts you need

- **SurrealDB Cloud** instance running SurrealDB 3.x for production. Local development uses Docker.
- **Sp00ky Cloud** account for deploying the sync node and web app.
- **Cloudflare Workers Paid** account for the configured ingestion Workflow CPU limit and Workers AI embeddings.
- **SurrealDB Agent Memory Context and API key** for each course's knowledge graph. This is a separate service from the SurrealDB SQL endpoint.
- **ChatGPT** with custom MCP servers (plugins) allowed on the plan, for studying.

### Agent Memory and lecture progress

Set `AGENT_MEMORY_URL` to your Context's full data-plane base URL, for example
`https://YOUR-CONTEXT-HOST/api/v1/YOUR-CONTEXT-ID`, and store its key as
`AGENT_MEMORY_API_KEY` in `.dev.vars` or the vault (`spky env set`). Use a dedicated Context
per environment. The Worker key needs `scope:create`, `memory:write`, and
`memory:read` for the `talktobolo/*` subtree. Never expose it to the browser.
See [SurrealDB's Context setup](https://surrealdb.com/docs/agent-memory/quickstarts/surrealdb-cloud)
and [REST API reference](https://surrealdb.com/docs/agent-memory/reference/rest-api).

Ingestion uploads each search section separately to Agent Memory and waits for
graph extraction before marking the material ready. Each section uses a
synthetic `observedAt` ordered by lecture position. Owner, course, material and
revision determine its scope. Reindex existing material after configuring
Agent Memory to build its graph; content edits reset progress and use a fresh
revision scope so old facts cannot enter current retrieval.

On a lecture page, **Your place in the lecture** records the last section the
student reached. Uploading or opening slides does not advance this boundary.
In ChatGPT, explain mode teaches the next section and moves the place forward
once the student confirms it (`record_progress` or `save_study_session`). Quiz
and question modes use reached sections. Reaching a section establishes
exposure, never mastery.

`prepare_study_session` adds the course graph's facts up to the student's
place (`course_concepts`), and any student evidence recorded during earlier
in-app voice calls (`student_evidence`). Each section has its own student
evidence scope, so rewinding progress also excludes evidence about later
sections. Sessions in ChatGPT do not write student evidence.

Course graph reads use `asOf` and `include: ["facts"]`; raw passages are filtered
by the current material revision and reached position in SQL before retrieval.
This matters because Agent Memory's `asOf` does not gate raw uploaded passages.
See [temporal retrieval](https://surrealdb.com/docs/agent-memory/reasoning/temporal-validity).
The graph is partitioned by lecture revision, so knowledge is recalled from
relevant lectures without merging one student's evidence into course truth.

With neither setting present, lecture progress and permitted passages still
work; no course graph is built. An unavailable configured memory service never
widens passage retrieval. It fails ingestion with a retryable upload error and
omits recalled memory from study sessions.

Deleting material or a course removes the local records. Agent Memory copies
remain under their revision scopes and are no longer queried.
For permanent erasure, use the Context's scoped erase operation as described in
[SurrealDB's forgetting guide](https://surrealdb.com/docs/agent-memory/operations/forget).

Run `pnpm --filter @studycall/api test` for the memory contract checks. With the
local Docker database running on port 8666, run
`BOLO_TEST_SURREAL=1 pnpm --filter @studycall/api test` to include real database
boundary and evidence checks. These create and remove a separate test namespace.

## Local development

### Database names

The local and production configurations use different namespace/database pairs.
Keep the API Worker and web app pointed at the same pair for each environment.

| Environment | Namespace | Database | Configuration |
| --- | --- | --- | --- |
| Local | `main` | `studycall` | [`dev/sp00ky.yml`](../dev/sp00ky.yml) |
| Staging | `talktobolo` | `main` | [`sp00ky.yml`](../sp00ky.yml) |

The checked-in [`apps/api/wrangler.jsonc`](../apps/api/wrangler.jsonc) defaults
to the local pair. Update its `SPKY_DB_NS`, `SPKY_DB_NAME`, and `APP_ORIGIN` before
deploying the API with wrangler. On sp00ky's Worker backend the platform sets
`SPKY_DB_*` itself.

### Start the local stack

Requirements: Node 22.22.0 or newer, pnpm 10.20.0, Docker, and for the course knowledge graph the Agent Memory settings above. Run the commands in this guide from the repository root.

```bash
pnpm install
cp apps/api/.dev.vars.example apps/api/.dev.vars   # fill in the database login
pnpm dev                                            # spky dev --path dev
```

`pnpm dev` starts a local SurrealDB (`:8666`), the sp00ky sync node (`:8667`),
the web app (http://localhost:5180) and the API Worker under `wrangler dev`
(`:8787`). In `.dev.vars`, use `SPKY_DB_URL=http://localhost:8666`,
`SPKY_DB_USER=root`, `SPKY_DB_PASS=root` (namespace `main` and database
`studycall` come from `wrangler.jsonc`) and `APP_ORIGIN=http://localhost:5180`.
An older `.dev.vars` with `SURREAL_URL`, `SURREAL_USER`, `SURREAL_PASS`,
`SURREAL_NS` and `SURREAL_DB` still works: the API reads `SURREAL_*` for
whatever `SPKY_DB_*` leaves unset. The local sync
node runs the `job` outbox against `wrangler dev` too, with the `JOB_TOKEN` from
`apps/api/wrangler.jsonc`.

ChatGPT reaches the MCP server over HTTPS only. To try the local stack from
ChatGPT, expose the local Worker and add
`https://YOUR-TUNNEL.trycloudflare.com/mcp` as a custom MCP server (see
[Using the MCP server](#using-the-mcp-server)):

```bash
cloudflared tunnel --url http://localhost:8787
```

If `spky dev` reports port 8787 in use, a `wrangler dev` from an earlier run
is still alive (stopping `spky dev` does not always stop it):
`lsof -ti tcp:8787 | xargs kill`.

### Materials and the bucket

A PDF upload is stored in the `materials` bucket and kept for as long as its
material exists: the app shows the original in its viewer, and course search
and the MCP tools cite its pages. A second upload to a
material that already has its PDF is refused (409); only a failed upload can be
replaced. Markdown and text files are imported straight into
`material.content` and never touch the bucket.

The bucket's `file:` backend needs a folder SurrealDB may write to. If local
uploads fail with a bucket error, check that the local SurrealDB allows
`/buckets/materials` (its bucket folder allowlist) and that the folder exists in
the container.

Existing deployments, once each:

1. Move bytes still stored in fields into the bucket: run
   `scripts/migrate-files-to-bucket.surql` as a system user until it reports
   nothing left.
2. Give the search chunks of PDFs indexed before page numbers existed their
   pages, so they can be cited (never reindex for this: that resets lecture
   progress). With the Worker's database credentials:

```bash
SURREAL_URL=... SURREAL_NS=... SURREAL_DB=... SURREAL_USER=... SURREAL_PASS=... pnpm backfill:pages
```

The script is idempotent: run it again until it reports no materials left.

### Schema changes

After a schema change: `pnpm generate`, then `pnpm exec spky migrate create migration-name --path dev`.
**Open the new migration and check for `'[REDACTED]'`** before applying it: the
CLI copies access-method JWT keys from a redacted `INFO`, and a key literally
set to `[REDACTED]` lets anyone sign valid session tokens. Delete those
`WITH JWT ... KEY '[REDACTED]' WITH ISSUER KEY '[REDACTED]'` clauses (SurrealDB
then generates a random key), as was done in the initial migration.

## Landing page

[`apps/landing`](../apps/landing) is the Astro marketing site, built as static HTML. It
requires Node 22.22 or newer. From the repository root:

```bash
cp apps/landing/.env.example apps/landing/.env.local
pnpm dev:landing
pnpm --filter @studycall/landing type-check
pnpm build:landing
```

Production links default to [the deployed app](https://talktobolo.stg.spky.cloud/).
Set `PUBLIC_APP_URL` in `apps/landing/.env.local` to override that destination.
Development falls back to `http://localhost:5180`. The static build is written
to `apps/landing/dist`.

### GitHub Pages

The landing page is published at <https://mono424.github.io/talk-to-bolo/>.
The [`Deploy landing page` workflow](../.github/workflows/deploy-landing.yml)
checks, builds, and deploys it when landing-page code or build configuration
changes on `main`. It can also be run manually from GitHub Actions.

GitHub Pages is configured to publish from **GitHub Actions**. The workflow
supplies `SITE_URL` and `BASE_PATH` from the Pages configuration, so logos,
photos, fonts, and scripts work under the `/talk-to-bolo/` project path.

The Get started and Log in links use `https://talktobolo.stg.spky.cloud/`.
Set the repository Actions variable `PUBLIC_APP_URL` to override that destination,
then rerun the workflow.

To build the same project-path layout locally:

```bash
SITE_URL=https://mono424.github.io BASE_PATH=/talk-to-bolo PUBLIC_APP_URL= pnpm build:landing
```

Landing page photo: [Sanket Mishra on Unsplash](https://unsplash.com/photos/a-girl-sitting-on-a-bench-using-a-laptop-vvE2GOnGudg).

## Sites

The Sites tab of a course holds static websites the student uploaded: a built
folder, a `.zip`, or one `.html` file, with its scripts, images and audio. A site
opens inside the course and has a link anyone can open.

The app creates the `site` row and posts the files one by one to
`/api/sites/:id/files`; `/api/sites/:id/finish` lists the pages, picks the one it
opens on (`index.html` if there is one) and marks it ready. Files are `site_file` rows
(server-only) with their bytes in the `materials` bucket in parts of at most 1 MiB,
at most 30 MB a file, 100 MB and 2000 files a site. Deleting a site or its course
takes the rows and bytes.

[`apps/api/src/sites.ts`](../apps/api/src/sites.ts) serves them at
`/site-files/<site key>/<path>`, public by the key's 128 random bits. Audio and
other non-HTML files answer `Range` requests with 206, which Safari needs to play them.

Sites come from students and share the app's origin, so **every response must carry
the CSP `sandbox` without `allow-same-origin`**. The browser then runs the page in an
opaque origin, away from the session in the app's storage and from the API. The
address of a file has to stay on this route for that reason: don't publish uploads
through the static assets. Opaque origins throw on `localStorage`, so HTML pages get
a small in-memory replacement as they are served, and what a site stores lasts only
until the page is closed or reloaded.

## Homework

**Answer** on any material opens a homework document for it (`homework`
and one `answer` row per part). `shared/homework.ts` splits the sheet into
parts: questions ("Question 2", "Problem 3", a numbered heading), lettered
parts ("Part (b)", "(c)") and roman sub-parts ("(ii)"), with points, page and
the introduction each part builds on. The app creates the rows itself.

The homework page (`apps/web/src/routes/Homework.tsx`) is one document of
answers with quiet margin labels per part. The exercise of the active part
shows beside it (on phones, pinned above the keyboard): for a PDF sheet a crop
of the page itself, located once from the text layer
(`apps/web/src/lib/sheet-regions.ts`) and saved as `answer.region`; otherwise
the part's text. Answers are Markdown with KaTeX math, saved as you type;
images (pasted, dropped onto a part, or picked with the Image button, which
offers the camera on phones) go to the `materials` bucket through
`/api/answers/:id/images` and are served at `/answer-images/:id`. The browser
re-encodes a type the API does not keep (HEIC in Safari) or a photo over
10 MB as a JPEG first (`apps/web/src/lib/images.ts`). Export PDF prints the answers in sheet
order. ChatGPT reads homework through the `get_homework` MCP tool.

## LaTeX material

A material of kind `tex` keeps its LaTeX source as written (upload a `.tex`
file, import one from a source, or `add_material` with `format: latex`). The
app shows it rendered and edits it in a LaTeX editor (CodeMirror, with a live
preview, `apps/web/src/components/TexEditor.tsx`). Everything else reads its
Markdown: `shared/latex.ts` converts sections, lists, theorem-like
environments, tables and verbatim, keeps every kind of math as LaTeX, and
turns `\newcommand`s into KaTeX macros. The search index, the tutor's
sections and the lecture place all use that conversion (`lectureMarkdown` in
`shared/lecture.ts`), on the server and in the browser alike. Math in every
material and recap renders with KaTeX (`apps/web/src/lib/markdown.ts`).

## Images in LaTeX documents

An image pasted, dropped or picked in a LaTeX document is stored like one in a
Markdown document (`/api/documents/:id/images`) and written into the source as
`\includegraphics[width=\linewidth]{answer-image:<key>}`, which `shared/latex.ts`
renders. TeX cannot open that path, so the LaTeX source of a document with images
downloads as a `.zip`: the `.tex` pointing at `images/<key>.png` or `.jpg`, the
images (GIF and WebP redrawn as PNG for pdfLaTeX), and `graphicx` loaded if the
source does not load it. LaTeX materials have no image upload.

Over MCP a document's images read as their addresses, `\includegraphics{<api>/answer-images/<key>}`
or `![](<api>/answer-images/<key>)`, and go back to `answer-image:<key>` on every
write (`imagesOutside` and `imagesInside` in `apps/api/src/answers.ts`).
`add_document_image` stores a new one from an https URL or base64 and answers
the line to write with `edit_document` or `write_document`.

## Document versions

Every change to a document's text is kept, however it was written: the
`document_versions` event in [`schema.surql`](../schema/src/schema.surql) runs
inside the write, so the web app, the MCP tools and anything else get versions
without doing anything. It tells the writer by `$access`: the student's session,
an AI client's, or the API's own database user.

`document_version` holds one synced row per version, for the list in the app.
`document_delta` (`@nosync`) holds what rebuilds it: hunks `{ at, len, old }`
running backwards from the current text, a full copy every 50 versions, and the
SHA-256 of the version's text, checked after rebuilding. The API rebuilds a
version in the database, whose offsets count code points (`rebuildVersion` in
`apps/api/src/documents.ts`). One write is one hunk, the stretch between what
stayed the same at both ends, so a write that changes two far-apart places in one
go stores the text between them too; the student's saves in one sitting are
separate writes, so they stay separate hunks.

The student's saves extend the latest version until they pause for 5 minutes, or
for 30 minutes in all. A change of 500 characters or more at once starts a new one,
so that what was there before stays one click away. Each write of an AI client is
a version of its own. A document from before versions gets what it held as version
1 with its next change. Going back (`restoreVersion`) writes the old text and format
as a new version; the paper list is not versioned.

The event must not fail, because that fails the write. Don't use `value::patch`
for this: text holding `@@` makes it fail or silently return the wrong text. Don't
use `RETURN` in the event either: where it ends in a nested block differs between
SurrealDB 3 (`spky dev`) and 4 (the deployment). `document-versions.test.ts` covers
both when run against each.

## Sources (bCourses and course websites)

A course can have sources (the `fetcher` table) that bring material in by
themselves: **Add, Add a source** on the course page, or `add_web_source` from
ChatGPT. Two kinds:

- **bCourses** (any Canvas): the student pastes a personal access token from
  bCourses (Account, Settings, New access token) and picks a course. The API
  lists the course's files, and its pages when asked. Courses that hide the
  Files tab from students are read through their Modules instead.
- **Course website**: one public https page. Every link on it is a candidate
  (http links are fetched over https), and its own text can be imported too.

Either kind can also import LaTeX sources (`.tex`, "Also import LaTeX
sources"), as `tex` material.

How a check runs (`apps/api/src/fetch`):

1. The `fetchers-tick` schedule (every 15 minutes, `sp00ky.yml`) starts the
   sources that are due; **Check now** in the app starts one at once (job
   `/jobs/fetchers/run`). Each check is one `FetchSource` Workflow run.
2. The connector lists what the source has as rows (`SourceRow` in
   `mapping.ts`), through the hockeymask `Connector` contract.
3. A [hockeymask](https://www.npmjs.com/package/@hockeymask/core) mapping turns
   the rows into `import` rows: which items to take, their title and kind.
   Each source runs a template built from its settings (PDFs, optionally
   pages, filtered by the words in "Only files whose name contains"), or its
   own mapping (the "Mapping (advanced)" editor, compiled before it is saved).
4. The `fetched_item` ledger (server-only) says what is new, what changed
   (Canvas `updated_at`, the page's text hash, a linked file's ETag or
   Last-Modified) and what the student deleted, which is never imported again.
5. New items become materials and are ingested like an upload; a changed one
   replaces its material's content and is re-indexed, which resets that
   lecture's progress and graph like any edit. At most 15 items per check; the
   rest follow ten minutes later.

Deleting a source keeps the material it imported. A token Canvas refuses stops
the source's scheduled checks until the student pastes a new one.

`FETCHER_KEY` seals the Canvas tokens (AES-GCM, bound to the source's id): any
long random string, in `.dev.vars` locally and in the vault for the deploy
(`pnpm exec spky env set FETCHER_KEY`). Changing it makes every saved token
unreadable, so students would have to paste theirs again. Requests to sources
go to public https hosts by name only, redirects included, and a token is only
ever sent to its own Canvas.

## Deploy

### Current staging app

The app is deployed at <https://talktobolo.stg.spky.cloud/>, by `spky deploy`
alone. The API is the `api` Worker backend in [`sp00ky.yml`](../sp00ky.yml):
the deploy bundles `apps/api/src/index.ts`, and the sync node runs it as a
Dynamic Worker with Workers AI and the `INGEST` Workflow.

Its environment is the vault keys listed under `apps.api.env` plus the plain
values there, and the database env sp00ky adds to every Worker backend:
`SPKY_DB_URL`, `SPKY_DB_NS` and `SPKY_DB_NAME`, and on the shared free database
the project's namespace login `owner` (`SPKY_DB_USER`, `SPKY_DB_PASS`,
`SPKY_DB_LEVEL=namespace`). On your own database sp00ky sets only the
coordinates: put the login in the vault as `SURREAL_USER` and `SURREAL_PASS`
(plus `SURREAL_AUTH_NS` / `SURREAL_AUTH_DB` for a namespace or database user)
and list them under `apps.api.env`. Set a missing key with
`pnpm exec spky env set <NAME>`, then deploy:

```bash
pnpm exec spky deploy --yes
```

Sources need `FETCHER_KEY` in the vault (see
[Sources](#sources-bcourses-and-course-websites)); set it once before the first
deploy that lists it.

Agent Memory is off on staging for now: the vault's `AGENT_MEMORY_URL` holds
only the host, and with an incomplete URL every ingestion fails. To turn it on,
set it to the Context's full URL ending in `/api/v1/<context id>` (and check
`AGENT_MEMORY_API_KEY`), add `- vault: [AGENT_MEMORY_URL, AGENT_MEMORY_API_KEY]`
under `apps.api.env`, and deploy. Then reindex existing material to build its
graph.

Background work runs as jobs: the web app writes a row to the `job` outbox and
the sync node POSTs its payload to the path in
[`apps/api/openapi.yml`](../apps/api/openapi.yml) (re-indexing, lecture
progress), retrying failures. Only the paths under `worker.expose` are public:
search, PDF upload and download, the OAuth consent calls, OAuth, MCP,
lecture images and uploaded sites. Frontend assets continue to use `/assets/`. Verify unauthenticated
`/mcp` returns an OAuth `401` challenge after deployment.

### 1. Database

Create a SurrealDB Cloud instance, a namespace `studycall` and a database
`main`, and a database user for sp00ky and the API (Editor or Owner, it defines
the server-only indexes). Put the endpoint and user in `sp00ky.yml`
(`surrealdb.endpoint`, `surrealdb.username`), then store the password:

```bash
pnpm exec spky env set DB_PASSWORD
```

### 2. API Worker (Cloudflare)

In `apps/api/wrangler.jsonc`, set `vars.APP_ORIGIN` to the production web app's
URL and `SPKY_DB_NS` / `SPKY_DB_NAME` to `studycall` / `main`. Then configure the
secrets (add `SPKY_DB_LEVEL` = `namespace` or `database` for a user defined
there) and deploy:

```bash
pnpm --filter @studycall/api exec wrangler secret put SPKY_DB_URL          # https://YOUR-INSTANCE.surreal.cloud
pnpm --filter @studycall/api exec wrangler secret put SPKY_DB_USER
pnpm --filter @studycall/api exec wrangler secret put SPKY_DB_PASS
pnpm --filter @studycall/api exec wrangler secret put AGENT_MEMORY_URL     # https://YOUR-CONTEXT-HOST/api/v1/YOUR-CONTEXT-ID
pnpm --filter @studycall/api exec wrangler secret put AGENT_MEMORY_API_KEY
pnpm --filter @studycall/api run deploy
```

### 3. Web app and sync node (Sp00ky Cloud, free plan on Cloudflare)

The app uses its own origin for public service links. Leave `VITE_API_URL` in
`apps/web/.env.production` empty for this deployment:

| Service | Public URL |
| --- | --- |
| App | <https://talktobolo.stg.spky.cloud/> |
| API | <https://talktobolo.stg.spky.cloud/api/> |
| MCP server | <https://talktobolo.stg.spky.cloud/mcp> |
| PDF images | `https://talktobolo.stg.spky.cloud/material-assets/:id` |

The hosting layer must route `/api/*`, `/mcp`, `/material-assets/*` and
`/health` to the API Worker before the SPA fallback. A static SPA deployment
alone returns HTML at these paths and does not provide the backend. If the
API is hosted separately, set `VITE_API_URL` to its actual origin.

Keep `/assets/*` on the frontend for Vite's JavaScript, styles and WebAssembly.
The API also retains its legacy `/assets/:id` image endpoint for existing links
on separately hosted API Workers.

Deploy the web app with:

```bash
pnpm exec spky login
pnpm exec spky project create --slug studycall --plan free
pnpm exec spky deploy
```

`pnpm exec spky deploy` uploads the sync node as a Cloudflare Worker, applies the schema
to SurrealDB (`pnpm exec spky push` re-applies it after schema changes), builds the SPA
with the database and sync node URLs baked in, and publishes it on Workers
Static Assets. `pnpm exec spky status` lists the URLs; put the web app's in the
Worker's `APP_ORIGIN` (step 2).

## Using the MCP server

**Connect ChatGPT** in the app's top bar opens a step by step guide (it also
opens by itself once per account and browser until a connection exists). It
copies the server address and opens ChatGPT Plugins. Choose **Add custom MCP
server**, fill in the name, description and address the guide shows, select
OAuth, then create and install the plugin. The guide turns to connected as soon
as the account approval on `/connect` creates the connection. ChatGPT
does not document a prefilled install link for arbitrary custom MCP servers.
Set it up once in a browser; the plugin then also works in the ChatGPT apps on
iOS and Android.

ChatGPT uses account approval on TalkToBolo's `/connect` page. No API key
is needed. OAuth uses S256 PKCE, short-lived access tokens and rotating
refresh tokens. Clients identify themselves with a Client ID Metadata Document
(an https `client_id`, which ChatGPT prefers; the server reads it and keeps it
for an hour) or register through dynamic client registration. Redirects back
to the client carry `iss` (RFC 9207). Revoke a connection under
**Connections**. Discovery is served under `/.well-known/`; `/oauth/register`,
`/oauth/authorize`, `/oauth/token` and `/oauth/revoke` belong to the API Worker.
Route these paths to the Worker alongside `/mcp` when using one app origin.

The client setup follows [OpenAI's custom MCP setup](https://developers.openai.com/plugins/deploy/connect-chatgpt)
and [OpenAI's authentication requirements](https://developers.openai.com/plugins/build/auth).

### Studying with ChatGPT voice

ChatGPT Voice uses connected plugins on web, iOS and Android, so a student can
study on the go: "What should I study today on TalkToBolo?", "Quiz me on what
I have covered", "Save this session to TalkToBolo". The study tools
(`apps/api/src/study.ts`) keep the lecture boundaries: reached sections are
exposure, not mastery, and tutoring stays within reached material plus the
sections explained in order.

1. `get_study_overview`: every course with lecture progress and recent
   sessions, or one course with each lecture's next section, the last recaps'
   review points and suggestions.
2. `prepare_study_session`: a spoken tutoring guide for `explain`, `quiz` or
   `qa` in the course language, with the next section (explain) or a spread of
   reached sections (quiz, qa), and with Agent Memory the course graph's facts
   up to the student's place. Its `student_model` comes from the concept map
   below.
3. `read_section` and `get_lecture_outline`: a lecture one section at a time,
   with headings, pages and links that open the PDF at the page.
4. `record_understanding` and `get_understanding`: the student's concept map
   (see below).
5. `save_study_session` (and `record_progress`): logs the session as a `call`
   with `assistant` set and a recap, shown under Sessions, and moves the lecture
   place forward. Progress never moves back from here. Observations recorded
   during the session join it.

#### What the student understood

`apps/api/src/understanding.ts` keeps the student's side of the course
knowledge graph in SurrealDB (`concept`, `requires` and `concept_evidence` in
`server-schema.surql`, not synced to the browser). The API calls no model, so
ChatGPT builds it while it tutors, through `record_understanding`:

- `concepts`: the small ideas of each section it reads (a definition, a rule,
  a step), where they are taught, what each `requires`, `part_of` a broader
  idea, and `prior` background knowledge the lecture assumes.
- `observations`: what the student showed about a concept after an answer:
  `explained`, `asked`, `unsure`, `wrong`, `partial`, `recalled`,
  `explained_back` or `applied`, with a note of what they said, and any
  misconception or gap (or ones they resolved).

ChatGPT never sets a level. The Worker derives it from the evidence
(`applyObservation`): `unseen`, `introduced` (only told), `struggling`,
`shaky`, `solid`, `mastered` (deep answers at least a day apart, no open
misconception), and when each is due for review. Names match across case,
accents and punctuation; a concept's id is derived from its course and name.

`get_understanding` (and `student_model` in `prepare_study_session`) reads it
back as three lists: `explain_first`, the weak foundations of the next
sections or of given topics, deepest first; `focus`, misconceptions, weak and
due concepts with why; `missing`, open gaps, concepts seen but never checked
and reached sections with no concepts mapped. `fetch` opens a `concept:` id
with its evidence. Deleting a lecture keeps its concepts and their evidence;
deleting the course removes them.

Read tools are annotated `readOnlyHint`, so with ChatGPT's plugin permission
**Allow read actions** Voice reads without asking; writes are approved on
screen. `search` and `fetch` follow ChatGPT's knowledge-source convention
across all courses, returning section ids and citable links; `fetch` also
opens `material:`, `course:`, `call:` and `concept:` ids.

Course tools: `list_courses`, `create_course`, `update_course`, `list_materials`,
`get_material`, `add_material`, `update_material`, `edit_material`, `add_pdf`
(https URL or base64), `delete_material`, `search_course`. Study tools:
`get_profile`, `get_study_overview`, `prepare_study_session`, `read_section`,
`get_lecture_outline`, `get_understanding`, `record_understanding`,
`record_progress`, `save_study_session`, `search`, `fetch`. Search hits and PDF materials carry page numbers and links into the
app's viewer, so assistants can cite pages; the text of a PDF is derived from
it and cannot be edited, only its title.

## Models and usage

Model settings live in [`apps/api/wrangler.jsonc`](../apps/api/wrangler.jsonc).

| Setting | Purpose | Checked-in value |
| --- | --- | --- |
| `EMBEDDING_MODEL` | Course search embeddings | `@cf/qwen/qwen3-embedding-0.6b` |

A PDF's text comes from its text layer (unpdf), so scanned pages without one
yield no text. Studying runs in the student's own ChatGPT; TalkToBolo calls no
language model itself. Document processing, embeddings, Agent Memory and
hosting use your connected provider accounts. Check those accounts for current
rates and limits.
