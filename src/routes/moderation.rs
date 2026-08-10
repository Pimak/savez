//! `/v1/moderation/*` HTTP routes (SPEC `docs/cahier-des-charges.md` §4.6, ROADMAP SC3,
//! REQ-moderation). The route table below is treated as canonical, copied verbatim into this
//! plan's `<interfaces>` from the SPEC — not invented here.
//!
//! These eight routes sit outside the shapez `ClientAPI` contract (`CON-api-contract` only binds
//! that client's own endpoints), but they deliberately reuse the SAME all-200 error taxonomy as
//! every other route in this project (`docs/adr/0003-all-200-error-taxonomy.md`,
//! `DEC-api-contract-conventions`): every handler below returns `Result<Json<T>, AppError>` and
//! declares an extractor whose rejection also converges on HTTP 200 (`AuthRejection`). Maintaining
//! a second error convention for eight routes would be a real cost with no real benefit.
//!
//! Every handler is a thin delegate to a `src/repository.rs` function ALREADY shared with
//! `savez mod` (`src/cli/moderation.rs`, `docs/adr/0004-cli-serve-subcommand.md`) — no business
//! rule is reimplemented here, on the same model as `routes::puzzles::report`.
//!
//! Deliberately absent: a route for the repository's role-promotion function. SPEC §4.6 grants
//! the `admin` role the right to promote/demote, but defines no HTTP route for exercising it, and
//! 07-06 already resolved this as CLI-only (`savez mod promote`/`demote`, T-07-28) — this module
//! must not invent one.
//!
//! Also deliberately absent: rate limiting. `/v1/moderation/*` is excluded from the quota-checking
//! call every `puzzles`/`auth` handler makes (07-07's decision, T-07-37) — imposing one here would
//! risk the moderation team locking itself out of its own tooling.

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;

use crate::db::AppState;
use crate::error::AppError;
use crate::repository::{self, ReportQueueEntry};
use crate::routes::puzzles::parse_puzzle_id;

/// Default page size for the two paginated read routes (`list_reports`/`log`) when the caller
/// omits `limit`.
const DEFAULT_LIMIT: i64 = 50;

/// Upper bound on `limit` for both paginated read routes — an availability protection (an
/// unbounded read could return the entire table), not an aesthetic preference.
const MAX_LIMIT: i64 = 200;

/// Parses and bounds an optional `limit`/`offset` query-string pair, shared by `list_reports` and
/// `log`. Both arrive as `Option<String>` (never a numeric axum query type) so that a non-numeric
/// value is rejected with the taxonomy code `bad-payload`, never axum's native, non-taxonomy
/// rejection — same discipline `ReportQuery`/`LogQuery` apply by construction below.
fn parse_limit_offset(limit: Option<&str>, offset: Option<&str>) -> Result<(i64, i64), AppError> {
    let limit = match limit {
        None => DEFAULT_LIMIT,
        Some(raw) => raw.trim().parse::<i64>().map_err(|_| AppError::BadPayload)?,
    }
    .clamp(1, MAX_LIMIT);

    let offset = match offset {
        None => 0,
        Some(raw) => raw.trim().parse::<i64>().map_err(|_| AppError::BadPayload)?,
    }
    .max(0);

    Ok((limit, offset))
}

/// Wire body of `POST /v1/moderation/reports/{id}/resolve`. `status` is mandatory (a resolution
/// with no target status is a malformed request, not a partial-but-valid one — same posture as
/// `SubmitPuzzlePayload`'s required fields); `notes` is facultative and carries `#[serde(default)]`
/// so an absent value deserializes as `None` rather than failing axum's own, non-taxonomy
/// rejection.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveRequest {
    pub status: String,
    #[serde(default)]
    pub notes: Option<String>,
}

/// Wire body of `POST /v1/moderation/puzzles/{id}/hide` and `.../unhide` — entirely optional (a
/// client may send no body at all), reflected by both the `Option<Json<HideRequest>>` extractor on
/// the handlers below AND `reason` itself defaulting to `None`.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HideRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

/// Query-string parameters of `GET /v1/moderation/reports`. `limit`/`offset` are `Option<String>`,
/// never a numeric axum query type, for the same `bad-payload`-not-native-rejection reason as
/// `parse_limit_offset` above.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportQuery {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub limit: Option<String>,
    #[serde(default)]
    pub offset: Option<String>,
}

/// `GET /v1/moderation/reports` (moderator). `status` absent resolves to `Some("pending")` (SPEC's
/// own default); `status=all` resolves to `None` (no filter, every status returned); any other
/// value is passed through to `repository::list_reports` unchanged — that function filters by
/// exact equality, so an unrecognized value simply yields an empty list, the expected behavior of
/// a filter, not an error condition.
pub async fn list_reports(
    State(state): State<AppState>,
    _auth: crate::auth::extractor::ModeratorUser,
    Query(query): Query<ReportQuery>,
) -> Result<Json<Vec<ReportQueueEntry>>, AppError> {
    let (limit, offset) = parse_limit_offset(query.limit.as_deref(), query.offset.as_deref())?;
    let status = match query.status.as_deref() {
        None => Some("pending"),
        Some("all") => None,
        Some(other) => Some(other),
    };
    Ok(Json(
        repository::list_reports(&state.pool, status, limit, offset).await?,
    ))
}

/// `POST /v1/moderation/reports/{id}/resolve` (moderator). `{id}` resolves via the same
/// `parse_puzzle_id` every numeric-`:id` puzzle route already uses — `puzzle_reports.id` is the
/// same `i32` primary-key shape as `puzzles.id`.
pub async fn resolve_report(
    State(state): State<AppState>,
    auth: crate::auth::extractor::ModeratorUser,
    Path(id): Path<String>,
    Json(payload): Json<ResolveRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let id = parse_puzzle_id(&id)?;
    let resolved = repository::resolve_report(
        &state.pool,
        id,
        &payload.status,
        auth.0.user_id,
        payload.notes.as_deref(),
    )
    .await?;
    Ok(Json(
        serde_json::json!({ "success": true, "resolved": resolved }),
    ))
}

/// `POST /v1/moderation/puzzles/{id}/hide` (moderator). `body` is `None` when the client sends no
/// `Content-Type` header at all (axum's `OptionalFromRequest` impl for `Json<T>`) — a fully absent
/// body and an explicit `{}` both resolve to `reason: None` either way.
pub async fn hide(
    State(state): State<AppState>,
    auth: crate::auth::extractor::ModeratorUser,
    Path(id): Path<String>,
    body: Option<Json<HideRequest>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let id = parse_puzzle_id(&id)?;
    let reason = body.and_then(|Json(payload)| payload.reason);
    repository::hide_puzzle(&state.pool, id, auth.0.user_id, reason.as_deref()).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}

/// `POST /v1/moderation/puzzles/{id}/unhide` (moderator). Same optional-body posture as `hide`
/// above.
pub async fn unhide(
    State(state): State<AppState>,
    auth: crate::auth::extractor::ModeratorUser,
    Path(id): Path<String>,
    body: Option<Json<HideRequest>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let id = parse_puzzle_id(&id)?;
    let reason = body.and_then(|Json(payload)| payload.reason);
    repository::unhide_puzzle(&state.pool, id, auth.0.user_id, reason.as_deref()).await?;
    Ok(Json(serde_json::json!({ "success": true })))
}
