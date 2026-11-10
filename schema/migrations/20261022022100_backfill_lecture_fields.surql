-- Migration: backfill_lecture_fields
-- Created: 2026-10-22T02:21:00.732058+00:00
--
-- `lecture_memory` added `learned_through` and `memory_rev` to material with a
-- DEFAULT, which SurrealDB only applies on create. Rows that already existed
-- kept NONE, and since both fields are plain `int`, every later UPDATE of such
-- a row failed to coerce them (saving lecture progress, reindexing). Give them
-- the values a new row starts with. One statement, so each row is written once.
UPDATE material SET
  learned_through = learned_through ?? -1,
  memory_rev = memory_rev ?? 0
WHERE learned_through IS NONE OR memory_rev IS NONE;
