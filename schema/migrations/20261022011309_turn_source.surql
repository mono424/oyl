-- Migration: turn_source
-- Created: 2026-10-22T01:13:12.084195+00:00

-- Modified
DEFINE EVENT OVERWRITE call_deleted ON call WHEN $event = 'DELETE' THEN { DELETE call_turn WHERE call = $before.id; DELETE turn_source WHERE call = $before.id; };

-- Added
DEFINE BUCKET OVERWRITE materials BACKEND 'file:/buckets/materials' PERMISSIONS NONE;
DEFINE TABLE OVERWRITE turn_source TYPE NORMAL SCHEMAFULL PERMISSIONS FOR select WHERE owner = $auth.id, FOR create, update, delete WHERE false;
DEFINE FIELD OVERWRITE at ON turn_source TYPE int PERMISSIONS FULL;
DEFINE FIELD OVERWRITE call ON turn_source TYPE record<call> PERMISSIONS FULL;
DEFINE FIELD OVERWRITE created_at ON turn_source TYPE datetime DEFAULT time::now() PERMISSIONS FULL;
DEFINE FIELD OVERWRITE heading ON turn_source TYPE none | string PERMISSIONS FULL;
DEFINE FIELD OVERWRITE material ON turn_source TYPE record<material> PERMISSIONS FULL;
DEFINE FIELD OVERWRITE owner ON turn_source TYPE record<user> PERMISSIONS FULL;
DEFINE FIELD OVERWRITE page ON turn_source TYPE none | int PERMISSIONS FULL;
DEFINE FIELD OVERWRITE page_end ON turn_source TYPE none | int PERMISSIONS FULL;
DEFINE FIELD OVERWRITE ref ON turn_source TYPE int PERMISSIONS FULL;
DEFINE FIELD OVERWRITE seq ON turn_source TYPE int PERMISSIONS FULL;
DEFINE FIELD OVERWRITE title ON turn_source TYPE string PERMISSIONS FULL;
DEFINE INDEX OVERWRITE turn_source_call ON turn_source FIELDS call, seq;
