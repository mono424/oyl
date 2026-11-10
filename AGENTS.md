# Studycall repository guidance

See README.md for what the app is and how it is deployed.

## Writing style

Never use long dashes (em dashes) anywhere: UI text, comments, commits, docs.
Use a comma, period, colon or a plain hyphen.

## Schema

- `schema/src/schema.surql` is the sp00ky schema. sp00ky's parser predates
  SurrealDB 3 syntax for search indexes and refuses indexes on fields clients
  cannot read, so those live in `apps/api/src/server-schema.surql`, which the
  Worker applies idempotently. Indexes there use `IF NOT EXISTS`: redefining
  one rebuilds it.
- After editing the sp00ky schema: `pnpm generate`, then
  `spky migrate create <name> --path dev`, then check the new migration for
  `'[REDACTED]'` (see README) before applying it.
- Up to canary.345, `pnpm generate` also writes the job outbox's `errors[*]`
  field as an unquoted column key in `apps/web/src/schema.gen.ts`, which
  TypeScript rejects. Delete that line; newer CLIs leave it out.
- `-- @crdt` makes a field unwritable by record users after `CREATE`: its generated
  `VALUE` guard keeps the old value, and `UPDATE` still returns the row. Annotate a
  field only together with the web editor and API writing through its edit log.
- Document versions are written by the `document_versions` event, inside every
  write of `document.content`, so writers do nothing for them. An error in that
  event fails the save, so no `value::patch` and no `RETURN` in it; run
  `apps/api/test/document-versions.test.ts` against SurrealDB 3 and 4 after changing
  it (docs/setup.md, Document versions).
- Record ids minted by the app start with a letter and have no dashes
  (`newId` in `apps/web/src/lib/ids.ts`, `k<uuid hex>` in the Worker), so they
  never need ⟨⟩ quoting.

## Web app (`apps/web`)

Every read is a named live query in `src/lib/query.ts`, registered with
`useQuery`. No one-shot remote reads from the client. A live query must never
switch in place (another id while the hook stays mounted): sp00ky canary.333's
`useQuery` then writes into rows core caches for the previous query. Remount
per id instead (`perParam` in `App.tsx`, or a keyed `Show`). Calls to the API Worker
go through `src/lib/api.ts` with the user's SurrealDB token. After a local
write the API must read, `await whenSynced(db)` first.

## API Worker (`apps/api`)

- SurrealDB over HTTP JSON-RPC (`src/surreal.ts`). `Surreal.system` acts as the
  API's database user and must filter by owner explicitly; `Surreal.as(token)`
  runs with the user's own permissions. Record ids travel as strings and
  become records with `type::record($id)`; bytes travel as base64. Null vars
  are dropped (NONE), never sent as NULL.
- Never share a pending promise across requests (a Worker may not await I/O
  another request started): per-isolate caches hold values or flags only.
- The API calls no language model. Studying runs in ChatGPT through the MCP
  server (`src/mcp.ts`, `src/study.ts`); PDFs become Markdown from their text
  layer (`src/pdf.ts`).
