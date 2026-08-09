use sqlx::PgPool;
use sqlx::types::Json;
use uuid::Uuid;

use crate::error::AppError;
use crate::routes::puzzles::{PuzzleFullData, PuzzleGameData, PuzzleMetadata, SubmitPuzzleRequest};

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
pub async fn list_new(
    pool: &PgPool,
    current_user_id: Uuid,
) -> Result<Vec<PuzzleMetadata>, AppError> {
    let rows = sqlx::query!(
        r#"
        SELECT p.id, p.short_key, p.likes, p.downloads, p.completions, p.difficulty,
               p.average_time, p.title, u.name AS author,
               (pc.user_id IS NOT NULL) AS "completed!"
        FROM puzzles p
        JOIN users u ON u.id = p.author_id
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
            // Postgres has no unsigned integer type: `id`/`likes`/`downloads`/`completions` come
            // back as `i32` and must be cast to the client-facing `u32` fields explicitly.
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
pub async fn list_mine(
    pool: &PgPool,
    current_user_id: Uuid,
) -> Result<Vec<PuzzleMetadata>, AppError> {
    let rows = sqlx::query!(
        r#"
        SELECT p.id, p.short_key, p.likes, p.downloads, p.completions, p.difficulty,
               p.average_time, p.title, u.name AS author,
               (pc.user_id IS NOT NULL) AS "completed!"
        FROM puzzles p
        JOIN users u ON u.id = p.author_id
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
        SELECT p.id, p.short_key, p.likes, p.downloads, p.completions, p.difficulty,
               p.average_time, p.title, u.name AS author,
               (pc.user_id IS NOT NULL) AS "completed!"
        FROM puzzles p
        JOIN users u ON u.id = p.author_id
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
pub async fn find_puzzle_by_id(
    pool: &PgPool,
    current_user_id: Uuid,
    id: i32,
) -> Result<Option<PuzzleFullData>, AppError> {
    let row = sqlx::query!(
        r#"
        SELECT p.id, p.short_key, p.likes, p.downloads, p.completions, p.difficulty,
               p.average_time, p.title, u.name AS author,
               p.data as "data: Json<PuzzleGameData>",
               (pc.user_id IS NOT NULL) AS "completed!"
        FROM puzzles p
        JOIN users u ON u.id = p.author_id
        LEFT JOIN puzzle_completions pc ON pc.puzzle_id = p.id AND pc.user_id = $1
        WHERE (p.hidden_at IS NULL OR p.author_id = $1) AND p.id = $2
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

/// Resolves a puzzle by `short_key`. Same non-incrementing behavior as `find_puzzle_by_id`
/// (D-06) and the SAME visibility predicate/rationale (D-10, docs/adr/0002-hidden-by-tri-state.md)
/// — see that function's doc comment.
pub async fn find_puzzle_by_short_key(
    pool: &PgPool,
    current_user_id: Uuid,
    short_key: &str,
) -> Result<Option<PuzzleFullData>, AppError> {
    let row = sqlx::query!(
        r#"
        SELECT p.id, p.short_key, p.likes, p.downloads, p.completions, p.difficulty,
               p.average_time, p.title, u.name AS author,
               p.data as "data: Json<PuzzleGameData>",
               (pc.user_id IS NOT NULL) AS "completed!"
        FROM puzzles p
        JOIN users u ON u.id = p.author_id
        LEFT JOIN puzzle_completions pc ON pc.puzzle_id = p.id AND pc.user_id = $1
        WHERE (p.hidden_at IS NULL OR p.author_id = $1) AND p.short_key = $2
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
