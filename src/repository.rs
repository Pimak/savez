use sqlx::PgPool;
use sqlx::types::Json;
use uuid::Uuid;

use crate::error::AppError;
use crate::routes::puzzles::{PuzzleFullData, PuzzleGameData, PuzzleMetadata, SubmitPuzzleRequest};

/// Single source of truth for `moderation_log.action` string literals (07-05-PLAN.md
/// `<interfaces>`). Plans 07-06 through 07-09 (report resolution, bans, role changes, rate-limit
/// and profanity-list config CLI) all reuse these constants — no moderation-action literal should
/// ever appear as a bare string anywhere else in this codebase.
pub mod moderation_action {
    pub const HIDE_PUZZLE: &str = "hide_puzzle";
    pub const UNHIDE_PUZZLE: &str = "unhide_puzzle";
    pub const DELETE_PUZZLE: &str = "delete_puzzle";
    pub const RESOLVE_REPORT: &str = "resolve_report";
    pub const BAN_USER: &str = "ban_user";
    pub const LIFT_BAN: &str = "lift_ban";
    pub const SET_ROLE: &str = "set_role";
    pub const RATELIMIT_SET: &str = "ratelimit_set";
    pub const PROFANITY_UPDATE: &str = "profanity_update";
}

/// SPEC §4.6 calls this threshold "configurable, default 3" — no D-01..D-20 decision asks for it
/// to actually be runtime-configurable, and REQUIREMENTS.md/ROADMAP both hardcode "3" literally.
/// Named constant instead of an inline literal so the single locked value is unambiguous and easy
/// to find; if a future plan is asked to make it configurable (CLI/DB, mirroring D-01's rate-limit
/// config table), this is the one place to change.
/// TODO(post-v1): make configurable via the rate-limit-style CLI/DB config table if ever needed.
pub const AUTO_HIDE_REPORT_THRESHOLD: i64 = 3;

/// Appends one append-only row to `moderation_log` (SC4, DEC-moderation-model). `executor` is a
/// generic `PgExecutor` rather than `&PgPool`: plan 07-06's permanent-delete flow must be able to
/// log inside its own transaction (a `Transaction<'_, Postgres>` also implements `PgExecutor`),
/// and this function must not force a fresh, uncoordinated pool connection in that case.
///
/// `moderation_log` receives ONLY `INSERT`s across this entire codebase — DEC-moderation-model's
/// append-only guarantee: no function anywhere may ever emit an `UPDATE` or `DELETE` against this
/// table. This is the sole write path.
pub async fn log_moderation_action(
    executor: impl sqlx::PgExecutor<'_>,
    moderator_id: Uuid,
    action: &str,
    target_type: &str,
    target_id: &str,
    details: Option<serde_json::Value>,
) -> Result<(), AppError> {
    sqlx::query!(
        r#"
        INSERT INTO moderation_log (moderator_id, action, target_type, target_id, details)
        VALUES ($1, $2, $3, $4, $5)
        "#,
        moderator_id,
        action,
        target_type,
        target_id,
        details,
    )
    .execute(executor)
    .await?;

    Ok(())
}

/// Inserts a new puzzle row. `author_id` ALWAYS comes from the caller-supplied, server-verified
/// JWT (`AuthUser.user_id` in `routes::puzzles::submit`) — never from `req` (there is none to
/// take: `SubmitPuzzleRequest` has no author-shaped field, and even if it did this function would
/// still ignore it, per D-01/D-02). Adding an author-shaped field to `SubmitPuzzleRequest` would
/// be a regression of T-03-21/T-05-05: the server, not the client, always determines authorship.
///
/// Does NOT pre-check `short_key` availability with a `SELECT` before the `INSERT` (would be a
/// TOCTOU race) — the `UNIQUE(short_key)` constraint is the single source of truth on uniqueness,
/// same pattern as `insert_user`'s `UNIQUE(name)` handling below. This is deliberately a SEPARATE
/// concern from the short key's FORMATION: `validation::validate_short_key` already rejected a
/// malformed key upstream in `routes::puzzles::submit`, before this function is ever called — a
/// unique-constraint violation here can therefore only mean "well-formed but already taken"
/// (`short-key-already-taken`), never a malformed one (`bad-short-key`). These are two distinct
/// rejections with two distinct wire codes, mapped by the `match` below.
pub async fn insert_puzzle(
    pool: &PgPool,
    req: &SubmitPuzzleRequest,
    author_id: Uuid,
) -> Result<i32, AppError> {
    let row = sqlx::query!(
        r#"
        INSERT INTO puzzles (short_key, title, author_id, data)
        VALUES ($1, $2, $3, $4)
        RETURNING id
        "#,
        req.short_key,
        req.title,
        author_id,
        Json(&req.data) as Json<&PuzzleGameData>,
    )
    .fetch_one(pool)
    .await
    .map_err(|err| match &err {
        sqlx::Error::Database(db_err) if db_err.is_unique_violation() => AppError::ShortKeyTaken,
        _ => AppError::Database(err),
    })?;

    Ok(row.id)
}

/// Newest-first puzzle listing. Sorted by `created_at DESC, id DESC`: the second key is
/// mandatory because `created_at DEFAULT now()` is the transaction start time and can collide
/// for two puzzles inserted in quick succession, which would otherwise make "newest first"
/// non-deterministic.
///
/// `current_user_id` is a NAKED `Uuid`, never `Option<Uuid>`: D-05 makes the caller (`routes::
/// puzzles::list`, `"new"` arm) mandatorily authenticated — there is no anonymous path left that
/// could call this function without a real user id.
///
/// Visibility predicate (docs/adr/0002-hidden-by-tri-state.md): keeps `WHERE p.hidden_at IS NULL`
/// unconditionally, even for the puzzle's own author. Unlike `find_puzzle_by_id`/
/// `find_puzzle_by_short_key`/`list_mine`, this is the "New" catalog view — an author who just hid
/// their own puzzle must NOT keep seeing it here, or they would reasonably conclude the hide
/// action silently failed. Author-side visibility of a hidden puzzle is preserved elsewhere
/// (direct download, `list_mine`), never in this listing.
///
/// D-15/ADR 0005: `completions`/`likes`/`average_time`/`difficulty` are no longer stored columns
/// on `puzzles` — they are computed live from a per-puzzle aggregate derived table (grouped by
/// puzzle id) joined against `puzzle_completions` (D-20's two indexes back this join; measured
/// 89ms at 8k puzzles / 120k
/// completions, versus 163-397ms for a `GROUP BY` over the already-joined row set — see
/// 07-01-PLAN.md `<interfaces>`). `difficulty` (D-14) is `completions / downloads`, `NULL` while
/// `downloads = 0` (never a division by zero). `average_time` (D-16) is the simple mean of every
/// `puzzle_completions.time_taken` row for the puzzle. `likes` (D-17) is not monotonic: a player
/// can push it back down by re-completing with `liked = false`. One query, no N+1.
pub async fn list_new(
    pool: &PgPool,
    current_user_id: Uuid,
) -> Result<Vec<PuzzleMetadata>, AppError> {
    let rows = sqlx::query!(
        r#"
        SELECT p.id, p.short_key, p.downloads, p.title, u.name AS author,
               COALESCE(agg.completions, 0) AS "completions!",
               COALESCE(agg.likes, 0)       AS "likes!",
               agg.average_time,
               CASE WHEN p.downloads = 0 THEN NULL
                    ELSE (COALESCE(agg.completions, 0)::real / p.downloads)::real
               END AS difficulty,
               (pc.user_id IS NOT NULL) AS "completed!"
        FROM puzzles p
        JOIN users u ON u.id = p.author_id
        LEFT JOIN (
            SELECT puzzle_id,
                   COUNT(*)                      AS completions,
                   COUNT(*) FILTER (WHERE liked) AS likes,
                   AVG(time_taken)::real         AS average_time
            FROM puzzle_completions
            GROUP BY puzzle_id
        ) agg ON agg.puzzle_id = p.id
        LEFT JOIN puzzle_completions pc ON pc.puzzle_id = p.id AND pc.user_id = $1
        WHERE p.hidden_at IS NULL
        ORDER BY p.created_at DESC, p.id DESC
        "#,
        current_user_id
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| PuzzleMetadata {
            // Postgres has no unsigned integer type: `id`/`downloads` come back as `i32`;
            // `completions`/`likes` come back as `i64` (COUNT's return type) — all four cast to
            // the client-facing `u32` fields explicitly.
            id: row.id as u32,
            short_key: row.short_key,
            likes: row.likes as u32,
            downloads: row.downloads as u32,
            completions: row.completions as u32,
            difficulty: row.difficulty,
            average_time: row.average_time,
            title: row.title,
            author: row.author,
            completed: row.completed, // computed by the LEFT JOIN above, relative to $1
        })
        .collect())
}

/// Newest-first puzzle listing scoped to a single author (D-13: `GET /v1/puzzles/list/mine`).
/// Same projection/mapping/tie-break ordering as `list_new`.
///
/// Visibility predicate: `WHERE p.author_id = $1` carries NO `hidden_at` exclusion whatsoever —
/// this is the deliberate limit case of the visibility rule (D-10 + D-13), not an exception to
/// it: an author always sees their own puzzles, hidden or not, via `mine`. See
/// docs/adr/0002-hidden-by-tri-state.md.
///
/// D-15/ADR 0005: same live-aggregation derived-table join as `list_new` (see that function's doc
/// comment for the full D-14/D-16/D-17/D-20 rationale) — only the visibility predicate differs.
pub async fn list_mine(
    pool: &PgPool,
    current_user_id: Uuid,
) -> Result<Vec<PuzzleMetadata>, AppError> {
    let rows = sqlx::query!(
        r#"
        SELECT p.id, p.short_key, p.downloads, p.title, u.name AS author,
               COALESCE(agg.completions, 0) AS "completions!",
               COALESCE(agg.likes, 0)       AS "likes!",
               agg.average_time,
               CASE WHEN p.downloads = 0 THEN NULL
                    ELSE (COALESCE(agg.completions, 0)::real / p.downloads)::real
               END AS difficulty,
               (pc.user_id IS NOT NULL) AS "completed!"
        FROM puzzles p
        JOIN users u ON u.id = p.author_id
        LEFT JOIN (
            SELECT puzzle_id,
                   COUNT(*)                      AS completions,
                   COUNT(*) FILTER (WHERE liked) AS likes,
                   AVG(time_taken)::real         AS average_time
            FROM puzzle_completions
            GROUP BY puzzle_id
        ) agg ON agg.puzzle_id = p.id
        LEFT JOIN puzzle_completions pc ON pc.puzzle_id = p.id AND pc.user_id = $1
        WHERE p.author_id = $1
        ORDER BY p.created_at DESC, p.id DESC
        "#,
        current_user_id
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| PuzzleMetadata {
            id: row.id as u32,
            short_key: row.short_key,
            likes: row.likes as u32,
            downloads: row.downloads as u32,
            completions: row.completions as u32,
            difficulty: row.difficulty,
            average_time: row.average_time,
            title: row.title,
            author: row.author,
            completed: row.completed,
        })
        .collect())
}

/// Top-rated puzzle listing (`GET /v1/puzzles/list/top-rated`, REQ-business-logic, ROADMAP SC1).
/// Same projection/mapping as `list_new` -- only the `ORDER BY` differs, ranking by the live-
/// aggregated like count first (descending), the live-aggregated completion count second
/// (descending), with the puzzle's own `id` (descending) as a final tie-break -- see the query
/// below for the exact SQL. The trailing id-based tie-break mirrors the `created_at DESC, id DESC`
/// convention already used by `list_new`/`search_puzzles` above -- without it, two puzzles tied on
/// both likes and completions would come back in a non-deterministic order.
///
/// Visibility predicate: `WHERE p.hidden_at IS NULL` unconditionally, same rationale as `list_new`
/// -- `top-rated` is a catalog view, not a direct-access or "mine" view, so it never admits the
/// puzzle's own author to a hidden puzzle either (docs/adr/0002-hidden-by-tri-state.md).
///
/// D-15/ADR 0005: same live-aggregation derived-table join as `list_new` (see that function's doc
/// comment for the full D-14/D-16/D-17/D-20 rationale) -- only the `ORDER BY` differs.
pub async fn list_top_rated(
    pool: &PgPool,
    current_user_id: Uuid,
) -> Result<Vec<PuzzleMetadata>, AppError> {
    let rows = sqlx::query!(
        r#"
        SELECT p.id, p.short_key, p.downloads, p.title, u.name AS author,
               COALESCE(agg.completions, 0) AS "completions!",
               COALESCE(agg.likes, 0)       AS "likes!",
               agg.average_time,
               CASE WHEN p.downloads = 0 THEN NULL
                    ELSE (COALESCE(agg.completions, 0)::real / p.downloads)::real
               END AS difficulty,
               (pc.user_id IS NOT NULL) AS "completed!"
        FROM puzzles p
        JOIN users u ON u.id = p.author_id
        LEFT JOIN (
            SELECT puzzle_id,
                   COUNT(*)                      AS completions,
                   COUNT(*) FILTER (WHERE liked) AS likes,
                   AVG(time_taken)::real         AS average_time
            FROM puzzle_completions
            GROUP BY puzzle_id
        ) agg ON agg.puzzle_id = p.id
        LEFT JOIN puzzle_completions pc ON pc.puzzle_id = p.id AND pc.user_id = $1
        WHERE p.hidden_at IS NULL
        ORDER BY COALESCE(agg.likes, 0) DESC, COALESCE(agg.completions, 0) DESC, p.id DESC
        "#,
        current_user_id
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| PuzzleMetadata {
            id: row.id as u32,
            short_key: row.short_key,
            likes: row.likes as u32,
            downloads: row.downloads as u32,
            completions: row.completions as u32,
            difficulty: row.difficulty,
            average_time: row.average_time,
            title: row.title,
            author: row.author,
            completed: row.completed,
        })
        .collect())
}

/// Increments `puzzles.downloads` by exactly 1 for a successful download (D-19: `downloads`
/// remains the sole stored aggregate -- no download-log table exists to recompute it live, and
/// creating one for this single purpose alone would be disproportionate, see 07-CONTEXT.md
/// Deferred Ideas). The increment is evaluated entirely by PostgreSQL under the row's own lock (see
/// the query below), never by a separate `SELECT` followed by an `UPDATE` on the Rust side -- the
/// latter would lose increments under concurrent downloads of the same puzzle (T-07-06).
///
/// Called exactly once, from `routes::puzzles::download`, after the puzzle has already been
/// resolved successfully -- never from `find_puzzle_by_id`/`find_puzzle_by_short_key` themselves,
/// so that `routes::puzzles::submit` (which calls `find_puzzle_by_id` to read back the puzzle it
/// just created) never counts a phantom download.
pub async fn increment_downloads(pool: &PgPool, puzzle_id: i32) -> Result<i32, AppError> {
    let row = sqlx::query!(
        "UPDATE puzzles SET downloads = downloads + 1 WHERE id = $1 RETURNING downloads",
        puzzle_id
    )
    .fetch_one(pool)
    .await?;

    Ok(row.downloads)
}

/// Title-substring search (`POST /v1/puzzles/search`, D-05/interfaces): filters on `p.title
/// ILIKE '%<search_term>%'` only — `difficulty`/`duration` are validated upstream
/// (`validation::validate_search_filters`) but do not contribute to any `WHERE` clause yet
/// (Phase 6 -> Phase 7 seam documented in `routes::puzzles::search`, since the columns they'd
/// filter on are never populated before Phase 7).
///
/// `search_term` is escaped by the CALLER before this function ever binds it (`\` -> `\\`,
/// `%` -> `\%`, `_` -> `\_`) so a literal `%`/`_` typed by a user is matched literally rather than
/// interpreted as an `ILIKE` wildcard — Postgres's default `LIKE`/`ILIKE` escape character is `\`.
/// This is not an injection concern (the query is parameterized and compile-time checked by
/// `sqlx::query!`); it is purely about match correctness and avoiding a degenerate wildcard
/// pattern.
///
/// Visibility predicate: `WHERE p.hidden_at IS NULL` unconditionally, same rationale as
/// `list_new` — search is a catalog view, not a direct-access or "mine" view.
///
/// D-15/ADR 0005: same live-aggregation derived-table join as `list_new` (see that function's doc
/// comment for the full D-14/D-16/D-17/D-20 rationale) — only the visibility/filter predicate
/// differs.
pub async fn search_puzzles(
    pool: &PgPool,
    current_user_id: Uuid,
    search_term: &str,
) -> Result<Vec<PuzzleMetadata>, AppError> {
    let escaped_term = search_term
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");

    let rows = sqlx::query!(
        r#"
        SELECT p.id, p.short_key, p.downloads, p.title, u.name AS author,
               COALESCE(agg.completions, 0) AS "completions!",
               COALESCE(agg.likes, 0)       AS "likes!",
               agg.average_time,
               CASE WHEN p.downloads = 0 THEN NULL
                    ELSE (COALESCE(agg.completions, 0)::real / p.downloads)::real
               END AS difficulty,
               (pc.user_id IS NOT NULL) AS "completed!"
        FROM puzzles p
        JOIN users u ON u.id = p.author_id
        LEFT JOIN (
            SELECT puzzle_id,
                   COUNT(*)                      AS completions,
                   COUNT(*) FILTER (WHERE liked) AS likes,
                   AVG(time_taken)::real         AS average_time
            FROM puzzle_completions
            GROUP BY puzzle_id
        ) agg ON agg.puzzle_id = p.id
        LEFT JOIN puzzle_completions pc ON pc.puzzle_id = p.id AND pc.user_id = $1
        WHERE p.hidden_at IS NULL AND p.title ILIKE '%' || $2 || '%'
        ORDER BY p.created_at DESC, p.id DESC
        "#,
        current_user_id,
        escaped_term
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| PuzzleMetadata {
            id: row.id as u32,
            short_key: row.short_key,
            likes: row.likes as u32,
            downloads: row.downloads as u32,
            completions: row.completions as u32,
            difficulty: row.difficulty,
            average_time: row.average_time,
            title: row.title,
            author: row.author,
            completed: row.completed,
        })
        .collect())
}

/// Resolves a puzzle by numeric `id`. Does NOT increment `puzzles.downloads` — D-06 defers all
/// counters (likes/downloads/completions) to Phase 7, where they arrive together.
///
/// Visibility predicate: `WHERE (p.hidden_at IS NULL OR p.author_id = $1) AND p.id = $2` — a
/// direct-access lookup (unlike `list_new`/`search_puzzles`'s catalog views) additionally admits a
/// puzzle hidden by ITS OWN author (D-10): the author can still `download` something they hid,
/// anyone else gets the same `not-found` as a nonexistent puzzle. See
/// docs/adr/0002-hidden-by-tri-state.md.
///
/// D-15/ADR 0005: unlike `list_new`/`list_mine`/`search_puzzles`, a single-puzzle lookup uses a
/// direct `LEFT JOIN puzzle_completions pc` grouped by the puzzle's own primary key and the
/// author's name (measured 0.362ms — cheaper here than building the derived table `list_new`
/// uses, since there is only one puzzle to aggregate over). Grouping by `p.id` alone would
/// suffice for every `puzzles` column by functional dependency on the primary key, but `u.name`
/// (a different table) must be listed explicitly in that grouping clause. `completed` can no
/// longer come from a second correlated `LEFT JOIN` under aggregation — it is computed as
/// `COUNT(...) FILTER (WHERE pc.user_id = $1) > 0` instead. See `list_new`'s doc comment for the
/// full D-14/D-16/D-17/D-20 aggregate-formula rationale, unchanged here.
pub async fn find_puzzle_by_id(
    pool: &PgPool,
    current_user_id: Uuid,
    id: i32,
) -> Result<Option<PuzzleFullData>, AppError> {
    let row = sqlx::query!(
        r#"
        SELECT p.id, p.short_key, p.downloads, p.title, u.name AS author,
               p.data as "data: Json<PuzzleGameData>",
               COUNT(pc.id)                         AS "completions!",
               COUNT(pc.id) FILTER (WHERE pc.liked)  AS "likes!",
               AVG(pc.time_taken)::real              AS average_time,
               CASE WHEN p.downloads = 0 THEN NULL
                    ELSE (COUNT(pc.id)::real / p.downloads)::real
               END AS difficulty,
               (COUNT(pc.id) FILTER (WHERE pc.user_id = $1) > 0) AS "completed!"
        FROM puzzles p
        JOIN users u ON u.id = p.author_id
        LEFT JOIN puzzle_completions pc ON pc.puzzle_id = p.id
        WHERE (p.hidden_at IS NULL OR p.author_id = $1) AND p.id = $2
        GROUP BY p.id, u.name
        "#,
        current_user_id,
        id
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| PuzzleFullData {
        meta: PuzzleMetadata {
            id: row.id as u32,
            short_key: row.short_key,
            likes: row.likes as u32,
            downloads: row.downloads as u32, // D-06: not incremented here
            completions: row.completions as u32,
            difficulty: row.difficulty,
            average_time: row.average_time,
            title: row.title,
            author: row.author,
            completed: row.completed,
        },
        game: row.data.0,
    }))
}

/// Creates a new user account. `verified_via` is always caller-supplied (e.g. `"official-api"`
/// from the oracle login flow) — never a client-controlled value at the HTTP layer.
///
/// Does NOT bind `role`: the column's schema default (`'user'`) is the only source of truth for
/// the role of an account created through this function. No public API path may ever write
/// `role` — the first admin account is created exclusively via a seed migration/CLI (plan
/// 05-06), never through `insert_user`. Mirrors `insert_puzzle`'s treatment of `author_id`.
///
/// Does NOT bind `email`, `password_hash` or `steam_id` either: all three are nullable and have
/// no meaning for a Phase 5 oracle-verified account.
///
/// Does NOT pre-check name availability with a `SELECT` before the `INSERT` (would be a TOCTOU
/// race and duplicate logic) — the `UNIQUE(name)` constraint is the single source of truth, same
/// pattern as `puzzles.short_key` in `insert_puzzle`. A unique-violation maps to
/// `AppError::NameTaken`; every other database error maps to `AppError::Database`. D-03 locked:
/// never a silent merge, never a reused account, never an automatic suffix.
pub async fn insert_user(pool: &PgPool, name: &str, verified_via: &str) -> Result<Uuid, AppError> {
    let row = sqlx::query!(
        r#"
        INSERT INTO users (name, verified_via)
        VALUES ($1, $2)
        RETURNING id
        "#,
        name,
        verified_via,
    )
    .fetch_one(pool)
    .await
    .map_err(|err| match &err {
        sqlx::Error::Database(db_err) if db_err.is_unique_violation() => AppError::NameTaken,
        _ => AppError::Database(err),
    })?;

    Ok(row.id)
}

/// D-13: `login` is one of the four routes D-13 names as blocked for a banned account -- this is
/// the login-time ban check that runs before the account even has an authenticated identity.
///
/// D-11: same "currently banned" derivation as `auth::cache::fetch_role_and_ban`
/// (`lifted_at IS NULL` AND `expires_at` is NULL or still in the future) -- several `user_bans`
/// rows may coexist for one user, this only asks whether AT LEAST ONE currently applies.
///
/// Deliberately reads the database directly, never through `AppState.auth_cache`: that cache is
/// keyed by `user_id`, and at this point in `login` no authenticated identity exists yet -- the
/// pseudo is the only link between the request and an existing account. This is also the exact
/// moment identity is established, so the freshest possible read is the correct one (ASVS V2),
/// rather than accepting up to `AUTH_CACHE_TTL` of staleness for the sake of a cache that doesn't
/// even have a key to read yet.
pub async fn is_name_banned(pool: &PgPool, name: &str) -> Result<bool, AppError> {
    let row = sqlx::query!(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM users u
            JOIN user_bans b ON b.user_id = u.id
            WHERE u.name = $1
              AND b.lifted_at IS NULL
              AND (b.expires_at IS NULL OR b.expires_at > now())
        ) AS "banned!"
        "#,
        name
    )
    .fetch_one(pool)
    .await?;

    Ok(row.banned)
}

/// Resolves a puzzle by `short_key`. Same non-incrementing behavior as `find_puzzle_by_id`
/// (D-06) and the SAME visibility predicate/rationale (D-10, docs/adr/0002-hidden-by-tri-state.md)
/// — see that function's doc comment.
///
/// D-15/ADR 0005: same point-lookup grouped-aggregation shape as `find_puzzle_by_id` — see that
/// function's doc comment for the full rationale.
pub async fn find_puzzle_by_short_key(
    pool: &PgPool,
    current_user_id: Uuid,
    short_key: &str,
) -> Result<Option<PuzzleFullData>, AppError> {
    let row = sqlx::query!(
        r#"
        SELECT p.id, p.short_key, p.downloads, p.title, u.name AS author,
               p.data as "data: Json<PuzzleGameData>",
               COUNT(pc.id)                         AS "completions!",
               COUNT(pc.id) FILTER (WHERE pc.liked)  AS "likes!",
               AVG(pc.time_taken)::real              AS average_time,
               CASE WHEN p.downloads = 0 THEN NULL
                    ELSE (COUNT(pc.id)::real / p.downloads)::real
               END AS difficulty,
               (COUNT(pc.id) FILTER (WHERE pc.user_id = $1) > 0) AS "completed!"
        FROM puzzles p
        JOIN users u ON u.id = p.author_id
        LEFT JOIN puzzle_completions pc ON pc.puzzle_id = p.id
        WHERE (p.hidden_at IS NULL OR p.author_id = $1) AND p.short_key = $2
        GROUP BY p.id, u.name
        "#,
        current_user_id,
        short_key
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| PuzzleFullData {
        meta: PuzzleMetadata {
            id: row.id as u32,
            short_key: row.short_key,
            likes: row.likes as u32,
            downloads: row.downloads as u32, // D-06: not incremented here
            completions: row.completions as u32,
            difficulty: row.difficulty,
            average_time: row.average_time,
            title: row.title,
            author: row.author,
            completed: row.completed,
        },
        game: row.data.0,
    }))
}

/// Upserts a puzzle completion (`POST /v1/puzzles/complete/:id`, D-01/D-02). Does NOT touch any
/// aggregate counter -- there is no longer anything to touch: D-15/ADR 0005 made `likes`/
/// `completions`/`average_time`/`difficulty` derived values computed live from `puzzle_completions`
/// at read time (`list_new`/`list_mine`/`search_puzzles`/`find_puzzle_by_id`/
/// `find_puzzle_by_short_key`), not columns maintained by this write path. This function records
/// the completion event and nothing else, on the same "write records the event, reads derive the
/// aggregate" discipline `find_puzzle_by_id`/`find_puzzle_by_short_key` already apply to
/// `downloads` (D-06/D-19).
///
/// D-02 upsert semantics, locked by the SQL below: `time_taken` NEVER regresses (kept at the
/// minimum of the old and new value), `liked` is ALWAYS overwritten by the latest value sent,
/// independently of whether the time improved. Reference scenario confirmed by the user:
/// 60s/`liked=false` then a replay at 90s/`liked=true` => `time_taken=60`, `liked=true`.
///
/// Visibility predicate mirrors `find_puzzle_by_id` (D-10, docs/adr/0002-hidden-by-tri-state.md):
/// a puzzle hidden by a third party is not completable by anyone but its own author -- absent row
/// maps to the same `AppError::NotFound` as a genuinely nonexistent puzzle.
pub async fn upsert_completion(
    pool: &PgPool,
    user_id: Uuid,
    puzzle_id: i32,
    time_taken: f32,
    liked: bool,
) -> Result<(), AppError> {
    let row = sqlx::query!(
        "SELECT author_id FROM puzzles WHERE id = $1 AND (hidden_at IS NULL OR author_id = $2)",
        puzzle_id,
        user_id,
    )
    .fetch_optional(pool)
    .await?;
    if row.is_none() {
        return Err(AppError::NotFound);
    }

    sqlx::query!(
        r#"
        INSERT INTO puzzle_completions (user_id, puzzle_id, time_taken, liked)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (user_id, puzzle_id)
        DO UPDATE SET
            time_taken = LEAST(puzzle_completions.time_taken, EXCLUDED.time_taken),
            liked      = EXCLUDED.liked
        "#,
        user_id,
        puzzle_id,
        time_taken,
        liked,
    )
    .execute(pool)
    .await
    .map_err(|err| match &err {
        // Defends against the race between the visibility check above and this write: mapping a
        // foreign-key violation on `puzzle_id` to `NotFound` rather than a bare 500, same posture
        // as `insert_user`'s constraint-driven error mapping below.
        sqlx::Error::Database(db_err) if db_err.is_foreign_key_violation() => AppError::NotFound,
        _ => AppError::Database(err),
    })?;

    Ok(())
}

/// Inserts a puzzle report (`POST /v1/puzzles/report/:id`, D-03/D-04). Does NOT bind the reports
/// table's pending/upheld/rejected column: the schema `DEFAULT 'pending'` is the sole source of
/// truth for a freshly created report, exactly as `insert_user` never binds `role`.
///
/// Visibility predicate identical to `upsert_completion` above (D-10): a puzzle hidden by a third
/// party cannot be reported by anyone but its own author, who in turn cannot reach the
/// self-report check below because `insert_report` is never called by `routes::puzzles::report`
/// on the reporter's own puzzle in the first place -- see that rejection immediately below.
///
/// D-05 auto-hide (REQ-moderation, 07-RESEARCH.md Pattern 3, verified live by the researcher):
/// after the report `INSERT` commits, a single conditional `UPDATE` re-checks the threshold and
/// applies the hide in the same statement, deliberately WITHOUT an explicit transaction -- D-05
/// locks this shape. This is safe under PostgreSQL's default `READ COMMITTED` isolation: an
/// `UPDATE`'s `WHERE` clause re-evaluates against the latest committed row version after it
/// acquires the row lock, so two reports crossing the threshold concurrently each either win the
/// lock and apply the write, or lose it, re-check `hidden_at IS NULL` (now false), and become a
/// safe no-op -- no lost update, no double-write. A repeat report past the threshold (e.g. a 4th)
/// is likewise idempotent: `hidden_at IS NULL` is already false, zero rows affected.
///
/// ADR 0002 (`docs/adr/0002-hidden-by-tri-state.md`): `hidden_by = NULL` on this path is the
/// convention's "automatic" state -- distinct from an author's own self-hide (`hidden_by` = the
/// author's id, Phase 6 `soft_delete_puzzle`) and a moderator's manual hide (`hidden_by` = the
/// moderator's id, `hide_puzzle` below). Because the guard is `WHERE hidden_at IS NULL` (not
/// `hidden_by IS NULL`), a puzzle the author already self-hid makes this `UPDATE` a no-op and
/// `hidden_by` is NOT overwritten to `NULL` -- deliberate: the puzzle is already hidden either
/// way, and erasing the author's own self-hide reason for no benefit would destroy real
/// information. Proven by `auto_hide_preserves_author_self_hide` in `tests/moderation.rs`.
///
/// This path never writes to `moderation_log`: `moderation_log.moderator_id` is `NOT NULL
/// REFERENCES users(id)`, and an automatic threshold-triggered hide has no moderator to attribute
/// it to -- inventing one would be a fabricated audit trail. The auto-hide's own trace IS
/// `hidden_by = NULL` on `puzzles` (the tri-state itself, per ADR 0002), not a journal row.
pub async fn insert_report(
    pool: &PgPool,
    reporter_id: Uuid,
    puzzle_id: i32,
    reason: &str,
) -> Result<(), AppError> {
    let row = sqlx::query!(
        "SELECT author_id FROM puzzles WHERE id = $1 AND (hidden_at IS NULL OR author_id = $2)",
        puzzle_id,
        reporter_id,
    )
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Err(AppError::NotFound);
    };
    // D-03: self-reporting is rejected before any write is attempted.
    if row.author_id == reporter_id {
        return Err(AppError::CannotReportOwnPuzzle);
    }

    sqlx::query!(
        "INSERT INTO puzzle_reports (user_id, puzzle_id, reason) VALUES ($1, $2, $3)",
        reporter_id,
        puzzle_id,
        reason,
    )
    .execute(pool)
    .await
    .map_err(|err| match &err {
        // D-03: `UNIQUE(user_id, puzzle_id)` is the single source of truth for "already reported
        // by this user" -- no pre-check `SELECT`, same TOCTOU-avoidance discipline as
        // `insert_puzzle`'s `short_key` handling.
        sqlx::Error::Database(db_err) if db_err.is_unique_violation() => AppError::DuplicateReport,
        sqlx::Error::Database(db_err) if db_err.is_foreign_key_violation() => AppError::NotFound,
        _ => AppError::Database(err),
    })?;

    // D-05: see this function's doc comment for the full correctness/idempotence argument. No
    // separate `SELECT COUNT` on the Rust side -- the subquery inside `WHERE` is the locked
    // mechanism, and no explicit transaction wraps this with the `INSERT` above.
    sqlx::query!(
        r#"
        UPDATE puzzles
        SET hidden_at = now(), hidden_by = NULL
        WHERE id = $1
          AND hidden_at IS NULL
          AND (SELECT COUNT(*) FROM puzzle_reports WHERE puzzle_id = $1 AND status = 'pending') >= $2
        "#,
        puzzle_id,
        AUTO_HIDE_REPORT_THRESHOLD,
    )
    .execute(pool)
    .await?;

    Ok(())
}

/// Soft-deletes a puzzle on behalf of its own author (`POST /v1/puzzles/delete/:id`,
/// D-08/D-09/D-11/D-12). Deliberately deviates from `insert_user`'s constraint-only pattern
/// (06-RESEARCH.md "Pattern 4"): authorization here is a three-way branch (not-found / forbidden /
/// success) that no single `UNIQUE`/foreign-key constraint can express, so a `SELECT` before the
/// `UPDATE` is unavoidable and intentional, not an oversight of the TOCTOU-avoidance convention
/// used elsewhere in this file.
///
/// The `SELECT` below carries NO `hidden_at` filter, unlike `upsert_completion`/`insert_report`
/// above: a puzzle already hidden by its own author must still resolve to success on a repeat
/// delete, never `not-found` -- this soft-delete is idempotent for its author (D-11: reversibility
/// remains a fact of data structure only, there is no "undelete" endpoint in Phase 6).
///
/// D-08: only `hidden_at`/`hidden_by` are ever written -- the `puzzles` row itself is NEVER
/// deleted, so there is no cascade to manage on `puzzle_completions`/`puzzle_reports`.
///
/// D-09 + docs/adr/0002-hidden-by-tri-state.md: `hidden_by` is ALWAYS `current_user_id` (the
/// author) on this path, never `NULL` -- `NULL` is reserved exclusively for Phase 7's automatic
/// report-threshold hide, and a moderator's own id is reserved for Phase 7's human moderation
/// action. Writing `NULL` here would collide with a meaning this function does not own.
///
/// D-12: a non-author gets `AppError::NoPermission`, distinct from `AppError::NotFound` for a
/// puzzle that does not exist at all -- the taxonomy must never conflate "doesn't exist" with
/// "exists but isn't yours".
pub async fn soft_delete_puzzle(
    pool: &PgPool,
    puzzle_id: i32,
    current_user_id: Uuid,
) -> Result<(), AppError> {
    let row = sqlx::query!("SELECT author_id FROM puzzles WHERE id = $1", puzzle_id)
        .fetch_optional(pool)
        .await?;
    let Some(row) = row else {
        return Err(AppError::NotFound);
    };
    if row.author_id != current_user_id {
        return Err(AppError::NoPermission);
    }

    sqlx::query!(
        "UPDATE puzzles SET hidden_at = now(), hidden_by = $1 WHERE id = $2",
        current_user_id,
        puzzle_id,
    )
    .execute(pool)
    .await?;

    Ok(())
}
