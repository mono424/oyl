-- Migration: document_source
-- Created: 2026-10-26T16:56:49.752556+00:00

-- Modified
DEFINE EVENT OVERWRITE material_deleted ON material WHEN $event = 'DELETE' THEN { UPDATE homework SET sheet = NONE WHERE sheet = $before.id; UPDATE paper SET material = NONE WHERE material = $before.id; UPDATE document SET source = NONE WHERE source = $before.id; DELETE chunk WHERE material = $before.id; DELETE asset WHERE material = $before.id; DELETE source_part WHERE material = $before.id; };

-- Added
DEFINE FIELD OVERWRITE source ON document TYPE none | record<material> PERMISSIONS FULL;
DEFINE INDEX OVERWRITE document_source ON document FIELDS source;
