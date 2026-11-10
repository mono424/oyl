-- Migration: remove_crdt
-- Hand-written: the generated diff also removed Sp00ky's internal _00_ tables.
-- The @crdt annotation left these behind. No client or API code writes through
-- them, and all were empty when this was written.

REMOVE FIELD IF EXISTS content__crdt ON TABLE answer;
REMOVE FIELD IF EXISTS content__crdt ON TABLE document;
REMOVE TABLE IF EXISTS answer__crdt_log;
REMOVE TABLE IF EXISTS document__crdt_log;
