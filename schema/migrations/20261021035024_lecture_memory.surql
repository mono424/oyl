-- Migration: lecture_memory
-- Created: 2026-10-21T03:50:27.166521+00:00

-- Added
DEFINE FIELD OVERWRITE learned_through ON material TYPE int DEFAULT -1 ASSERT $value >= -1 PERMISSIONS FOR select, create FULL, FOR update WHERE false;
DEFINE FIELD OVERWRITE memory_rev ON material TYPE int DEFAULT 0 PERMISSIONS FOR select, create FULL, FOR update WHERE false;
