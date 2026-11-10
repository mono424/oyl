-- Migration: job_outbox
-- Created: 2026-10-22T06:20:23.803546+00:00

-- Added
DEFINE TABLE OVERWRITE job TYPE NORMAL SCHEMAFULL PERMISSIONS FOR select, delete WHERE $access = 'account' AND assigned_to.owner = $auth.id, FOR create WHERE $access = 'account' AND assigned_to.owner = $auth.id AND <string> payload.id = <string> assigned_to, FOR update WHERE false;
DEFINE FIELD OVERWRITE assigned_to ON job TYPE none | record<material | call> PERMISSIONS FOR select, create WHERE true, FOR update WHERE false;
DEFINE FIELD OVERWRITE assignee ON job TYPE none | string PERMISSIONS FOR select WHERE true, FOR create, update WHERE false;
DEFINE FIELD OVERWRITE created_at ON job TYPE datetime DEFAULT time::now() PERMISSIONS FOR select, create WHERE true, FOR update WHERE false;
DEFINE FIELD OVERWRITE delay ON job TYPE none | int PERMISSIONS FOR select, create WHERE true, FOR update WHERE false;
DEFINE FIELD OVERWRITE errors ON job TYPE array<object> DEFAULT ALWAYS [] PERMISSIONS FOR select, update WHERE false, FOR create WHERE true;
DEFINE FIELD OVERWRITE lease_epoch ON job TYPE none | int PERMISSIONS FOR select WHERE true, FOR create, update WHERE false;
DEFINE FIELD OVERWRITE lease_until ON job TYPE none | datetime PERMISSIONS FOR select WHERE true, FOR create, update WHERE false;
DEFINE FIELD OVERWRITE max_retries ON job TYPE int DEFAULT ALWAYS 3 PERMISSIONS FULL;
DEFINE FIELD OVERWRITE path ON job TYPE string PERMISSIONS FOR select, create WHERE true, FOR update WHERE false;
DEFINE FIELD OVERWRITE payload ON job TYPE any PERMISSIONS FOR select, create WHERE true, FOR update WHERE false;
DEFINE FIELD OVERWRITE result ON job TYPE any PERMISSIONS FOR select WHERE true, FOR create, update WHERE false;
DEFINE FIELD OVERWRITE retries ON job TYPE int DEFAULT ALWAYS 0 PERMISSIONS FOR select, create WHERE true, FOR update WHERE false;
DEFINE FIELD OVERWRITE retry_strategy ON job TYPE string DEFAULT ALWAYS 'linear' ASSERT $value INSIDE ['linear', 'exponential'] PERMISSIONS FOR select, create WHERE true, FOR update WHERE false;
DEFINE FIELD OVERWRITE status ON job TYPE string DEFAULT ALWAYS 'pending' ASSERT $value INSIDE ['pending', 'processing', 'success', 'failed'] PERMISSIONS FOR select, create WHERE true, FOR update WHERE false;
DEFINE FIELD OVERWRITE timeout ON job TYPE none | int PERMISSIONS FOR select, create WHERE true, FOR update WHERE false;
DEFINE FIELD OVERWRITE updated_at ON job TYPE datetime DEFAULT ALWAYS time::now() PERMISSIONS FOR select, create WHERE true, FOR update WHERE false;
DEFINE INDEX OVERWRITE idx_job_activity ON job FIELDS updated_at;
DEFINE INDEX OVERWRITE idx_job_assigned ON job FIELDS assigned_to;
DEFINE INDEX OVERWRITE idx_job_dispatch ON job FIELDS status, created_at;
DEFINE INDEX OVERWRITE idx_job_retention ON job FIELDS status, updated_at;
-- Added by hand: the diff leaves out the element SurrealDB defines by itself
-- (`errors.*`, without FLEXIBLE), and the runner's `{ code, reason }` entries
-- need it FLEXIBLE (see schema/src/outbox/api.surql).
DEFINE FIELD OVERWRITE errors[*] ON job TYPE object FLEXIBLE;
