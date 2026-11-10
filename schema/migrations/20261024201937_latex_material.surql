-- Migration: latex_material
-- Created: 2026-10-24T20:19:40.017025+00:00

-- Modified
DEFINE FIELD OVERWRITE kind ON material TYPE string ASSERT $value INSIDE ['text', 'markdown', 'pdf', 'tex'] PERMISSIONS FOR select, create FULL, FOR update WHERE false;

-- Added
DEFINE FIELD OVERWRITE import_tex ON fetcher TYPE none | bool DEFAULT false PERMISSIONS FULL;
