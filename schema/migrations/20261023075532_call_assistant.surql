-- Migration: call_assistant
-- Created: 2026-10-23T07:55:36.225117+00:00

-- Added
DEFINE FIELD OVERWRITE assistant ON call TYPE none | string PERMISSIONS FULL;
