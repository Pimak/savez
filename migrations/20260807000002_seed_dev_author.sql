-- Temporary mechanism (D-02/D-03, see .planning/phases/03-persistance/03-CONTEXT.md).
-- Authentication does not exist yet (it arrives in Phase 5 via JWT + the `x-token` middleware).
-- Until then, every puzzle submitted through POST /v1/puzzles/submit is attached to this fixed
-- seed author. The UUID literal below MUST match the `DEV_SEED_AUTHOR_ID` Rust constant in
-- src/repository.rs byte-for-byte (plan 03-03 wires it into the submit handler).
-- TODO(Phase 5): delete this migration's effect (or add a follow-up migration that removes the
-- row) once JWT auth supplies a real author_id for every submission — this row and the constant
-- referencing it are the single, clearly identified retirement point (D-03).
INSERT INTO users (id, name, verified_via, role, email, password_hash)
VALUES (
    '00000000-0000-0000-0000-000000000001',
    'dev-seed-author',
    'dev-seed', -- sentinel value, deliberately distinct from official-api / open / steam-openid
    'user',
    NULL,
    NULL
)
ON CONFLICT (id) DO NOTHING;
