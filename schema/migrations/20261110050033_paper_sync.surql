-- Migration: paper_sync
-- Created: 2026-11-10T05:00:42.207877+00:00
-- Generated, then trimmed: the diff also listed `content` of `answer` and
-- `document` without their DEFAULT '' (drift from before this change, left
-- for its own migration).

-- Added
DEFINE TABLE OVERWRITE paper_revision TYPE NORMAL SCHEMAFULL PERMISSIONS FOR select WHERE owner = $auth.id, FOR create, update, delete WHERE false;
DEFINE TABLE OVERWRITE paper_sync TYPE NORMAL SCHEMAFULL PERMISSIONS FOR select WHERE owner = $auth.id, FOR create, update, delete WHERE false;
DEFINE FIELD OVERWRITE author ON paper_revision TYPE string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE created_at ON paper_revision TYPE datetime DEFAULT time::now() READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE file_count ON paper_revision TYPE int PERMISSIONS FULL;
DEFINE FIELD OVERWRITE files ON paper_revision TYPE string COMMENT 'sp00ky:opaque' PERMISSIONS FULL;
DEFINE FIELD OVERWRITE message ON paper_revision TYPE none | string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE n ON paper_revision TYPE int READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE owner ON paper_revision TYPE record<user> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE paper ON paper_revision TYPE record<paper_sync> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE parent ON paper_revision TYPE none | record<paper_revision> PERMISSIONS FULL;
DEFINE FIELD OVERWRITE size ON paper_revision TYPE int PERMISSIONS FULL;
DEFINE FIELD OVERWRITE created_at ON paper_sync TYPE datetime DEFAULT time::now() READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE deleted_at ON paper_sync TYPE none | datetime PERMISSIONS FULL;
DEFINE FIELD OVERWRITE engine ON paper_sync TYPE string ASSERT $value INSIDE ['typst', 'latex'] PERMISSIONS FULL;
DEFINE FIELD OVERWRITE head ON paper_sync TYPE none | record<paper_revision> PERMISSIONS FULL;
DEFINE FIELD OVERWRITE main ON paper_sync TYPE string ASSERT string::len($value) > 0 PERMISSIONS FULL;
DEFINE FIELD OVERWRITE name ON paper_sync TYPE string ASSERT string::len($value) > 0 PERMISSIONS FULL;
DEFINE FIELD OVERWRITE owner ON paper_sync TYPE record<user> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE updated_at ON paper_sync TYPE datetime VALUE time::now() PERMISSIONS FULL;
DEFINE FIELD OVERWRITE updated_by ON paper_sync TYPE none | string PERMISSIONS FULL;
DEFINE INDEX OVERWRITE paper_revision_paper ON paper_revision FIELDS paper, n UNIQUE;
DEFINE INDEX OVERWRITE paper_sync_owner ON paper_sync FIELDS owner;
DEFINE EVENT OVERWRITE paper_sync_deleted ON paper_sync WHEN $event = 'DELETE' THEN { DELETE paper_revision WHERE paper = $before.id };
