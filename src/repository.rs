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
pub async fn list_new(pool: &PgPool) -> Result<Vec<PuzzleMetadata>, AppError> {
    let rows = sqlx::query!(
        r#"
        SELECT p.id, p.short_key, p.likes, p.downloads, p.completions, p.difficulty,
               p.average_time, p.title, u.name AS author
        FROM puzzles p
        JOIN users u ON u.id = p.author_id
        WHERE p.hidden_at IS NULL
        ORDER BY p.created_at DESC, p.id DESC
        "#
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
            completed: false, // D-07: no current user before Phase 5, always false
        })
        .collect())
}

/// Resolves a puzzle by numeric `id`. Does NOT increment `puzzles.downloads` — D-06 defers all
/// counters (likes/downloads/completions) to Phase 7, where they arrive together.
pub async fn find_puzzle_by_id(pool: &PgPool, id: i32) -> Result<Option<PuzzleFullData>, AppError> {
    let row = sqlx::query!(
        r#"
        SELECT p.id, p.short_key, p.likes, p.downloads, p.completions, p.difficulty,
               p.average_time, p.title, u.name AS author,
               p.data as "data: Json<PuzzleGameData>"
        FROM puzzles p
        JOIN users u ON u.id = p.author_id
        WHERE p.hidden_at IS NULL AND p.id = $1
        "#,
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
            completed: false, // D-07
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
/// (D-06) — see that function's doc comment.
pub async fn find_puzzle_by_short_key(
    pool: &PgPool,
    short_key: &str,
) -> Result<Option<PuzzleFullData>, AppError> {
    let row = sqlx::query!(
        r#"
        SELECT p.id, p.short_key, p.likes, p.downloads, p.completions, p.difficulty,
               p.average_time, p.title, u.name AS author,
               p.data as "data: Json<PuzzleGameData>"
        FROM puzzles p
        JOIN users u ON u.id = p.author_id
        WHERE p.hidden_at IS NULL AND p.short_key = $1
        "#,
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
            completed: false, // D-07
        },
        game: row.data.0,
    }))
}
