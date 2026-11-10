-- Migration: sites
-- Created: 2026-11-08T21:19:40.432643+00:00

-- Modified
DEFINE EVENT OVERWRITE course_deleted ON course WHEN $event = 'DELETE' THEN { DELETE material WHERE course = $before.id; DELETE call WHERE course = $before.id; DELETE fetcher WHERE course = $before.id; DELETE homework WHERE course = $before.id; DELETE document WHERE course = $before.id; DELETE site WHERE course = $before.id; };

-- Added
DEFINE TABLE OVERWRITE site TYPE NORMAL SCHEMAFULL PERMISSIONS FOR select, update, delete WHERE owner = $auth.id, FOR create WHERE owner = $auth.id AND course.owner = $auth.id;
DEFINE FIELD OVERWRITE course ON site TYPE record<course> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE created_at ON site TYPE datetime DEFAULT time::now() READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE entry ON site TYPE none | string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE file_count ON site TYPE int DEFAULT 0 PERMISSIONS FULL;
DEFINE FIELD OVERWRITE owner ON site TYPE record<user> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE pages ON site TYPE string DEFAULT '[]' PERMISSIONS FULL;
DEFINE FIELD OVERWRITE size ON site TYPE int DEFAULT 0 PERMISSIONS FULL;
DEFINE FIELD OVERWRITE status ON site TYPE string DEFAULT 'uploading' ASSERT $value INSIDE ['uploading', 'ready'] PERMISSIONS FULL;
DEFINE FIELD OVERWRITE title ON site TYPE string ASSERT string::len($value) > 0 PERMISSIONS FULL;
DEFINE FIELD OVERWRITE updated_at ON site TYPE datetime VALUE time::now() PERMISSIONS FULL;
DEFINE INDEX OVERWRITE site_course ON site FIELDS course;
