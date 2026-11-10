-- ##################################################################
-- API OUTBOX TABLE
--
-- Background work for the API backend (sp00ky.yml `apps.api`, paths in
-- apps/api/openapi.yml): the web app creates a row, sp00ky's job runner POSTs
-- its payload to the backend and records the outcome here. Shape from
-- `spky api add`; only the permissions and `assigned_to` are talktobolo's.
-- ##################################################################

-- A student may create a job only about their own material, call or source, and only
-- with that same record as `payload.id`: the backend never sees who created a
-- job, so it treats the owner of `payload.id` as the caller. `payload` is an
-- object (the web app writes rows itself, not through `db.run`, which would
-- store it as a JSON string the rule cannot read). Nobody but the platform
-- (as root) changes a job after creating it.
DEFINE TABLE job SCHEMAFULL
PERMISSIONS
  FOR select, delete WHERE $access = "account" AND assigned_to.owner = $auth.id
  FOR create WHERE $access = "account" AND assigned_to.owner = $auth.id
    AND <string> payload.id = <string> assigned_to
  FOR update WHERE false;

-- The material, call or source this job belongs to, used by the permissions above.
-- OPTIONAL on purpose: a server-side schedule (`schedules:` in sp00ky.yml)
-- spawns system-initiated jobs that belong to no single record, so the
-- scheduler creates rows without it. Such rows are invisible to clients.
DEFINE FIELD assigned_to ON TABLE job TYPE option<record<material | call | fetcher>>
PERMISSIONS
  FOR create, select WHERE true
  FOR update WHERE false;

DEFINE FIELD path ON TABLE job TYPE string
PERMISSIONS
  FOR create, select WHERE true
  FOR update WHERE false;

DEFINE FIELD payload ON TABLE job TYPE any
PERMISSIONS
  FOR create, select WHERE true
  FOR update WHERE false;

DEFINE FIELD retries ON TABLE job TYPE int DEFAULT ALWAYS 0
PERMISSIONS
  FOR create, select WHERE true
  FOR update WHERE false;

DEFINE FIELD max_retries ON TABLE job TYPE int DEFAULT ALWAYS 3;

DEFINE FIELD retry_strategy ON TABLE job TYPE string DEFAULT ALWAYS "linear"
ASSERT $value IN ["linear", "exponential"]
PERMISSIONS
  FOR create, select WHERE true
  FOR update WHERE false;

DEFINE FIELD status ON TABLE job TYPE string DEFAULT ALWAYS "pending"
ASSERT $value IN ["pending", "processing", "success", "failed"]
PERMISSIONS
  FOR create, select WHERE true
  FOR update WHERE false;

DEFINE FIELD errors ON TABLE job TYPE array<object> DEFAULT ALWAYS []
PERMISSIONS
  FOR create WHERE true
  FOR select, update WHERE false;

-- The element must be FLEXIBLE or a SCHEMAFULL table rejects the runner's
-- `{ code, reason }` entries as unknown fields (`errors[0].code`).
-- OVERWRITE because SurrealDB 3.1 defines the element (`errors.*`, without
-- FLEXIBLE) by itself the moment `errors` is declared `array<object>`.
DEFINE FIELD OVERWRITE errors[*] ON TABLE job TYPE object FLEXIBLE;

-- Set on create, and thereafter only when a writer sets it explicitly (every
-- platform write does). The recovery sweeps' staleness clocks read it, so it
-- must NOT be `VALUE time::now()`.
DEFINE FIELD updated_at ON TABLE job TYPE datetime
DEFAULT ALWAYS time::now()
PERMISSIONS
  FOR create, select WHERE true
  FOR update WHERE false;

-- `DEFAULT`, deliberately NOT `VALUE`: a `VALUE` field is recomputed on every
-- UPDATE, which would slide the `created_at + delay` due clause forward.
DEFINE FIELD created_at ON TABLE job TYPE datetime
DEFAULT time::now()
PERMISSIONS
  FOR create, select WHERE true
  FOR update WHERE false;

-- Platform claim marker: the SSP stamps the owning instance when it picks up
-- or recovers a job. Written by the platform as root, never by clients.
DEFINE FIELD assignee ON TABLE job TYPE option<string>
PERMISSIONS
  FOR select WHERE true
  FOR create, update WHERE false;

-- Lease expiry for the attempt holding this row, and the fencing token that
-- attempt presents to write its outcome. Reclaim keys off `lease_until`;
-- `lease_epoch` is bumped on every claim and reclaim so a reclaimed attempt
-- cannot overwrite the new one. Platform-written (root).
DEFINE FIELD lease_until ON TABLE job TYPE option<datetime>
PERMISSIONS
  FOR select WHERE true
  FOR create, update WHERE false;

DEFINE FIELD lease_epoch ON TABLE job TYPE option<int>
PERMISSIONS
  FOR select WHERE true
  FOR create, update WHERE false;

-- The backend's response body on success, stored by the job runner.
DEFINE FIELD result ON TABLE job TYPE any
PERMISSIONS
  FOR select WHERE true
  FOR create, update WHERE false;

-- Per-job HTTP timeout in SECONDS, overriding the backend default. It also
-- sizes the reclaim lease, so never write milliseconds here.
DEFINE FIELD timeout ON TABLE job TYPE option<int>
PERMISSIONS
  FOR create, select WHERE true
  FOR update WHERE false;

-- One-shot delay in ms: the job becomes due at `created_at + delay`.
DEFINE FIELD delay ON TABLE job TYPE option<int>
PERMISSIONS
  FOR create, select WHERE true
  FOR update WHERE false;

-- ── Indexes ────────────────────────────────────────────────────────────────
--
-- `(status, created_at)` serves the dispatcher's drain, the retention prune and
-- the queue counts; `updated_at` the prune's ageing and the Jobs page.
-- `assigned_to` serves the lecture page's live query of its latest jobs.
-- `IF NOT EXISTS`, never `OVERWRITE`: that would rebuild them on every deploy.
DEFINE INDEX IF NOT EXISTS idx_job_dispatch ON TABLE job COLUMNS status, created_at;
DEFINE INDEX IF NOT EXISTS idx_job_activity ON TABLE job COLUMNS updated_at;
DEFINE INDEX IF NOT EXISTS idx_job_assigned ON TABLE job COLUMNS assigned_to;
