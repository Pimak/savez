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

/// `GET /v1/puzzles/list/{category}`. This handler's contract changed in Phase 6, hardening D-05's
/// Phase 3 placeholder: `new` is a real query, newest first (D-04, unchanged). `mine` is now ALSO a
/// real query (D-13) — it reflects the authenticated caller's own puzzles, including ones they have
/// hidden (D-10). `top-rated` (REQ-business-logic, ROADMAP SC1) is now also a real, ranked query
/// (likes descending, then completions descending, see the repository function it delegates to).
/// All three recognized categories are now real queries — no placeholder remains. Any OTHER
/// category value is rejected with the wire code `bad-category`, replacing the Phase 3
/// silent-empty-array fallback: this is a deliberate hardening of D-05, not a copy of its original
/// behavior.
///
/// Stays on `AuthUser`, deliberately not `ActiveUser`: D-13 names four blocked routes and reading
/// is not one of them — a banned account keeps full read access, including to its own `mine` list.
///
/// D-02: `read`-class rate limit, checked as the very first instruction of this handler body
/// (07-07-PLAN.md `<interfaces>`).
pub async fn list(
    State(state): State<AppState>,
    Path(category): Path<String>,
    auth: crate::auth::extractor::AuthUser,
) -> Result<Json<Vec<PuzzleMetadata>>, AppError> {
    crate::ratelimit::check_and_record(&state.pool, auth.user_id, crate::ratelimit::RouteClass::Read)
        .await?;
    match category.as_str() {
        "new" => Ok(Json(repository::list_new(&state.pool, auth.user_id).await?)),
        "mine" => Ok(Json(
            repository::list_mine(&state.pool, auth.user_id).await?,
        )),
        "top-rated" => Ok(Json(
            repository::list_top_rated(&state.pool, auth.user_id).await?,
        )),
        _ => Err(AppError::BadCategory),
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
//
// The profanity-list read immediately below `decode_puzzle_data` is NOT itself a validation step
// in this ordering — it is a resource acquisition (served from an in-memory cache in the common
// case, 07-08-PLAN.md `<interfaces>`) that `validate_title` needs as a parameter to stay pure and
// synchronous. Placing it right before `validate_title` (rather than at the top of the handler)
// means a request that fails at `decode_puzzle_data` never even triggers this read.
//
// D-13: one of the four routes blocked for a banned account. `ActiveUser` rejects with
// `AuthRejection::Banned` (wire code `banned`) before this handler body ever runs — a banned
// caller never even reaches `decode_puzzle_data`.
//
// D-02: `write`-class rate limit, checked as the FIRST instruction of this handler body, strictly
// before `decode_puzzle_data` (07-07-PLAN.md `<interfaces>`) — lz-string decompression is the
// single most expensive step in this whole phase, and a request already past its quota must never
// consume it.
pub async fn submit(
    State(state): State<AppState>,
    auth: crate::auth::extractor::ActiveUser,
    Json(payload): Json<SubmitPuzzlePayload>,
) -> Result<Json<PuzzleMetadata>, AppError> {
    crate::ratelimit::check_and_record(&state.pool, auth.0.user_id, crate::ratelimit::RouteClass::Write)
        .await?;
    let data = decode_puzzle_data(&payload.data)?;
    let profanity = state.profanity_cache.get(&state.pool).await?;
    let title = validation::validate_title(&payload.title, &profanity)?;
    let short_key = validation::validate_short_key(&payload.short_key)?;
    validation::validate_game_data(&data)?;

    let request = SubmitPuzzleRequest {
        title,
        short_key,
        data,
    };
    let id = repository::insert_puzzle(&state.pool, &request, auth.0.user_id).await?;
    // `is_moderator = false`: this reads back a puzzle the caller just created for themselves --
    // whether the caller happens to also be a moderator is irrelevant to this read (07-05-PLAN.md
    // `<interfaces>`).
    let full = repository::find_puzzle_by_id(&state.pool, auth.0.user_id, id, false)
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
/// happens to share that numeric `id`. `completed` is computed relative to the authenticated
/// caller (SC4), and D-05/D-10 both require a token: a puzzle hidden by a third party resolves to
/// `not-found` for anyone but its own author — resolution failure (either lookup path) returns
/// before the download counter below is ever touched.
///
/// D-19: exactly one successful resolution, by either path above, bumps the stored download
/// counter exactly once via the repository — never once per branch. `submit` deliberately does NOT
/// go through this handler when it reads back the puzzle it just created (it calls
/// `find_puzzle_by_id` directly instead), so a fresh submission is never counted as a download.
/// The `downloads` value on the response below comes from that repository call's own `RETURNING`,
/// so the response reflects the download that just happened. `difficulty` on that same response is
/// computed from the PRE-increment `downloads` (it was already read by the resolution query above)
/// — a one-count lag with no functional consequence, self-corrects on the very next read, and is
/// documented here rather than "fixed" with a second, redundant read.
///
/// Stays on `AuthUser`: reading is always allowed for a banned account (D-13 names only four
/// blocked routes, and this isn't one of them).
///
/// `is_moderator` (ROADMAP SC2, 07-05-PLAN.md `<interfaces>`): derived from the cached role
/// (at least the moderator threshold, cumulative per `auth::cache::Role`'s `Ord`), never from a
/// client-supplied value -- a moderator can download any puzzle direct-access, including one
/// hidden by a third party, so review/audit work is never blocked by the same visibility rule
/// that protects ordinary users.
///
/// D-02: `read`-class rate limit, checked as the very first instruction of this handler body.
pub async fn download(
    State(state): State<AppState>,
    Path(id_or_key): Path<String>,
    auth: crate::auth::extractor::AuthUser,
) -> Result<Json<PuzzleFullData>, AppError> {
    crate::ratelimit::check_and_record(&state.pool, auth.user_id, crate::ratelimit::RouteClass::Read)
        .await?;
    let is_moderator = auth.role >= crate::auth::cache::Role::Moderator;
    let mut full = match repository::find_puzzle_by_short_key(
        &state.pool,
        auth.user_id,
        &id_or_key,
        is_moderator,
    )
    .await?
    {
        Some(full) => full,
        None => {
            let by_id = match id_or_key.parse::<i32>() {
                Ok(id) => {
                    repository::find_puzzle_by_id(&state.pool, auth.user_id, id, is_moderator)
                        .await?
                }
                Err(_) => None,
            };
            by_id.ok_or(AppError::NotFound)?
        }
    };

    full.meta.downloads =
        repository::increment_downloads(&state.pool, full.meta.id as i32).await? as u32;

    Ok(Json(full))
}

/// Wire body of `POST /v1/puzzles/search` (`interfaces` in 06-04-PLAN.md, `api.js:142-154`).
/// Every field carries a `#[serde(default ...)]`: a partial body must behave as an unfiltered
/// search, not as a 4xx JSON-deserialization error — axum's automatic rejection on missing fields
/// would produce a status code outside the project's all-200 taxonomy
/// (DEC-api-contract-conventions), which this project never allows for a well-formed but partial
/// request body.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest {
    #[serde(default)]
    pub search_term: String,
    #[serde(default = "default_any_filter")]
    pub difficulty: String,
    #[serde(default = "default_any_filter")]
    pub duration: String,
}

fn default_any_filter() -> String {
    "any".to_string()
}

/// `POST /v1/puzzles/search`. Requires `x-token` (D-05/D-06) — see `list`/`download` above for the
/// same requirement. `difficulty`/`duration` are validated but currently inert: `difficulty`/
/// `average_time` have been live-computed values since 07-01 (D-14/D-16, ADR 0005), so this is no
/// longer a data-availability gap — wiring these two filters into a `WHERE` clause is simply out of
/// this plan's scope (07-02-PLAN.md `<interfaces>` "Hors périmètre"): neither REQ-business-logic
/// nor the ROADMAP asks for it, and no D-01..D-20 decision covers it. Only `search_term` filters
/// the result set today.
///
/// Stays on `AuthUser`: reading is always allowed for a banned account (D-13 names only four
/// blocked routes, and this isn't one of them).
///
/// D-02: `read`-class rate limit, checked as the very first instruction of this handler body.
pub async fn search(
    State(state): State<AppState>,
    auth: crate::auth::extractor::AuthUser,
    Json(payload): Json<SearchRequest>,
) -> Result<Json<Vec<PuzzleMetadata>>, AppError> {
    crate::ratelimit::check_and_record(&state.pool, auth.user_id, crate::ratelimit::RouteClass::Read)
        .await?;
    validation::validate_search_filters(
        &payload.search_term,
        &payload.difficulty,
        &payload.duration,
    )?;
    Ok(Json(
        repository::search_puzzles(&state.pool, auth.user_id, payload.search_term.trim()).await?,
    ))
}

/// `complete/:id`, `report/:id`, `delete/:id` all take a numeric `:id` path segment. This resolves
/// it manually via a plain string extractor rather than declaring a numeric path type on the
/// handler signature: axum's native rejection on a non-numeric segment for that numeric type
/// produces a bare 400 outside this project's all-200 taxonomy (DEC-api-contract-conventions),
/// which every other rejection in this codebase avoids by construction. `download`'s
/// `id_or_key.parse::<i32>()` is the existing analog this mirrors, just factored out since three
/// handlers need it here instead of one.
///
/// `pub(crate)` (07-10-PLAN.md `<interfaces>`): `routes::moderation`'s `resolve_report`/`hide`/
/// `unhide`/`purge`/`lift_ban` handlers reuse this exact function for their own numeric path
/// segments rather than redefining an equivalent.
pub(crate) fn parse_puzzle_id(raw: &str) -> Result<i32, AppError> {
    raw.trim().parse::<i32>().map_err(|_| AppError::BadId)
}

/// Wire body of `POST /v1/puzzles/complete/:id` (`api.js:206-221`). Both fields carry
/// `#[serde(default)]` so a partial/missing body is rejected by VALIDATION inside the handler
/// (a taxonomy code) rather than by axum's JSON deserialization (a bare, non-taxonomy status) —
/// same discipline as `SearchRequest` above. `time`'s absence defaults to `0.0`, which the
/// handler's finiteness/positivity guard below rejects with `bad-payload`; `liked`'s absence
/// defaults to `false`, a value the client is free to send legitimately, so no additional guard is
/// needed on that field.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteRequest {
    #[serde(default)]
    pub time: f32,
    #[serde(default)]
    pub liked: bool,
}

/// Wire body of `POST /v1/puzzles/report/:id` (`api.js:192-204`). `#[serde(default)]` on `reason`
/// means a missing field arrives as `""`, which `validation::validate_report_reason` rejects with
/// `bad-payload` — never an axum-level deserialization error outside the taxonomy.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportRequest {
    #[serde(default)]
    pub reason: String,
}

/// `POST /v1/puzzles/complete/:id` (D-01/D-02). Rejects a non-finite or non-positive `time` before
/// ever reaching the repository: `puzzle_completions.time_taken` is `REAL NOT NULL`, and a
/// `NaN`/`Infinity`/zero/negative value would otherwise be persisted silently, corrupting D-02's
/// "never regresses" guarantee (a `NaN` compares false to everything, defeating `LEAST`) and any
/// future Phase 7 aggregate built on this column.
///
/// D-13: one of the four routes blocked for a banned account. `ActiveUser` rejects with
/// `AuthRejection::Banned` (wire code `banned`) before this handler body ever runs.
///
/// D-02: `write`-class rate limit, checked as the very first instruction of this handler body.
pub async fn complete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    auth: crate::auth::extractor::ActiveUser,
    Json(payload): Json<CompleteRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::ratelimit::check_and_record(&state.pool, auth.0.user_id, crate::ratelimit::RouteClass::Write)
        .await?;
    let id = parse_puzzle_id(&id)?;
    if !payload.time.is_finite() || payload.time <= 0.0 {
        return Err(AppError::BadPayload);
    }
    repository::upsert_completion(&state.pool, auth.0.user_id, id, payload.time, payload.liked)
        .await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// `POST /v1/puzzles/report/:id` (D-03/D-04). `reason` is validated against the strict enum before
/// any repository call — a malformed reason must never consume a database round-trip.
///
/// D-13: one of the four routes blocked for a banned account. `ActiveUser` rejects with
/// `AuthRejection::Banned` (wire code `banned`) before this handler body ever runs.
///
/// D-02: `write`-class rate limit, checked as the very first instruction of this handler body.
pub async fn report(
    State(state): State<AppState>,
    Path(id): Path<String>,
    auth: crate::auth::extractor::ActiveUser,
    Json(payload): Json<ReportRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::ratelimit::check_and_record(&state.pool, auth.0.user_id, crate::ratelimit::RouteClass::Write)
        .await?;
    let id = parse_puzzle_id(&id)?;
    validation::validate_report_reason(&payload.reason)?;
    repository::insert_report(&state.pool, auth.0.user_id, id, &payload.reason).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// `POST /v1/puzzles/delete/:id` (D-08/D-09/D-11/D-12) — POST, never DELETE (the real client's
/// literal method choice, `api.js:171-179`). Deliberately declares NO `Json` extractor: the client
/// sends a literal `{}` body, and an empty or entirely absent body must not fail the request either
/// way. Authorization is fully server-side: `auth.user_id` (from the verified JWT) is the only
/// input to "am I the author?", never anything the client could assert about itself. A non-author
/// gets `no-permission`, distinct from `not-found` for a puzzle that does not exist (D-12) — see
/// `repository::soft_delete_puzzle` for the full three-way branch and its idempotence guarantee for
/// a repeat delete by the same author.
///
/// Deliberately stays on `AuthUser`, NOT `ActiveUser` — this is a decision, not an oversight a
/// future reviewer should "fix". D-13 and SPEC §4.6 both name exactly four routes blocked for a
/// banned account (login, submit, complete, report); `delete` is not among them in either source.
/// Removing one's own content is not an act of harm, and a banned author must still be able to do
/// it — blocking `delete` too would be a scope expansion this plan has no mandate to make.
///
/// D-02: `write`-class rate limit, checked as the very first instruction of this handler body —
/// `delete` is a write action per the route→class table in `src/ratelimit.rs`, independent of its
/// ban-blocking status above (D-02 and D-13 are separate axes).
pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    auth: crate::auth::extractor::AuthUser,
) -> Result<Json<serde_json::Value>, AppError> {
    crate::ratelimit::check_and_record(&state.pool, auth.user_id, crate::ratelimit::RouteClass::Write)
        .await?;
    let id = parse_puzzle_id(&id)?;
    repository::soft_delete_puzzle(&state.pool, id, auth.user_id).await?;
    Ok(Json(serde_json::json!({ "success": true })))
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
