-- ##################################################################

-- SERVER-ONLY TABLES, owned by the API Worker
--
-- Not part of the sp00ky schema (schema/src/schema.surql): nothing here is
-- synced to the client, and the vector/full-text index syntax is SurrealDB 3,
-- which sp00ky's schema parser does not read yet. The Worker applies this file
-- once per isolate (see ensureServerSchema in src/service.ts), so it must stay
-- idempotent: indexes and the analyzer use IF NOT EXISTS, because redefining
-- an index rebuilds it. To change one, REMOVE it by hand first.
--
-- The `material_deleted` / `course_deleted` events in the main schema clean
-- these tables up.
-- ##################################################################


-- Search sections of a material. Written by the API Worker only. Readable by
-- the owner so the API can run the search with the user's own session.
DEFINE TABLE OVERWRITE chunk SCHEMAFULL
  PERMISSIONS
    FOR select WHERE owner = $auth.id
    FOR create, update, delete WHERE false;

DEFINE FIELD OVERWRITE owner ON TABLE chunk TYPE record<user>;
DEFINE FIELD OVERWRITE course ON TABLE chunk TYPE record<course>;
DEFINE FIELD OVERWRITE material ON TABLE chunk TYPE record<material>;
DEFINE FIELD OVERWRITE rev ON TABLE chunk TYPE int;
DEFINE FIELD OVERWRITE position ON TABLE chunk TYPE int;
-- The heading path the chunk sits under ("Week 3 > Gradient descent").
DEFINE FIELD OVERWRITE heading ON TABLE chunk TYPE option<string>;
DEFINE FIELD OVERWRITE text ON TABLE chunk TYPE string;
-- First and last PDF page the chunk was taken from (shared/lecture.ts
-- chunkPages). NONE for typed and Markdown material.
DEFINE FIELD OVERWRITE page ON TABLE chunk TYPE option<int>;
DEFINE FIELD OVERWRITE page_end ON TABLE chunk TYPE option<int>;
DEFINE FIELD OVERWRITE embedding ON TABLE chunk TYPE array<float>;
DEFINE FIELD OVERWRITE created_at ON TABLE chunk TYPE datetime DEFAULT time::now();

DEFINE INDEX IF NOT EXISTS chunk_course ON TABLE chunk FIELDS course;
DEFINE INDEX IF NOT EXISTS chunk_material ON TABLE chunk FIELDS material;

-- Semantic search. 1024 = @cf/qwen/qwen3-embedding-0.6b (multilingual) on
-- Workers AI; change both together.
DEFINE INDEX IF NOT EXISTS chunk_embedding ON TABLE chunk FIELDS embedding
  HNSW DIMENSION 1024 DIST COSINE TYPE F32;

-- Keyword search, combined with the vector hits (hybrid search). No stemmer:
-- lecture material comes in any language.
DEFINE ANALYZER IF NOT EXISTS lecture_text TOKENIZERS blank, class, punct FILTERS lowercase, ascii;
DEFINE INDEX IF NOT EXISTS chunk_text ON TABLE chunk FIELDS text FULLTEXT ANALYZER lecture_text BM25;

-- Images extracted from PDFs. The PNG is the file /assets/<id> of the
-- `materials` bucket (schema/buckets/materials.surql), never a field: a synced
-- table holding the bytes made the SSP keep every image in memory. The API
-- serves it at /material-assets/<id>; the id is random and unguessable.
DEFINE TABLE OVERWRITE asset SCHEMAFULL
  -- Server only: the web app reads it through this API, never through sync.
  COMMENT 'sp00ky:nosync'
  PERMISSIONS
    FOR select WHERE owner = $auth.id
    FOR create, update, delete WHERE false;

DEFINE FIELD OVERWRITE owner ON TABLE asset TYPE record<user>;
DEFINE FIELD OVERWRITE material ON TABLE asset TYPE record<material>;
DEFINE FIELD OVERWRITE page ON TABLE asset TYPE option<int>;
DEFINE FIELD OVERWRITE mime ON TABLE asset TYPE string;
DEFINE FIELD OVERWRITE width ON TABLE asset TYPE option<int>;
DEFINE FIELD OVERWRITE height ON TABLE asset TYPE option<int>;
-- The bytes moved to the `materials` bucket (scripts/migrate-files-to-bucket.surql).
REMOVE FIELD IF EXISTS data ON TABLE asset;
-- SHA-256 of the pixels: a logo repeated on every slide is dropped.
DEFINE FIELD OVERWRITE hash ON TABLE asset TYPE option<string>;
DEFINE FIELD OVERWRITE created_at ON TABLE asset TYPE datetime DEFAULT time::now();

DEFINE INDEX IF NOT EXISTS asset_material ON TABLE asset FIELDS material;

-- Every way a row goes (a re-run, the repeated-image pass, a deleted material)
-- takes its file with it.
DEFINE EVENT OVERWRITE asset_file ON TABLE asset WHEN $event = 'DELETE'
  THEN type::file('materials', '/assets/' + <string>record::id($before.id)).delete();

-- The uploaded PDF, split into parts of at most 1 MiB (SurrealDB's HTTP body
-- limit). The original, kept for the viewer and citations until the material
-- is deleted.
DEFINE TABLE OVERWRITE source_part SCHEMAFULL
  -- Server only: the web app reads it through this API, never through sync.
  COMMENT 'sp00ky:nosync'
  PERMISSIONS
    FOR select WHERE owner = $auth.id
    FOR create, update, delete WHERE false;

DEFINE FIELD OVERWRITE owner ON TABLE source_part TYPE record<user>;
DEFINE FIELD OVERWRITE material ON TABLE source_part TYPE record<material>;
DEFINE FIELD OVERWRITE idx ON TABLE source_part TYPE int;
-- The bytes moved to the `materials` bucket (scripts/migrate-files-to-bucket.surql).
REMOVE FIELD IF EXISTS data ON TABLE source_part;

DEFINE INDEX IF NOT EXISTS source_part_material ON TABLE source_part FIELDS material, idx UNIQUE;

-- The bytes are the file /sources/<id> of the `materials` bucket.
DEFINE EVENT OVERWRITE source_part_file ON TABLE source_part WHEN $event = 'DELETE'
  THEN type::file('materials', '/sources/' + <string>record::id($before.id)).delete();

-- The `agent` access (main schema) looks API keys up by hash on every MCP
-- sign-in.
DEFINE INDEX IF NOT EXISTS api_key_hash ON TABLE api_key FIELDS key_hash UNIQUE;

-- Public OAuth clients and short-lived authorization state. None of these
-- credentials or hashes are synced to the browser. A client is registered
-- here (DCR, `bolo_` ids) or identified by the https URL of its metadata
-- document (CIMD, `oauth_client:m<hash of the URL>`, refreshed on authorize).
DEFINE TABLE OVERWRITE oauth_client SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD OVERWRITE client_id ON TABLE oauth_client TYPE string;
DEFINE FIELD OVERWRITE client_name ON TABLE oauth_client TYPE string;
DEFINE FIELD OVERWRITE redirect_uris ON TABLE oauth_client TYPE array<string>;
-- When a metadata document client's document was last read.
DEFINE FIELD OVERWRITE fetched_at ON TABLE oauth_client TYPE option<datetime>;
DEFINE FIELD OVERWRITE created_at ON TABLE oauth_client TYPE datetime DEFAULT time::now();
DEFINE INDEX IF NOT EXISTS oauth_client_id ON TABLE oauth_client FIELDS client_id UNIQUE;

DEFINE TABLE OVERWRITE oauth_request SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD OVERWRITE client ON TABLE oauth_request TYPE record<oauth_client>;
DEFINE FIELD OVERWRITE redirect_uri ON TABLE oauth_request TYPE string;
DEFINE FIELD OVERWRITE state ON TABLE oauth_request TYPE option<string>;
-- The issuer the client discovered; the redirect back carries it as `iss`.
DEFINE FIELD OVERWRITE issuer ON TABLE oauth_request TYPE option<string>;
DEFINE FIELD OVERWRITE resource ON TABLE oauth_request TYPE string;
DEFINE FIELD OVERWRITE scope ON TABLE oauth_request TYPE string;
DEFINE FIELD OVERWRITE challenge ON TABLE oauth_request TYPE string;
DEFINE FIELD OVERWRITE expires_at ON TABLE oauth_request TYPE datetime;
DEFINE FIELD OVERWRITE consumed_at ON TABLE oauth_request TYPE option<datetime>;

-- Connections are listed beside personal keys in Settings, giving the
-- existing disconnect action immediate effect on this grant and its tokens.
DEFINE TABLE OVERWRITE oauth_grant SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD OVERWRITE owner ON TABLE oauth_grant TYPE record<user>;
DEFINE FIELD OVERWRITE client ON TABLE oauth_grant TYPE record<oauth_client>;
DEFINE FIELD OVERWRITE connection ON TABLE oauth_grant TYPE record<api_key>;
DEFINE FIELD OVERWRITE resource ON TABLE oauth_grant TYPE string;
DEFINE FIELD OVERWRITE scope ON TABLE oauth_grant TYPE string;
DEFINE FIELD OVERWRITE expires_at ON TABLE oauth_grant TYPE datetime;
DEFINE FIELD OVERWRITE revoked_at ON TABLE oauth_grant TYPE option<datetime>;
DEFINE FIELD OVERWRITE created_at ON TABLE oauth_grant TYPE datetime DEFAULT time::now();

DEFINE TABLE OVERWRITE oauth_code SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD OVERWRITE grant ON TABLE oauth_code TYPE record<oauth_grant>;
DEFINE FIELD OVERWRITE code_hash ON TABLE oauth_code TYPE string;
DEFINE FIELD OVERWRITE redirect_uri ON TABLE oauth_code TYPE string;
DEFINE FIELD OVERWRITE challenge ON TABLE oauth_code TYPE string;
DEFINE FIELD OVERWRITE expires_at ON TABLE oauth_code TYPE datetime;
DEFINE FIELD OVERWRITE consumed_at ON TABLE oauth_code TYPE option<datetime>;
DEFINE INDEX IF NOT EXISTS oauth_code_hash ON TABLE oauth_code FIELDS code_hash UNIQUE;

DEFINE TABLE OVERWRITE oauth_refresh SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD OVERWRITE grant ON TABLE oauth_refresh TYPE record<oauth_grant>;
DEFINE FIELD OVERWRITE token_hash ON TABLE oauth_refresh TYPE string;
DEFINE FIELD OVERWRITE expires_at ON TABLE oauth_refresh TYPE datetime;
DEFINE FIELD OVERWRITE consumed_at ON TABLE oauth_refresh TYPE option<datetime>;
DEFINE INDEX IF NOT EXISTS oauth_refresh_hash ON TABLE oauth_refresh FIELDS token_hash UNIQUE;

DEFINE TABLE OVERWRITE oauth_access SCHEMAFULL PERMISSIONS NONE;
DEFINE FIELD OVERWRITE grant ON TABLE oauth_access TYPE record<oauth_grant>;
DEFINE FIELD OVERWRITE owner ON TABLE oauth_access TYPE record<user>;
DEFINE FIELD OVERWRITE token_hash ON TABLE oauth_access TYPE string;
DEFINE FIELD OVERWRITE expires_at ON TABLE oauth_access TYPE datetime;
DEFINE FIELD OVERWRITE revoked_at ON TABLE oauth_access TYPE option<datetime>;
DEFINE INDEX IF NOT EXISTS oauth_access_hash ON TABLE oauth_access FIELDS token_hash UNIQUE;

-- Opaque OAuth access tokens become ordinary owner-scoped sessions, keeping
-- every MCP tool behind the same table permissions as the signed-in app.
-- Unlike fields, access definitions generate a signing key. Keep it stable
-- across Worker isolates; changes to this definition require a migration.
DEFINE ACCESS IF NOT EXISTS oauth_agent ON DATABASE TYPE RECORD
  SIGNIN (
    SELECT * FROM user WHERE id = (
      SELECT VALUE owner FROM oauth_access WHERE token_hash = crypto::sha256($oauth_key)
        AND revoked_at = NONE AND expires_at > time::now()
        AND grant.revoked_at = NONE AND grant.expires_at > time::now()
        AND grant.connection.id != NONE AND grant.connection.revoked_at = NONE AND grant.connection.owner = owner AND grant.owner = owner LIMIT 1
    )[0]
  )
  DURATION FOR TOKEN 1h, FOR SESSION 1h
;

-- ##################################################################
-- STUDENT UNDERSTANDING (understanding.ts)
--
-- The student's side of the course knowledge graph: one `concept` node per
-- small idea of a course (a definition, a rule, a step, a relationship),
-- `requires` edges between them, and the evidence of what the student showed
-- about each one. ChatGPT maps the concepts while it tutors and records what
-- the student says through the MCP server; the Worker turns that evidence
-- into a level per concept. Courses belong to one student, so a concept and
-- its level are that student's.
-- ##################################################################

DEFINE TABLE OVERWRITE concept SCHEMAFULL
  COMMENT 'sp00ky:nosync'
  PERMISSIONS
    FOR select WHERE owner = $auth.id
    FOR create, update, delete WHERE false;

DEFINE FIELD OVERWRITE owner ON TABLE concept TYPE record<user>;
DEFINE FIELD OVERWRITE course ON TABLE concept TYPE record<course>;
-- The name folded for matching (lowercase, no accents or punctuation).
DEFINE FIELD OVERWRITE key ON TABLE concept TYPE string;
DEFINE FIELD OVERWRITE name ON TABLE concept TYPE string;
DEFINE FIELD OVERWRITE summary ON TABLE concept TYPE option<string>;
-- Background knowledge the lecture assumes but does not teach.
DEFINE FIELD OVERWRITE prior ON TABLE concept TYPE bool DEFAULT false;
-- The broader concept this one is a part of.
DEFINE FIELD OVERWRITE parent ON TABLE concept TYPE option<record<concept>>;
-- Where the lecture teaches it: search sections of one material revision.
-- Positions of another revision than the material's current one are stale.
DEFINE FIELD OVERWRITE material ON TABLE concept TYPE option<record<material>>;
DEFINE FIELD OVERWRITE rev ON TABLE concept TYPE option<int>;
DEFINE FIELD OVERWRITE positions ON TABLE concept TYPE array<int> DEFAULT [];
-- Derived from the evidence (understanding.ts applyObservation).
DEFINE FIELD OVERWRITE level ON TABLE concept TYPE string DEFAULT 'unseen'
  ASSERT $value IN ['unseen', 'introduced', 'struggling', 'shaky', 'solid', 'mastered'];
DEFINE FIELD OVERWRITE score ON TABLE concept TYPE option<float>;
DEFINE FIELD OVERWRITE checks ON TABLE concept TYPE int DEFAULT 0;
DEFINE FIELD OVERWRITE successes ON TABLE concept TYPE int DEFAULT 0;
DEFINE FIELD OVERWRITE deep_successes ON TABLE concept TYPE int DEFAULT 0;
DEFINE FIELD OVERWRITE last_outcome ON TABLE concept TYPE option<string>;
DEFINE FIELD OVERWRITE explained_at ON TABLE concept TYPE option<datetime>;
DEFINE FIELD OVERWRITE last_checked_at ON TABLE concept TYPE option<datetime>;
DEFINE FIELD OVERWRITE first_success_at ON TABLE concept TYPE option<datetime>;
DEFINE FIELD OVERWRITE last_success_at ON TABLE concept TYPE option<datetime>;
-- Open wrong beliefs and missing pieces, until resolved.
DEFINE FIELD OVERWRITE misconceptions ON TABLE concept TYPE array<string> DEFAULT [];
DEFINE FIELD OVERWRITE gaps ON TABLE concept TYPE array<string> DEFAULT [];
-- Compare-and-set counter for concurrent updates.
DEFINE FIELD OVERWRITE version ON TABLE concept TYPE int DEFAULT 0;
DEFINE FIELD OVERWRITE created_at ON TABLE concept TYPE datetime DEFAULT time::now();
DEFINE FIELD OVERWRITE updated_at ON TABLE concept TYPE datetime VALUE time::now();

DEFINE INDEX IF NOT EXISTS concept_course_key ON TABLE concept FIELDS course, key UNIQUE;

-- `in` builds on `out`: explain `out` first.
DEFINE TABLE OVERWRITE requires TYPE RELATION IN concept OUT concept SCHEMAFULL
  COMMENT 'sp00ky:nosync'
  PERMISSIONS
    FOR select WHERE owner = $auth.id
    FOR create, update, delete WHERE false;

DEFINE FIELD OVERWRITE owner ON TABLE requires TYPE record<user>;
DEFINE FIELD OVERWRITE course ON TABLE requires TYPE record<course>;
DEFINE FIELD OVERWRITE created_at ON TABLE requires TYPE datetime DEFAULT time::now();

DEFINE INDEX IF NOT EXISTS requires_pair ON TABLE requires FIELDS in, out UNIQUE;
DEFINE INDEX IF NOT EXISTS requires_course ON TABLE requires FIELDS course;

-- One observation of the student: what they said or did about a concept.
DEFINE TABLE OVERWRITE concept_evidence SCHEMAFULL
  COMMENT 'sp00ky:nosync'
  PERMISSIONS
    FOR select WHERE owner = $auth.id
    FOR create, update, delete WHERE false;

DEFINE FIELD OVERWRITE owner ON TABLE concept_evidence TYPE record<user>;
DEFINE FIELD OVERWRITE course ON TABLE concept_evidence TYPE record<course>;
DEFINE FIELD OVERWRITE concept ON TABLE concept_evidence TYPE record<concept>;
-- The logged session, set when it is saved.
DEFINE FIELD OVERWRITE call ON TABLE concept_evidence TYPE option<record<call>>;
DEFINE FIELD OVERWRITE material ON TABLE concept_evidence TYPE option<record<material>>;
DEFINE FIELD OVERWRITE position ON TABLE concept_evidence TYPE option<int>;
DEFINE FIELD OVERWRITE outcome ON TABLE concept_evidence TYPE string
  ASSERT $value IN ['explained', 'asked', 'unsure', 'wrong', 'partial', 'recalled', 'explained_back', 'applied'];
DEFINE FIELD OVERWRITE note ON TABLE concept_evidence TYPE string;
DEFINE FIELD OVERWRITE misconception ON TABLE concept_evidence TYPE option<string>;
DEFINE FIELD OVERWRITE gap ON TABLE concept_evidence TYPE option<string>;
DEFINE FIELD OVERWRITE resolved ON TABLE concept_evidence TYPE array<string> DEFAULT [];
DEFINE FIELD OVERWRITE score_after ON TABLE concept_evidence TYPE option<float>;
DEFINE FIELD OVERWRITE level_after ON TABLE concept_evidence TYPE string;
DEFINE FIELD OVERWRITE at ON TABLE concept_evidence TYPE datetime DEFAULT time::now();

DEFINE INDEX IF NOT EXISTS concept_evidence_concept ON TABLE concept_evidence FIELDS concept, at;
DEFINE INDEX IF NOT EXISTS concept_evidence_course ON TABLE concept_evidence FIELDS course, at;

-- The main schema's course and material events do not know these tables
-- (sp00ky does not sync them), so they clean up here. Deleting a concept
-- removes its `requires` edges with it.
DEFINE EVENT OVERWRITE course_concepts_deleted ON TABLE course WHEN $event = 'DELETE' THEN {
  DELETE concept WHERE course = $before.id;
};
DEFINE EVENT OVERWRITE concept_deleted ON TABLE concept WHEN $event = 'DELETE' THEN {
  DELETE concept_evidence WHERE concept = $before.id;
};
-- What the student understood stays when a lecture goes; only where it is taught does.
DEFINE EVENT OVERWRITE material_concepts_unanchored ON TABLE material WHEN $event = 'DELETE' THEN {
  UPDATE concept SET material = NONE, rev = NONE, positions = [] WHERE material = $before.id;
};

-- ##################################################################
-- KNOWLEDGE GRAPH BUILDS (buildMissingGraphs in src/service.ts)
--
-- The latest memory-only Workflow run of each material, so the backfill
-- keeps only a few builds going at once (Agent Memory refuses a burst) and
-- tries again, a bounded number of times, when one ends without a graph.
-- ##################################################################

DEFINE TABLE OVERWRITE graph_build SCHEMAFULL
  COMMENT 'sp00ky:nosync'
  PERMISSIONS NONE;

DEFINE FIELD OVERWRITE material ON TABLE graph_build TYPE record<material>;
-- The material revision the run builds, and the attempts made for it.
DEFINE FIELD OVERWRITE rev ON TABLE graph_build TYPE int;
DEFINE FIELD OVERWRITE attempts ON TABLE graph_build TYPE int DEFAULT 0;
DEFINE FIELD OVERWRITE run ON TABLE graph_build TYPE string;
DEFINE FIELD OVERWRITE started_at ON TABLE graph_build TYPE datetime DEFAULT time::now();

DEFINE EVENT OVERWRITE material_graph_build_deleted ON TABLE material WHEN $event = 'DELETE' THEN {
  DELETE graph_build WHERE material = $before.id;
};

-- ##################################################################
-- SOURCES (src/fetch)
--
-- The server-only half of a `fetcher` (main schema): its Canvas token,
-- sealed with FETCHER_KEY (fetch/secret.ts), and what it imported, so a run
-- knows what is new, what changed and what the student deleted.
-- ##################################################################

DEFINE TABLE OVERWRITE fetcher_secret SCHEMAFULL
  COMMENT 'sp00ky:nosync'
  PERMISSIONS NONE;
DEFINE FIELD OVERWRITE fetcher ON TABLE fetcher_secret TYPE record<fetcher>;
DEFINE FIELD OVERWRITE token ON TABLE fetcher_secret TYPE string;
DEFINE FIELD OVERWRITE updated_at ON TABLE fetcher_secret TYPE datetime VALUE time::now();

-- One item of a source and the material it became. `material` NONE: the
-- student deleted it, so it is never imported again. The id is derived from
-- the fetcher and the item (fetch/run.ts ledgerId).
DEFINE TABLE OVERWRITE fetched_item SCHEMAFULL
  COMMENT 'sp00ky:nosync'
  PERMISSIONS NONE;
DEFINE FIELD OVERWRITE owner ON TABLE fetched_item TYPE record<user>;
DEFINE FIELD OVERWRITE fetcher ON TABLE fetched_item TYPE record<fetcher>;
DEFINE FIELD OVERWRITE external_id ON TABLE fetched_item TYPE string;
DEFINE FIELD OVERWRITE material ON TABLE fetched_item TYPE option<record<material>>;
-- The item's version when it was last imported (ImportRow.version).
DEFINE FIELD OVERWRITE version ON TABLE fetched_item TYPE option<string>;
DEFINE FIELD OVERWRITE created_at ON TABLE fetched_item TYPE datetime DEFAULT time::now();
DEFINE FIELD OVERWRITE updated_at ON TABLE fetched_item TYPE datetime VALUE time::now();

DEFINE INDEX IF NOT EXISTS fetched_item_fetcher ON TABLE fetched_item FIELDS fetcher;
DEFINE INDEX IF NOT EXISTS fetched_item_material ON TABLE fetched_item FIELDS material;
-- The scheduled check looks for due sources.
DEFINE INDEX IF NOT EXISTS fetcher_due ON TABLE fetcher FIELDS enabled, next_run_at;

DEFINE EVENT OVERWRITE fetcher_server_deleted ON TABLE fetcher WHEN $event = 'DELETE' THEN {
  DELETE fetcher_secret WHERE fetcher = $before.id;
  DELETE fetched_item WHERE fetcher = $before.id;
};
DEFINE EVENT OVERWRITE material_fetched_deleted ON TABLE material WHEN $event = 'DELETE' THEN {
  UPDATE fetched_item SET material = NONE WHERE material = $before.id;
};

-- ##################################################################
-- HOMEWORK IMAGES (index.ts /api/answers/:id/images, /api/documents/:id/images)
--
-- Images pasted into an answer or a document (main schema `answer`,
-- `document`): a sketch, a photo of a drawing, a figure. The bytes are the
-- file /answers/<id> of the `materials` bucket; the Markdown refers to them
-- as `answer-image:<id>`.
-- ##################################################################

DEFINE TABLE OVERWRITE answer_image SCHEMAFULL
  COMMENT 'sp00ky:nosync'
  PERMISSIONS NONE;
DEFINE FIELD OVERWRITE owner ON TABLE answer_image TYPE record<user>;
-- The answer or document the image was pasted into.
DEFINE FIELD OVERWRITE answer ON TABLE answer_image TYPE record<answer | document>;
DEFINE FIELD OVERWRITE mime ON TABLE answer_image TYPE string;
DEFINE FIELD OVERWRITE size ON TABLE answer_image TYPE int;
DEFINE FIELD OVERWRITE created_at ON TABLE answer_image TYPE datetime DEFAULT time::now();

DEFINE INDEX IF NOT EXISTS answer_image_answer ON TABLE answer_image FIELDS answer;

DEFINE EVENT OVERWRITE answer_image_file ON TABLE answer_image WHEN $event = 'DELETE'
  THEN type::file('materials', '/answers/' + <string>record::id($before.id)).delete();
DEFINE EVENT OVERWRITE answer_images_deleted ON TABLE answer WHEN $event = 'DELETE' THEN {
  DELETE answer_image WHERE answer = $before.id;
};
-- The editor saves the whole text, so without this a change made meanwhile by
-- someone else (the assistant, another device) that it had not received yet would
-- be overwritten. `base` names the text a save was built on. The client merges
-- and saves again when this refuses it. Only saves that set a new `base` are
-- checked: anything else writes as before.
DEFINE EVENT OVERWRITE document_stale_save ON TABLE document
  WHEN $event = 'UPDATE' AND $access = 'account' AND $after.base != NONE AND $after.base != $before.base
    AND $after.content != $before.content AND crypto::sha256($before.content ?? '') != $after.base
  THEN { THROW 'document_changed_elsewhere'; };

DEFINE EVENT OVERWRITE document_images_deleted ON TABLE document WHEN $event = 'DELETE' THEN {
  DELETE answer_image WHERE answer = $before.id;
};

-- ##################################################################
-- SITES (src/sites.ts)
--
-- The files of a static website uploaded to a course (main schema `site`).
-- The id is derived from the site and the path (sites.ts fileId). The bytes
-- are the files /sites/<id>_<n> of the `materials` bucket, parts of at most
-- 1 MiB, so a range of a long recording reads only the parts it needs.
-- ##################################################################

DEFINE TABLE OVERWRITE site_file SCHEMAFULL
  COMMENT 'sp00ky:nosync'
  PERMISSIONS NONE;
DEFINE FIELD OVERWRITE owner ON TABLE site_file TYPE record<user>;
DEFINE FIELD OVERWRITE site ON TABLE site_file TYPE record<site>;
DEFINE FIELD OVERWRITE path ON TABLE site_file TYPE string;
DEFINE FIELD OVERWRITE size ON TABLE site_file TYPE int;
DEFINE FIELD OVERWRITE parts ON TABLE site_file TYPE int;
DEFINE FIELD OVERWRITE created_at ON TABLE site_file TYPE datetime DEFAULT time::now();

DEFINE INDEX IF NOT EXISTS site_file_site ON TABLE site_file FIELDS site;

DEFINE EVENT OVERWRITE site_file_parts ON TABLE site_file WHEN $event = 'DELETE' THEN {
  FOR $n IN array::range(0, $before.parts) {
    type::file('materials', '/sites/' + <string>record::id($before.id) + '_' + <string>$n).delete();
  };
};
DEFINE EVENT OVERWRITE site_files_deleted ON TABLE site WHEN $event = 'DELETE' THEN {
  DELETE site_file WHERE site = $before.id;
};
