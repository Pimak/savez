use sqlx::PgPool;
use sqlx::types::Json;
use uuid::Uuid;

use crate::error::AppError;
use crate::routes::puzzles::{PuzzleFullData, PuzzleGameData, PuzzleMetadata, SubmitPuzzleRequest};

// TODO(Phase 5): retirer dès que l'auth JWT fournit un author_id réel — voir
// migrations/20260807000002_seed_dev_author.sql, dont la ligne seed porte cet UUID littéral.
// Les deux valeurs doivent rester identiques byte-for-byte.
pub const DEV_SEED_AUTHOR_ID: Uuid = uuid::uuid!("00000000-0000-0000-0000-000000000001");

/// Inserts a new puzzle row. `author_id` is ALWAYS `DEV_SEED_AUTHOR_ID` — never a value taken
/// from `req` (there is none to take: `SubmitPuzzleRequest` has no author-shaped field, and even
/// if it did this function would still ignore it, per D-01/D-02).
pub async fn insert_puzzle(pool: &PgPool, req: &SubmitPuzzleRequest) -> Result<i32, AppError> {
    let row = sqlx::query!(
        r#"
        INSERT INTO puzzles (short_key, title, author_id, data)
        VALUES ($1, $2, $3, $4)
        RETURNING id
        "#,
        req.short_key,
        req.title,
        DEV_SEED_AUTHOR_ID,
        Json(&req.data) as Json<&PuzzleGameData>,
    )
    .fetch_one(pool)
    .await?;

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
