-- Source: docs/cahier-des-charges.md §4.3, transposed to native PostgreSQL types.
-- IDs: users.id is a native UUID (gen_random_uuid() is built into Postgres core since v13 — no
-- pgcrypto extension needed). puzzles.id stays an autoincrement identity column per SPEC.
-- Booleans: native BOOLEAN. Timestamps: TIMESTAMPTZ, defaulting to now(). puzzles.data: JSONB.

CREATE TABLE users (
    id            UUID PRIMARY KEY NOT NULL DEFAULT gen_random_uuid(),
    name          TEXT NOT NULL UNIQUE,
    email         TEXT UNIQUE,
    password_hash TEXT,
    verified_via  TEXT NOT NULL, -- 'official-api' | 'open' | 'steam-openid' in real use;
                                 -- Phase 3's seed row uses a distinct sentinel (D-02) — no CHECK
                                 -- constraint here on purpose, to avoid a second migration to
                                 -- remove the sentinel value from an enum list in Phase 5
    steam_id      TEXT,
    role          TEXT NOT NULL DEFAULT 'user', -- user | moderator | admin
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE puzzles (
    id            INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    short_key     TEXT NOT NULL UNIQUE,
    title         TEXT NOT NULL,
    author_id     UUID NOT NULL REFERENCES users(id),
    data          JSONB NOT NULL, -- PuzzleGameData, stored decompressed (DEC-data-storage-decompressed)
    likes         INTEGER NOT NULL DEFAULT 0,
    downloads     INTEGER NOT NULL DEFAULT 0, -- not incremented in Phase 3 (D-06)
    completions   INTEGER NOT NULL DEFAULT 0,
    difficulty    REAL,
    average_time  REAL,
    locale        TEXT,
    hidden_at     TIMESTAMPTZ,
    hidden_by     UUID REFERENCES users(id),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE puzzle_completions (
    id           INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id      UUID NOT NULL REFERENCES users(id),
    puzzle_id    INTEGER NOT NULL REFERENCES puzzles(id),
    time_taken   REAL NOT NULL,
    liked        BOOLEAN NOT NULL DEFAULT false,
    completed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, puzzle_id)
);

CREATE TABLE puzzle_reports (
    id            INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id       UUID NOT NULL REFERENCES users(id),
    puzzle_id     INTEGER NOT NULL REFERENCES puzzles(id),
    reason        TEXT NOT NULL, -- profane | unsolvable | trolling
    status        TEXT NOT NULL DEFAULT 'pending', -- pending | upheld | rejected
    reviewed_at   TIMESTAMPTZ,
    reviewer_id   UUID REFERENCES users(id),
    review_notes  TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, puzzle_id)
);

CREATE TABLE user_bans (
    id                 INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id            UUID NOT NULL REFERENCES users(id),
    reason             TEXT NOT NULL,
    moderator_id       UUID NOT NULL REFERENCES users(id),
    expires_at         TIMESTAMPTZ, -- NULL = permanent
    lifted_at          TIMESTAMPTZ,
    lift_reason        TEXT,
    lift_moderator_id  UUID REFERENCES users(id),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE moderation_log (
    id           INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    moderator_id UUID NOT NULL REFERENCES users(id),
    action       TEXT NOT NULL, -- hide_puzzle | unhide_puzzle | delete_puzzle | ban_user | ...
    target_type  TEXT NOT NULL,
    target_id    TEXT NOT NULL,
    details      JSONB,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
    -- append-only per DEC-moderation-model: no UPDATE/DELETE statements should ever target this table
);
