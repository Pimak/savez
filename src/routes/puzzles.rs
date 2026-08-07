use axum::Json;
use serde::{Deserialize, Serialize};

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

pub async fn list_new() -> Json<Vec<PuzzleMetadata>> {
    Json(vec![]) // hardcoded content per REQ-http-skeleton; real data arrives in Phase 3
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
