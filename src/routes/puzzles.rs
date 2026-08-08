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

#[cfg(test)]
mod tests {
    use super::*;

    // Source: compressX64 output from tobspr-games/shapez.io, commit
    // ae88eb48b2834e32fd8c5cf5d91179c6328d78db
    const OFFICIAL_FIXTURE: &str = "N4IgbgpgTgzglgewHYgFwEYA0IBGCCuSAJjGqAO5roAM2AFmgBwC+2O+cANkXEgOalUAbVAAXAJ4AHCGhAQAtnFGjoIbEoWyYdAIbTUAYXxGT+NSEkJBoAB5paIcfexR7zVmKkzUIPgh2c5hryWrr6pqbmltYgdqgArNhOCS5oAJzU7pie0rI4nAgAxgDWUVZksWgATEnVqRiMmcwAutgQNoWc+EQQRABCHNy8AmhCIEbK0ACycFBQCFC9IM3MQA";

    // Source: JSON.stringify shape confirmed against tobspr-games/shapez-community-edition,
    // commit a3fdbf4f594772bbb8b72910987e7a23008fea8f, src/js/platform/api.js:255
    const CE_FIXTURE: &str = r#"{"version":1,"bounds":{"w":10,"h":8},"buildings":[{"type":"emitter","item":"shape:CuCuCuCu","pos":{"x":0,"y":0,"r":0}},{"type":"goal","item":"shape:CuCuCuCu","pos":{"x":5,"y":5,"r":90}},{"type":"block","pos":{"x":2,"y":2,"r":180}}],"excludedBuildings":["CutterMirrored"]}"#;

    #[test]
    fn decodes_authentic_official_compressed_fixture() {
        assert_eq!(OFFICIAL_FIXTURE.len(), 256, "fixture must not be truncated");
        let data = decode_puzzle_data(OFFICIAL_FIXTURE).expect("official fixture must decode");
        assert_eq!(data.bounds.w, 10);
        assert_eq!(data.bounds.h, 8);
        assert_eq!(data.buildings.len(), 3);
        assert_eq!(data.excluded_buildings, vec!["CutterMirrored".to_string()]);
    }

    #[test]
    fn decodes_authentic_ce_raw_json_fixture() {
        let data = decode_puzzle_data(CE_FIXTURE).expect("CE fixture must decode");
        assert_eq!(data.bounds.w, 10);
        assert_eq!(data.bounds.h, 8);
        assert_eq!(data.buildings.len(), 3);
        assert_eq!(data.excluded_buildings, vec!["CutterMirrored".to_string()]);
    }

    #[test]
    fn rejects_oversized_decompressed_payload() {
        // Inflate `excludedBuildings` on an otherwise-valid PuzzleGameData well past
        // MAX_DECOMPRESSED_PUZZLE_DATA_BYTES (D-06/D-07), targeting ~1.05-1.2 MiB decompressed.
        let big_excluded: Vec<String> = (0..90_000).map(|i| format!("Building{i}")).collect();
        let big_data = PuzzleGameData {
            version: 1,
            bounds: Bounds { w: 10, h: 8 },
            buildings: vec![],
            excluded_buildings: big_excluded,
        };
        let big_json = serde_json::to_string(&big_data).expect("serialize big PuzzleGameData");
        assert!(
            big_json.len() > MAX_DECOMPRESSED_PUZZLE_DATA_BYTES,
            "test fixture must actually exceed the size guard"
        );
        // Prove the payload is otherwise well-formed: rejection below must come from the size
        // guard, not from malformed content.
        assert!(serde_json::from_str::<PuzzleGameData>(&big_json).is_ok());

        let compressed = lz_str::compress_to_encoded_uri_component(&big_json[..]);
        assert!(decode_puzzle_data(&compressed).is_err());
    }

    #[test]
    fn rejects_undecodable_payload_without_panic() {
        for input in ["not-valid-lzstring-!!@@##", "", "{}", "N4IgbgpgTgzg"] {
            assert!(
                decode_puzzle_data(input).is_err(),
                "expected input {input:?} to be rejected"
            );
        }
    }
}
