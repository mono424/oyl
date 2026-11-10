-- Migration: documents
-- Created: 2026-10-26T16:34:18.172292+00:00

-- Modified
DEFINE EVENT OVERWRITE course_deleted ON course WHEN $event = 'DELETE' THEN { DELETE material WHERE course = $before.id; DELETE call WHERE course = $before.id; DELETE fetcher WHERE course = $before.id; DELETE homework WHERE course = $before.id; DELETE document WHERE course = $before.id; };
DEFINE EVENT OVERWRITE material_deleted ON material WHEN $event = 'DELETE' THEN { UPDATE homework SET sheet = NONE WHERE sheet = $before.id; UPDATE paper SET material = NONE WHERE material = $before.id; DELETE chunk WHERE material = $before.id; DELETE asset WHERE material = $before.id; DELETE source_part WHERE material = $before.id; };

-- Added
DEFINE TABLE OVERWRITE document TYPE NORMAL SCHEMAFULL PERMISSIONS FOR select, update, delete WHERE owner = $auth.id, FOR create WHERE owner = $auth.id AND course.owner = $auth.id;
DEFINE TABLE OVERWRITE paper TYPE NORMAL SCHEMAFULL PERMISSIONS FOR select, update, delete WHERE owner = $auth.id, FOR create WHERE owner = $auth.id AND document.owner = $auth.id;
DEFINE FIELD OVERWRITE cite_style ON document TYPE string DEFAULT 'author-year' ASSERT $value INSIDE ['author-year', 'numeric'] PERMISSIONS FULL;
DEFINE FIELD OVERWRITE content ON document TYPE string DEFAULT '' PERMISSIONS FULL;
DEFINE FIELD OVERWRITE course ON document TYPE record<course> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE created_at ON document TYPE datetime DEFAULT time::now() READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE owner ON document TYPE record<user> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE title ON document TYPE string ASSERT string::len($value) > 0 PERMISSIONS FULL;
DEFINE FIELD OVERWRITE updated_at ON document TYPE datetime VALUE time::now() PERMISSIONS FULL;
DEFINE FIELD OVERWRITE authors ON paper TYPE none | string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE created_at ON paper TYPE datetime DEFAULT time::now() READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE document ON paper TYPE record<document> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE material ON paper TYPE none | record<material> PERMISSIONS FULL;
DEFINE FIELD OVERWRITE owner ON paper TYPE record<user> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE title ON paper TYPE string ASSERT string::len($value) > 0 PERMISSIONS FULL;
DEFINE FIELD OVERWRITE url ON paper TYPE none | string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE venue ON paper TYPE none | string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE year ON paper TYPE none | string PERMISSIONS FULL;
DEFINE INDEX OVERWRITE document_course ON document FIELDS course;
DEFINE INDEX OVERWRITE paper_document ON paper FIELDS document;
DEFINE EVENT OVERWRITE document_deleted ON document WHEN $event = 'DELETE' THEN { DELETE paper WHERE document = $before.id };
