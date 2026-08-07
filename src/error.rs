// TODO(Phase 6): the project-wide all-in-200 convention (DEC-api-contract-conventions —
// every response is HTTP 200, business errors carry a `{ "error": "<code>" }` body using the
// `T.backendErrors` taxonomy) lands with the rest of the error taxonomy in Phase 6
// (REQ-shapez-contract-complete). In Phase 3, `AppError` maps straight to plain HTTP status
// codes; a `UNIQUE(short_key)` constraint violation is not special-cased and simply surfaces
// as `Database` -> 500.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("not found")]
    NotFound,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::Database(err) => {
                tracing::error!(error = %err, "database error");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
            AppError::NotFound => StatusCode::NOT_FOUND.into_response(),
        }
    }
}
