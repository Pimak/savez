// TODO(Phase 6): the project-wide all-in-200 convention (DEC-api-contract-conventions —
// every response is HTTP 200, business errors carry a `{ "error": "<code>" }` body using the
// `T.backendErrors` taxonomy) lands with the rest of the error taxonomy in Phase 6
// (REQ-shapez-contract-complete). In Phase 3, `AppError` maps straight to plain HTTP status
// codes; a `UNIQUE(short_key)` constraint violation is not special-cased and simply surfaces
// as `Database` -> 500. Phase 5 adds five more auth-related variants (OracleVerificationFailed,
// TokenIssuanceFailed, NameTaken, InvalidName, AuthModeNotImplemented) under the same
// temporary-mapping regime — all five are superseded wholesale by Phase 6's taxonomy too.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("not found")]
    NotFound,
    // TODO(Phase 6): replace with the full `T.backendErrors` taxonomy mapping
    // (DEC-api-contract-conventions) — this is a temporary generic "bad request" bucket for the
    // Phase 4 decode-failure path (D-04/D-05): neither JSON-direct nor lz-string decompression
    // produced a valid `PuzzleGameData`, or the decompressed payload exceeded
    // MAX_DECOMPRESSED_PUZZLE_DATA_BYTES (D-06/D-07).
    #[error("invalid puzzle data")]
    InvalidPuzzleData,
    #[error("oracle verification failed")]
    OracleVerificationFailed,
    #[error("token issuance failed")]
    TokenIssuanceFailed,
    #[error("name already taken")]
    NameTaken,
    #[error("invalid name")]
    InvalidName,
    #[error("auth mode not implemented")]
    AuthModeNotImplemented,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::Database(err) => {
                tracing::error!(error = %err, "database error");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
            AppError::NotFound => StatusCode::NOT_FOUND.into_response(),
            AppError::InvalidPuzzleData => {
                // WR-02 (04-REVIEW.md): every decode failure lands here — malformed input,
                // oversized payloads, and rejected decompression-bomb attempts alike. Without
                // this, repeated probing of the decode path (including the CR-01 bomb vector)
                // leaves no trace in logs/metrics.
                tracing::warn!("rejected puzzle submission: invalid or oversized data");
                StatusCode::BAD_REQUEST.into_response()
            }
            AppError::OracleVerificationFailed => {
                // Never log the official token itself here (T-05-13) — only the fact that
                // verification failed. No account is created either way (D-05).
                tracing::warn!("oracle verification failed: no account created");
                StatusCode::UNAUTHORIZED.into_response()
            }
            AppError::TokenIssuanceFailed => {
                tracing::error!("failed to issue server JWT after successful oracle verification");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
            AppError::NameTaken => {
                // D-03: hard rejection, never a silent merge/reuse of an existing account.
                tracing::warn!("rejected registration: name already taken");
                StatusCode::CONFLICT.into_response()
            }
            AppError::InvalidName => {
                tracing::warn!("rejected registration: invalid name");
                StatusCode::BAD_REQUEST.into_response()
            }
            AppError::AuthModeNotImplemented => {
                // `open`/`steam-openid` are declared in config (SC4) but not functional in v1 —
                // a deployment switched to `open` must refuse registrations, not silently accept
                // them without verification.
                tracing::warn!("rejected: configured AUTH_MODE is not implemented in v1");
                StatusCode::NOT_IMPLEMENTED.into_response()
            }
        }
    }
}
