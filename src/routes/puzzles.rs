use axum::Json;
use serde::Serialize;

/// Field names/casing verified byte-for-byte against `tobspr-games/shapez.io`'s
/// `src/js/savegame/savegame_typedefs.js`.
#[derive(Serialize)]
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
