-- D-08: the first administrator can only exist via this migration, never via a public route.
-- `main.rs` has no argument parsing today, and Phase 7 will build a full clap CLI for
-- REQ-moderation anyway (see DEC-moderation-model) — introducing a throwaway CLI subcommand here
-- would only be replaced later. A migration seed also has an established precedent in this
-- project (20260807000002_seed_dev_author.sql).
-- The `role` column is written here and nowhere else in the whole codebase: no route ever binds
-- `role` on INSERT (see repository::insert_user), so 'admin' is structurally unreachable through
-- the public API regardless of what a client sends.
-- Sentinel verified_via=seed-admin is deliberately distinct from dev-seed (Phase 3's temporary
-- submission author, retired by the next migration) and from official-api (real accounts created
-- by POST /v1/public/login): all three mechanisms must stay distinguishable by a plain read of
-- the `users` table.
-- Deliberate side effect, not a bug: because `users.name` is UNIQUE, this row also reserves the
-- pseudo `admin` — nobody can register that name through POST /v1/public/login once this
-- migration has run.
INSERT INTO users (id, name, verified_via, role, email, password_hash)
VALUES (
    '00000000-0000-0000-0000-000000000002',
    'admin',
    'seed-admin',
    'admin',
    NULL,
    NULL
)
ON CONFLICT (id) DO NOTHING;
