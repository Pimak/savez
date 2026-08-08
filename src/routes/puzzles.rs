use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};

use crate::db::AppState;
use crate::error::AppError;
use crate::repository;

/// Field names/casing verified byte-for-byte against `tobspr-games/shapez.io`'s
/// `src/js/savegame/savegame_typedefs.js`.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PuzzleMetadata {
    pub id: u32,
    pub short_key: String,
    pub likes: u32,
    pub downloads: u32,
    pub completions: u32,
    pub difficulty: Option<f32>,
    pub average_time: Option<f32>,
    pub title: String,
    pub author: String,
    pub completed: bool,
}

/// `GET /v1/puzzles/list/{category}`. Only `new` is backed by a real query (D-04): it reflects
/// the actual submitted puzzles, newest first. `top-rated` and `mine` return `200 []` rather than
/// an error or a copy of `new` (D-05) — ranking by likes arrives in Phase 7, and `mine` needs a
/// current user that doesn't exist before Phase 5. Any other category value also falls back to an
/// empty array in 200, per the project's all-200 convention (DEC-api-contract-conventions): an
/// unknown category is never a 404.
pub async fn list(
    State(state): State<AppState>,
    Path(category): Path<String>,
) -> Result<Json<Vec<PuzzleMetadata>>, AppError> {
    match category.as_str() {
        "new" => Ok(Json(repository::list_new(&state.pool).await?)),
        // D-05: top-rated (sort by likes) and mine (current-user scope) are not implementable
        // before Phase 7/Phase 5 respectively; an empty list is the contractually-correct
        // placeholder, never an error and never `new`'s content.
        _ => Ok(Json(Vec::new())),
    }
}

/// `x`/`y`/`r` in the JS typedef (`savegame_typedefs.js`) are only annotated `number`. Treated as
/// `i32` here: grid coordinates and building rotation are integral in shapez's placement model
/// (RESEARCH.md Assumptions Log A1).
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Pos {
    pub x: i32,
    pub y: i32,
    pub r: i32,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Bounds {
    pub w: u32,
    pub h: u32,
}

/// Internally-tagged enum mirroring the JS discriminated union
/// (`PuzzleGameBuildingConstantProducer | PuzzleGameBuildingGoal | PuzzleGameBuildingBlock`)
/// exactly: `type` carries the discriminant, `item` is present on `emitter`/`goal` only.
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PuzzleGameBuilding {
    Emitter { item: String, pos: Pos },
    Goal { item: String, pos: Pos },
    Block { pos: Pos },
}

// `Debug` is required by `sqlx::query!`'s generated row struct wherever `PuzzleGameData` is
// bound as a `Json<PuzzleGameData>` column (see `src/repository.rs::find_puzzle_by_id` /
// `find_puzzle_by_short_key`) — sqlx 0.9's macro-generated row types unconditionally derive
// `Debug`, which requires every field type (transitively: `Bounds`, `Pos`, `PuzzleGameBuilding`)
// to implement it too.
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PuzzleGameData {
    pub version: u32,
    pub bounds: Bounds,
    pub buildings: Vec<PuzzleGameBuilding>,
    pub excluded_buildings: Vec<String>,
}

/// Field names `meta`/`game` are literal (no camelCase renaming needed — both are already
/// lower-case single words matching the JS typedef `PuzzleFullData`).
#[derive(Serialize, Deserialize)]
pub struct PuzzleFullData {
    pub meta: PuzzleMetadata,
    pub game: PuzzleGameData,
}

/// Body of `POST /v1/puzzles/submit`. Deliberately has NO `author`/`authorId` field: the author
/// is always the server-side seeded dev user in Phase 3 (D-02) and will be the JWT-authenticated
/// user from Phase 5 onward — never a client-supplied value (threat T-03-11). Do not add an
/// author-shaped field here "for convenience"; the server always determines authorship itself.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitPuzzleRequest {
    pub title: String,
    pub short_key: String,
    pub data: PuzzleGameData,
}

// TODO(Phase 6): the exact submission response shape (and the `T.backendErrors` error codes) is
// part of the full ClientAPI contract (DEC-api-contract-conventions), delivered in Phase 6
// (REQ-shapez-contract-complete). In Phase 3 the created `PuzzleMetadata` is returned as-is.
//
// `payload` never carries an author: `SubmitPuzzleRequest` has no author-shaped field, and serde
// silently drops unknown JSON keys, so a client-supplied `author`/`authorId` is structurally
// without effect (T-03-21) — `repository::insert_puzzle` always writes `DEV_SEED_AUTHOR_ID`.
pub async fn submit(
    State(state): State<AppState>,
    Json(payload): Json<SubmitPuzzleRequest>,
) -> Result<Json<PuzzleMetadata>, AppError> {
    let id = repository::insert_puzzle(&state.pool, &payload).await?;
    let full = repository::find_puzzle_by_id(&state.pool, id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(full.meta))
}

/// Resolves `id_or_key` as a `short_key` first (an exact match against the UNIQUE column is
/// unambiguous regardless of whether the string happens to look numeric), falling back to a
/// numeric `id` lookup only when no puzzle has that exact `short_key`. This ordering matters: a
/// client-chosen `short_key` is not guaranteed to be non-numeric (structural validation of
/// submitted `shortKey` values is deferred to Phase 6 — CONTEXT.md Deferred Ideas), so an
/// id-first lookup could silently resolve an all-digit `short_key` to an unrelated puzzle that
/// happens to share that numeric `id`. Does NOT increment `downloads` (D-06) nor compute
/// `completed` (D-07, stays `false` — no current user exists before Phase 5).
pub async fn download(
    State(state): State<AppState>,
    Path(id_or_key): Path<String>,
) -> Result<Json<PuzzleFullData>, AppError> {
    if let Some(full) = repository::find_puzzle_by_short_key(&state.pool, &id_or_key).await? {
        return Ok(Json(full));
    }
    let full = match id_or_key.parse::<i32>() {
        Ok(id) => repository::find_puzzle_by_id(&state.pool, id).await?,
        Err(_) => None,
    };
    full.map(Json).ok_or(AppError::NotFound)
}
