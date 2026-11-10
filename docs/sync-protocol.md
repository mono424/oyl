# Oyl sync protocol (v1)

Oyl is offline-first: a paper is a folder on the user's disk and the desktop
app works fully without a network. Sync copies papers to the user's Oyl Cloud
account so they are backed up, open on other machines and show up in the web
app. v1 runs over a small HTTP API on the Oyl API Worker. The data lives in
SurrealDB tables shaped so that a later version can let **sp00ky** sync them
to clients directly; the HTTP API is the transport for now, not the model.

The server side is `apps/api/src/sync.ts` (papers, revisions, blobs) and
`apps/api/src/sync-device.ts` (sign-in), tested in `apps/api/test/sync.test.ts`.

## Model

- **Paper**: one synced paper folder. `paper_sync` record (the table is not
  called `paper`: that is a web document's bibliography entry): `id`, `owner`,
  `name`, `engine` (`typst` | `latex`), `main` (relative path of the entry
  file), `head` (the latest revision, NONE before the first commit),
  `updated_by` (the head's author), `created_at`, `updated_at`, `deleted_at`
  (set while soft-deleted). The API answers these as `created`, `updated` and
  `deleted` (a boolean).
- **Blob**: a file's bytes, addressed by `sha256` (lowercase hex). Stored
  once per owner. Text and binary alike. Max 50 MiB (52,428,800 bytes) per blob.
- **Revision**: an immutable snapshot of a paper: `paper_revision` record
  with `id`, `paper`, `n` (1, 2, 3, ... per paper), `parent` (previous
  revision or none), `files` (map of relative path → `{ sha256, size }`),
  `file_count`, `size` (bytes of all files), `author` (`device:<name>` |
  `web` | `agent`), `message`, `created_at`. A revision lists every tracked
  file of the paper, so any revision can be materialised alone. In the
  database `files` is a JSON string with the paths sorted, marked `@opaque` so
  the sync node does not keep it in memory; the API answers it as an object.
- What is synced is what the desktop history tracks plus the assets a paper
  needs (figures, PDFs in `sources/`, `.bib`, styles). Never `.oyl/`, `.git/`,
  build output, or files over 50 MiB.

### Paths

A path in `files` (and `main`) is relative to the paper folder, uses `/`,
is Unicode NFC (normalize macOS file names before sending), at most 512
characters, with no empty, `.` or `..` segments, no backslash or control
characters, and does not start with `.oyl/` or `.git/`. Two paths of one
revision may not differ only in case (a macOS folder could not hold both).
The server refuses anything else with 400 and compares paths byte for byte.

### Where the bytes live

Blobs are files in the `materials` SurrealDB bucket
(`schema/buckets/materials.surql`), where uploaded PDFs, page images and
uploaded sites already live, so there is one storage system to deploy and back
up. Each blob has a server-only `sync_blob` row (owner, sha256, size, parts)
whose id is derived from the owner and the hash, so the same bytes are stored
once per owner and one owner's hash never reveals another's blob. The bytes are
split into parts of 1 MiB (SurrealDB's request size) at
`/papers/<blob key>_<n>`. An upload streams through the Worker: parts are
written to a staging path while the SHA-256 is computed, and only when it
matches the name are they moved into place and the row written. A wrong or
oversized upload leaves nothing behind. Blobs are never changed and, in v1,
never deleted (revisions are kept forever).

## Auth

`Authorization: Bearer <token>`: the same account tokens the web app uses
(`verifyUserToken`), or a sync key created for a device. The desktop signs in
with a device flow (in the style of RFC 8628):

1. `POST /api/sync/device { device_name }` → `{ device_code, user_code,
   verify_url, interval, expires_in }`. `device_name` is what the user will
   see ("MacBook Pro", at most 60 characters, default "Mac"). `user_code` is
   eight consonants shown as `BCDF-GHJK`; `interval` is 5 (seconds) and
   `expires_in` 600.
2. The app shows the code and opens `verify_url` (the web app's
   `/connect?code=<user_code>`), where the signed-in user checks the code and
   approves "Oyl on <device name>". The page also takes the code typed in
   (`/connect` alone).
3. The app polls `POST /api/sync/device/token { device_code }` every
   `interval` seconds:
   - `202 { status: "pending", interval }` (with `Retry-After`) while the user
     has not answered,
   - `429 { code: "slow_down", interval }` when it polls faster than
     `interval`; wait and poll again,
   - `403 { code: "access_denied" }` once the user declined,
   - `410 { code: "expired_token" }` for an unknown, expired or already used
     code: start again at step 1,
   - `200 { token, account: { id, name, email } }` once approved, exactly
     once. Approving gives the device another 10 minutes to collect it.

   The token is a sync key: `oyl_sync_` followed by 43 base64url characters.
   Store it in the macOS Keychain. It does not expire.

The key opens `/api/sync/*` and nothing else: not the MCP server, not the
other API routes, and it cannot sign in to SurrealDB. On the server it is an
`api_key` row "Oyl on <device name>" (listed under Connections in Settings,
where the user revokes it) plus a server-only `sync_device` row with the key's
SHA-256. Revoking takes effect on the next request: every endpoint then answers
`401`, and the app should drop the key and offer to sign in again.

## Endpoints (all JSON unless noted, all under `/api/sync`)

Base URL: the deployed app's origin, `https://talktobolo.stg.spky.cloud` today
(`http://localhost:8787` for the local stack, `pnpm dev`). Make it a setting.

Errors are `{ error: "<message for people>" }` plus the fields noted. Record
ids are strings like `paper_sync:k3f...` and `paper_revision:k9a...`; in a
path, percent-encode them (`paper_sync%3Ak3f...`) or send only the key after
the colon.

| Method & path | Does |
| --- | --- |
| `GET /papers` | List the caller's papers, removed ones included: `[{ id, name, engine, main, head, updated_by, created, updated, deleted }]`, latest change first |
| `POST /papers` | Create `{ name, engine, main, id? }` → `201` with the paper (as in the list). `id` is optional: one the client makes up (`paper_sync:k` + 32 lowercase hex digits) makes a retry safe, answering `200` with the paper already made (`409` if another account has it) |
| `PATCH /papers/:id` | Any of `{ name, main, engine, deleted }` → the paper. `deleted: true` soft-deletes, `false` restores |
| `GET /papers/:id/head` | `{ revision, n, parent, files, author, message, created, file_count, size }` (404 when empty) |
| `GET /papers/:id/revisions?limit=&before=` | Recent revisions, newest first (no file maps): `[{ revision, n, parent, author, message, created, file_count, size }]`. `limit` 1 to 200 (default 50), `before=<n>` pages back |
| `GET /papers/:id/revisions/:rev` | One revision with its file map, as `head` |
| `POST /blobs/missing` | `{ hashes: [...] }` (at most 1000) → `{ missing: [...] }` - which the server lacks, in the order asked |
| `PUT /blobs/:sha256` | Upload bytes (`application/octet-stream`); the server checks the hash. `201 { sha256, size }`, or `200` when it had them already (the body is not read). `400 { sha256 }` with the actual hash on a mismatch, `413` over 50 MiB |
| `GET /blobs/:sha256` | Download bytes (`ETag` = `"<sha256>"`, `Cache-Control: private, max-age=31536000, immutable`, `If-None-Match` → `304`) |
| `POST /papers/:id/revisions` | Commit `{ parent, files, message?, author? }` → `201 { revision, n }`; `409 { head }` when `parent` is not the current head (compare-and-set) |

All blobs a revision references must exist before it is committed (`422
{ missing: [...] }` lists the missing ones), and each file's `size` must be
its blob's. Commits are atomic: either the revision becomes head or nothing
changes.

Commit details:

- `parent` is the revision the commit was made on, `null` for the first.
- `files` is the whole file map (at most 5000 files). `message` is optional
  (at most 2000 characters). `author` defaults to `device:<the key's device
  name>`, or `web` for the web app's session.
- `409 { head }`: `head` is the current head as `GET /head` returns it,
  file map included (`null` if the paper has none), so a pull can start from
  it without another request.
- `200 { revision, n, unchanged: true }`: nothing new was made, because
  `files` equal the head's, or because the head is this very commit (made on
  `parent` with these files): a retry after a lost response is safe.
- `410` when the paper is soft-deleted: restore it (`PATCH deleted: false`)
  or stop syncing it.

Limits: blob 50 MiB, 1000 hashes per `blobs/missing`, 5000 files and a 4 MB
JSON body per commit, 200 revisions per page, names 200 characters, paths 512.
Every request is cheap to retry: blob uploads are idempotent by hash, commits
by the rules above. Back off on `429` and `5xx` (honour `Retry-After`).

The web app's `/connect` page also uses `POST /device/request { user_code }`
and `POST /device/approve { user_code, allow }`, with the user's session only.

## Desktop algorithm

State per paper in `<paper>/.oyl/sync.json`: `{ paper_id, synced_revision,
synced_files }`, the last revision both sides agree on (the merge base).

- **Push** (local changed since `synced_files`, debounced ~5 s after the last
  save, and on demand): hash changed files → `blobs/missing` → upload missing
  → commit with `parent = synced_revision`. `201` (or `200 unchanged`) →
  record it. `409` → pull first (from the `head` in the answer), then push
  again.
- **Pull** (on open, every 30 s while online, after a 409): read head; if it
  differs from `synced_revision`, download missing blobs into a private
  working copy and treat the remote revision like an agent branch: files the
  user did not change since `synced_files` are applied directly; files both
  sides changed are merged three-way (base = `synced_files`) with the same
  engine as agent branches; real conflicts go to the same review UI. Then
  record the remote revision as synced (and push the merge result if any).
- **Offline**: everything keeps working; a status dot says "Offline: will
  sync". Retries back off (5 s → 5 min). A `401` means signed out: stop and
  offer to sign in again.
- **First sync** of an existing local paper creates the cloud paper and
  pushes; opening a cloud paper on a new machine downloads its head into a
  chosen folder.

## Web app

The web app lists synced papers under **Papers** in the sidebar (name, engine,
last change and the device it came from), opens one read-only (the entry file
or any text file of a revision in the source view, with the revisions and
files beside it), and approves devices on `/connect`. It reads `paper_sync`
and `paper_revision` live through sp00ky and the bytes through
`GET /blobs/:sha256`. In v1.1 it edits the entry file through a commit
(author `web`), which reaches the desktop as a pull.

## Path to sp00ky

`paper_sync` and `paper_revision` are ordinary SurrealDB tables with
per-owner permissions (record users read their own rows and write none), so a
later version can subscribe to them through sp00ky instead of polling; blobs
stay content-addressed. Clients keep the same merge logic; only the transport
changes.
