use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};

use crate::db::AppState;
use crate::error::AppError;
use crate::repository;
use crate::validation;

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

/// Internal type constructed AFTER `decode_puzzle_data` succeeds (D-08) — no longer the wire body
/// of `POST /v1/puzzles/submit` (see `SubmitPuzzlePayload` for that). Deliberately has NO
/// `author`/`authorId` field: the author is always the JWT-authenticated user (`AuthUser.user_id`,
/// Phase 5) — never a client-supplied value (threat T-03-11/T-05-05). Do not add an author-shaped
/// field here "for convenience"; the server always determines authorship itself.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitPuzzleRequest {
    pub title: String,
    pub short_key: String,
    pub data: PuzzleGameData,
}

/// Wire body of `POST /v1/puzzles/submit` (D-08, confirmed against real client source, not just
/// inferred from the CE FIXME comment): `data` arrives as a `String`, never a nested object — the
/// official client sends `compressX64(JSON.stringify(payload.data))`
/// (`tobspr-games/shapez.io`), the Community Edition client sends `JSON.stringify(payload.data)`
/// (`tobspr-games/shapez-community-edition`, its own FIXME notwithstanding). `decode_puzzle_data`
/// detects which of the two formats a given submission used.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitPuzzlePayload {
    pub title: String,
    pub short_key: String,
    pub data: String,
}

/// D-06/D-07: a real puzzle's decompressed `data` is a few dozen KiB; this is a wide safety
/// margin against a small compressed input expanding into a huge string (decompression bomb,
/// T-04-01) before any JSON parsing is attempted. Applied uniformly to BOTH wire formats (04-REVIEW.md
/// WR-01): on the Community Edition path `raw` IS the uncompressed payload, so this bounds it
/// directly; on the official-client path it bounds the post-decompression result as a second,
/// redundant check (the binding constraint there is MAX_COMPRESSED_PUZZLE_DATA_BYTES below).
const MAX_DECOMPRESSED_PUZZLE_DATA_BYTES: usize = 1_048_576; // ~1 MiB

/// 04-REVIEW.md CR-01: `lz_str::decompress_from_encoded_uri_component` has no incremental/bounded
/// output API — it fully materializes its result in memory before this function ever sees it, and
/// its LZW-family algorithm has no built-in cap on that output size (a small compressed input can
/// expand at up to O(N^2) in the number of decoded codes). MAX_DECOMPRESSED_PUZZLE_DATA_BYTES
/// alone cannot defend against this because by the time it runs the damage (unbounded allocation)
/// is already done. This bounds the *compressed* input instead, before decompression is ever
/// attempted. Real compressX64 payloads for a puzzle are a few KiB; 64 KiB is a wide margin.
const MAX_COMPRESSED_PUZZLE_DATA_BYTES: usize = 65_536; // 64 KiB

/// Detects and decodes either wire format of `data` (D-08/DEC-dual-submit-format): tries a direct
/// JSON parse first (Community Edition's actual behavior), then falls back to lz-string
/// (`compressX64`/EncodedURIComponent) decompression (the official client's actual behavior).
/// Neither `.unwrap()` nor `.expect()` is used on the fallible steps below (T-04-02, Pitfall 3
/// in RESEARCH.md): a crafted malformed input must never panic the request handler.
fn decode_puzzle_data(raw: &str) -> Result<PuzzleGameData, AppError> {
    // D-06/D-07 + WR-01: reject an oversized raw payload before attempting EITHER format. On the
    // Community Edition path this directly enforces the ~1 MiB ceiling (raw IS the uncompressed
    // JSON here); on the official-client path a compressed payload this large is already far
    // beyond any real puzzle and is rejected before the far tighter compressed-length check below.
    if raw.len() > MAX_DECOMPRESSED_PUZZLE_DATA_BYTES {
        return Err(AppError::InvalidPuzzleData);
    }

    // D-08: Community Edition sends raw, uncompressed JSON.
    if let Ok(data) = serde_json::from_str::<PuzzleGameData>(raw) {
        return Ok(data);
    }

    // CR-01: reject a clearly-oversized *compressed* input BEFORE calling
    // decompress_from_encoded_uri_component — the check must run before decompression is
    // attempted, not after its result is materialized, since that materialization is itself the
    // unbounded-memory-allocation risk this guard exists to prevent.
    if raw.len() > MAX_COMPRESSED_PUZZLE_DATA_BYTES {
        return Err(AppError::InvalidPuzzleData);
    }

    // D-08: the official client sends `compressX64` (lz-string, EncodedURIComponent variant).
    // D-04/D-05: an input that decodes as neither format fails here — never a partial decode,
    // never a 200.
    let units =
        lz_str::decompress_from_encoded_uri_component(raw).ok_or(AppError::InvalidPuzzleData)?;
    // T-04-02/Pitfall 3: a dangling UTF-16 surrogate fails here (`Result::Err`), not above
    // (`Option::None`) — both paths must be handled, never `.unwrap()`.
    let json = String::from_utf16(&units).map_err(|_| AppError::InvalidPuzzleData)?;

    // D-06/D-07: redundant with the compressed-length check above (which is the actual binding
    // constraint here since MAX_COMPRESSED_PUZZLE_DATA_BYTES < MAX_DECOMPRESSED_PUZZLE_DATA_BYTES),
    // kept as defense in depth per the literal D-06 wording ("checked before JSON parsing").
    if json.len() > MAX_DECOMPRESSED_PUZZLE_DATA_BYTES {
        return Err(AppError::InvalidPuzzleData);
    }

    serde_json::from_str(&json).map_err(|_| AppError::InvalidPuzzleData)
}

// The submission response shape is the created `PuzzleMetadata`, unchanged since Phase 3: the
// client only inspects the presence of a top-level `error` key (`ClientAPI._request()`), it does
// not otherwise special-case a successful `submit` body. Validation of the submission itself is
// now complete (REQ-shapez-contract-complete): every business rule below runs before any write.
//
// `payload` never carries an effective author: `SubmitPuzzleRequest` has no author-shaped field,
// and serde silently drops unknown JSON keys, so a client-supplied `author`/`authorId` is
// structurally without effect (T-03-21/T-05-05) — the author is always `auth.user_id`, taken from
// the server-verified JWT injected by the `AuthUser` extractor, never anything the client sends.
//
// Validation order below is mandatory and not permutable, on the same model as `login()`
// (`routes/auth.rs`) — it decides which single error code comes back when a submission breaks
// several rules at once: decode, then title, then short key, then game data, and ONLY THEN the
// database round-trip for uniqueness. Title/short key/game-data checks are pure and cheap; short
// key uniqueness is the one rule that needs a database round-trip, so it must run last — a
// malformed short key must never consume a SQL query.
pub async fn submit(
    State(state): State<AppState>,
    auth: crate::auth::extractor::AuthUser,
    Json(payload): Json<SubmitPuzzlePayload>,
) -> Result<Json<PuzzleMetadata>, AppError> {
    let data = decode_puzzle_data(&payload.data)?;
    let title = validation::validate_title(&payload.title)?;
    let short_key = validation::validate_short_key(&payload.short_key)?;
    validation::validate_game_data(&data)?;

    let request = SubmitPuzzleRequest {
        title,
        short_key,
        data,
    };
    let id = repository::insert_puzzle(&state.pool, &request, auth.user_id).await?;
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

    // The decompressed payload below is written by this project, not by any real client; only the
    // ENCODING is authentic: this is `compressX64` (lz-string, `EncodedURIComponent` variant)
    // applied to the canonical payload also used verbatim as `CE_FIXTURE`. This test exercises
    // `decode_puzzle_data`'s compressed-input branch, not the content's provenance.
    const OFFICIAL_FIXTURE: &str = "N4IgbgpgTgzglgewHYgFwEYA0IBGCCuSAJjGqAO5roAM2AFmgBwC+2O+cANkXEgOalUAbVAAXAJ4AHCGhAQAtnFGjoIbEoWyAwvh178akJISDQADzS0Q4y9iiXmrMVJmoQfBAENOhjfO26gTqGxqYgFqgALNg2qADMdmgAnNSOmM7SsjicCADGANYhJmThaABMMeWJGIypzAC62BBmuZz4RBBEAEIc3LwCaEIgOsrQALJwUFAIUJ0g9cxAA";

    // Source: JSON.stringify shape confirmed against tobspr-games/shapez-community-edition,
    // commit a3fdbf4f594772bbb8b72910987e7a23008fea8f, src/js/platform/api.js:255
    const CE_FIXTURE: &str = r#"{"version":1,"bounds":{"w":10,"h":8},"buildings":[{"type":"emitter","item":"CuCuCuCu","pos":{"x":0,"y":0,"r":0}},{"type":"goal","item":"CuCuCuCu","pos":{"x":4,"y":3,"r":90}},{"type":"block","pos":{"x":2,"y":2,"r":180}}],"excludedBuildings":["CutterMirrored"]}"#;

    #[test]
    fn decodes_authentic_official_compressed_fixture() {
        assert_eq!(OFFICIAL_FIXTURE.len(), 251, "fixture must not be truncated");
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
    fn rejects_oversized_compressed_input_before_decompression() {
        // 04-REVIEW.md CR-01 regression: a compressed-looking input larger than
        // MAX_COMPRESSED_PUZZLE_DATA_BYTES must be rejected without ever reaching
        // lz_str::decompress_from_encoded_uri_component — real compressX64 output for a puzzle is
        // a few KiB, so this only ever rejects payloads far outside legitimate use.
        let oversized_compressed_looking: String = "A".repeat(MAX_COMPRESSED_PUZZLE_DATA_BYTES + 1);
        assert!(oversized_compressed_looking.len() > MAX_COMPRESSED_PUZZLE_DATA_BYTES);
        assert!(decode_puzzle_data(&oversized_compressed_looking).is_err());
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
