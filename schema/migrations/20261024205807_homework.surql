-- Migration: homework
-- Created: 2026-10-24T20:58:10.132859+00:00

-- Modified
DEFINE EVENT OVERWRITE course_deleted ON course WHEN $event = 'DELETE' THEN { DELETE material WHERE course = $before.id; DELETE call WHERE course = $before.id; DELETE fetcher WHERE course = $before.id; DELETE homework WHERE course = $before.id; };
DEFINE EVENT OVERWRITE material_deleted ON material WHEN $event = 'DELETE' THEN { UPDATE homework SET sheet = NONE WHERE sheet = $before.id; DELETE chunk WHERE material = $before.id; DELETE asset WHERE material = $before.id; DELETE source_part WHERE material = $before.id; };

-- Added
DEFINE TABLE OVERWRITE answer TYPE NORMAL SCHEMAFULL PERMISSIONS FOR select, update, delete WHERE owner = $auth.id, FOR create WHERE owner = $auth.id AND homework.owner = $auth.id;
DEFINE TABLE OVERWRITE homework TYPE NORMAL SCHEMAFULL PERMISSIONS FOR select, update, delete WHERE owner = $auth.id, FOR create WHERE owner = $auth.id AND course.owner = $auth.id;
DEFINE FIELD OVERWRITE anchor ON answer TYPE none | string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE anchor_page ON answer TYPE none | int PERMISSIONS FULL;
DEFINE FIELD OVERWRITE content ON answer TYPE string DEFAULT '' PERMISSIONS FULL;
DEFINE FIELD OVERWRITE context ON answer TYPE none | string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE created_at ON answer TYPE datetime DEFAULT time::now() READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE homework ON answer TYPE record<homework> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE key ON answer TYPE string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE label ON answer TYPE string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE owner ON answer TYPE record<user> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE page ON answer TYPE none | int PERMISSIONS FULL;
DEFINE FIELD OVERWRITE points ON answer TYPE none | float PERMISSIONS FULL;
DEFINE FIELD OVERWRITE position ON answer TYPE int PERMISSIONS FULL;
DEFINE FIELD OVERWRITE prompt ON answer TYPE string DEFAULT '' PERMISSIONS FULL;
DEFINE FIELD OVERWRITE question ON answer TYPE string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE question_title ON answer TYPE none | string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE region ON answer TYPE none | string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE updated_at ON answer TYPE datetime VALUE time::now() PERMISSIONS FULL;
DEFINE FIELD OVERWRITE course ON homework TYPE record<course> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE created_at ON homework TYPE datetime DEFAULT time::now() READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE owner ON homework TYPE record<user> READONLY PERMISSIONS FULL;
DEFINE FIELD OVERWRITE sheet ON homework TYPE none | record<material> PERMISSIONS FULL;
DEFINE FIELD OVERWRITE title ON homework TYPE string ASSERT string::len($value) > 0 PERMISSIONS FULL;
DEFINE FIELD OVERWRITE updated_at ON homework TYPE datetime VALUE time::now() PERMISSIONS FULL;
DEFINE INDEX OVERWRITE answer_homework ON answer FIELDS homework, position;
DEFINE INDEX OVERWRITE homework_course ON homework FIELDS course;
DEFINE EVENT OVERWRITE homework_deleted ON homework WHEN $event = 'DELETE' THEN { DELETE answer WHERE homework = $before.id };
