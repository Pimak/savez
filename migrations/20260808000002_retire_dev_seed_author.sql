-- Executes the TODO(Phase 5) left by 20260807000002_seed_dev_author.sql: JWT auth (Phase 5) now
-- supplies a real author_id for every submission (see routes::puzzles::submit /
-- auth::extractor::AuthUser), so the temporary Phase 3 seed author is no longer needed. The
-- matching Rust-side constant (`DEV_SEED_AUTHOR_ID` in src/repository.rs) was already deleted by
-- plan 05-05 — this migration is the row's retirement point.
-- Migrations are immutable history: we never rewrite or delete
-- 20260807000002_seed_dev_author.sql itself, only neutralize its effect with a follow-up
-- migration.
-- All three conditions below are required:
--   - the UUID targets exactly the seed row;
--   - the `verified_via = 'dev-seed'` sentinel guards against deleting an unrelated row that
--     might later reuse this UUID;
--   - the NOT EXISTS guard avoids a foreign-key violation on a development database that still
--     has puzzles attributed to the seed author. On any fresh database (every #[sqlx::test], CI,
--     a new production deploy) no such puzzle exists, so the row is deleted as intended.
DELETE FROM users
WHERE id = '00000000-0000-0000-0000-000000000001'
  AND verified_via = 'dev-seed'
  AND NOT EXISTS (
    SELECT 1 FROM puzzles WHERE author_id = '00000000-0000-0000-0000-000000000001'
  );
