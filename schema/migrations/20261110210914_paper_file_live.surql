-- Migration: paper_file_live
-- Live text of synced papers: `paper_file` with its `@crdt` content, and the
-- tables sp00ky generates for it. The generated diff also redefined
-- answer.content and document.content (schema.surql no longer has their
-- DEFAULT ''); that drift is left for its own migration.
-- Created: 2026-11-10T21:09:16.895459+00:00

-- Modified
DEFINE EVENT OVERWRITE paper_sync_deleted ON paper_sync WHEN $event = 'DELETE' THEN { DELETE paper_revision WHERE paper = $before.id; DELETE paper_file WHERE paper = $before.id; };

-- Added
DEFINE TABLE OVERWRITE _00_crdt_doc TYPE NORMAL SCHEMAFULL PERMISSIONS FOR select WHERE doc.tb() = 'paper_file' AND doc.owner = $auth.id, FOR create, update, delete NONE;
DEFINE TABLE OVERWRITE _00_crdt_peer TYPE NORMAL SCHEMAFULL PERMISSIONS FOR select WHERE author = $auth.id, FOR create WHERE $auth.id != NONE AND $access != '_00_impersonate', FOR update, delete NONE;
DEFINE TABLE OVERWRITE paper_file TYPE NORMAL SCHEMAFULL PERMISSIONS FOR select, update WHERE owner = $auth.id, FOR create, delete WHERE false;
DEFINE TABLE OVERWRITE paper_file__crdt_log TYPE NORMAL SCHEMAFULL COMMENT 'sp00ky:crdt_log content=text' PERMISSIONS FOR select WHERE doc.owner = $auth.id, FOR create WHERE $access != '_00_impersonate' AND doc.owner = $auth.id, FOR update, delete NONE;
DEFINE FIELD OVERWRITE doc ON _00_crdt_doc TYPE record PERMISSIONS FULL;
DEFINE FIELD OVERWRITE field ON _00_crdt_doc TYPE string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE folded_at ON _00_crdt_doc TYPE none | datetime PERMISSIONS FULL;
DEFINE FIELD OVERWRITE lease_by ON _00_crdt_doc TYPE none | string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE lease_until ON _00_crdt_doc TYPE none | datetime PERMISSIONS FULL;
DEFINE FIELD OVERWRITE rev ON _00_crdt_doc TYPE int DEFAULT 0 PERMISSIONS FULL;
DEFINE FIELD OVERWRITE snapshot ON _00_crdt_doc TYPE none | bytes PERMISSIONS FULL;
DEFINE FIELD OVERWRITE state_hash ON _00_crdt_doc TYPE none | string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE upto ON _00_crdt_doc TYPE none | object FLEXIBLE PERMISSIONS FULL;
DEFINE FIELD OVERWRITE at ON _00_crdt_peer TYPE datetime READONLY VALUE $before OR time::now() PERMISSIONS FULL;
DEFINE FIELD OVERWRITE author ON _00_crdt_peer TYPE record READONLY VALUE $before OR $auth.id PERMISSIONS FULL;
DEFINE FIELD OVERWRITE content ON paper_file TYPE string DEFAULT '' VALUE (IF $auth.id != NONE AND $before != NONE { $before } ELSE { $value OR '' }) COMMENT 'sp00ky:opaque' PERMISSIONS FULL;
DEFINE FIELD OVERWRITE content__crdt ON paper_file TYPE none | object FLEXIBLE VALUE (IF $auth.id != NONE { $before } ELSE { $value }) PERMISSIONS FULL;
DEFINE FIELD OVERWRITE created_at ON paper_file TYPE datetime DEFAULT time::now() READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE owner ON paper_file TYPE record<user> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE paper ON paper_file TYPE record<paper_sync> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE path ON paper_file TYPE string READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE seed ON paper_file TYPE string READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE author ON paper_file__crdt_log TYPE none | record READONLY VALUE $before OR $auth.id OR $value PERMISSIONS FULL;
DEFINE FIELD OVERWRITE created_at ON paper_file__crdt_log TYPE datetime READONLY VALUE $before OR time::now() PERMISSIONS FULL;
DEFINE FIELD OVERWRITE data ON paper_file__crdt_log TYPE bytes READONLY ASSERT bytes::len($value) <= 262144 COMMENT 'sp00ky:opaque' PERMISSIONS FULL;
DEFINE FIELD OVERWRITE doc ON paper_file__crdt_log TYPE record<paper_file> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE field ON paper_file__crdt_log TYPE string READONLY ASSERT $value INSIDE ['content'] PERMISSIONS FULL;
DEFINE FIELD OVERWRITE peer ON paper_file__crdt_log TYPE string READONLY ASSERT string::len($value) <= 20 AND ($auth.id = NONE OR (SELECT VALUE author FROM ONLY type::record('_00_crdt_peer', $value)) = $auth.id) PERMISSIONS FULL;
DEFINE FIELD OVERWRITE size ON paper_file__crdt_log TYPE int READONLY VALUE bytes::len(data) PERMISSIONS FULL;
DEFINE INDEX OVERWRITE _00_crdt_doc_doc ON _00_crdt_doc FIELDS doc, field UNIQUE;
DEFINE INDEX OVERWRITE paper_file_path ON paper_file FIELDS paper, path UNIQUE;
DEFINE INDEX OVERWRITE paper_file__crdt_log_doc ON paper_file__crdt_log FIELDS doc, field;
DEFINE EVENT OVERWRITE _00_crdt_cleanup ON paper_file WHEN $event = 'DELETE' THEN { DELETE _00_crdt_doc WHERE doc = $before.id };
