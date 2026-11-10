-- Migration: document_format
-- Created: 2026-10-26T17:09:02.459322+00:00

-- Added
DEFINE FIELD OVERWRITE format ON document TYPE string DEFAULT 'markdown' ASSERT $value INSIDE ['markdown', 'tex'] PERMISSIONS FULL;
DEFINE FIELD OVERWRITE cite_key ON paper TYPE none | string PERMISSIONS FULL;
