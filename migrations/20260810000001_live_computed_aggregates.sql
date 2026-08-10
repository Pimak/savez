-- D-18/ADR 0005: puzzles.likes/completions/difficulty/average_time were stored columns from the
-- Phase 3 schema (migrations/20260807000001_initial_schema.sql) but no write path has ever touched
-- them since -- the researcher confirmed 0 non-default rows in `puzzles` on the project's dev
-- database. Phase 7 replaces them with a live aggregation over `puzzle_completions` computed on
-- every read (D-15: list/search/download), so these four columns become dead weight with nothing
-- left to migrate -- only to drop. `puzzles.downloads` is explicitly NOT touched here (D-19): it
-- stays a stored counter, incremented at write time, because no download-events log table exists
-- to recompute it from.
ALTER TABLE puzzles
    DROP COLUMN likes,
    DROP COLUMN completions,
    DROP COLUMN difficulty,
    DROP COLUMN average_time;

-- D-20: the live aggregation's read path (derived-table GROUP BY for list/search, point-lookup
-- GROUP BY for a single puzzle) leans on these two indexes -- measured by the researcher via
-- EXPLAIN ANALYZE at 8,000 puzzles / 120,000 completions: 89ms for the list form, 0.362ms for the
-- point-lookup form, both with these indexes in place. A materialized view was explicitly rejected
-- (D-20) as disproportionate maintenance cost for CON-ops-cost's single-VPS, <1h/month budget.
CREATE INDEX idx_puzzle_completions_puzzle_id ON puzzle_completions (puzzle_id);
CREATE INDEX idx_puzzle_completions_puzzle_id_liked ON puzzle_completions (puzzle_id, liked);
