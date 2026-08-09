// Definitive Phase 6 error contract (DEC-api-contract-conventions, D-17): every business error
// answers HTTP 200 with `{ "error": "<code>" }`, and every code comes from the client's
// `T.backendErrors` taxonomy — or, failing an equivalent, from a short, explicitly documented
// list of project-specific codes (see `docs/adr/0003-all-200-error-taxonomy.md`). The one
// exception is `AppError::Database`, which stays a literal 5xx: see that ADR for why an
// infrastructure failure is not folded into the business-error taxonomy.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(thiserror::Error, Debug)]
pub enum AppError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("not found")]
    NotFound,
    #[error("invalid puzzle data")]
    InvalidPuzzleData,
    #[error("bad payload")]
    BadPayload,
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
    #[error("bad id")]
    BadId,
    #[error("bad category")]
    BadCategory,
    #[error("bad short key")]
    BadShortKey,
    #[error("short key already taken")]
    ShortKeyTaken,
    #[error("bad title")]
    BadTitle,
    #[error("profane title")]
    ProfaneTitle,
    #[error("no emitters")]
    NoEmitters,
    #[error("no goals")]
    NoGoals,
    #[error("bad shape key in emitter")]
    BadShapeKeyInEmitter,
    #[error("bad shape key in goal")]
    BadShapeKeyInGoal,
    #[error("bad building placement")]
    BadBuildingPlacement,
    #[error("cannot report your own puzzle")]
    CannotReportOwnPuzzle,
    #[error("duplicate report")]
    DuplicateReport,
    #[error("no permission")]
    NoPermission,
}

impl AppError {
    /// Total, panic-free mapping from every variant to its wire code — exhaustive `match`, no
    /// `_ =>` arm, so a variant added later without a code fails compilation instead of silently
    /// falling through. `Database` returns `"internal-error"` even though `into_response` never
    /// takes this path for it (see below): `code()` itself must stay total.
    pub fn code(&self) -> &'static str {
        match self {
            AppError::Database(_) => "internal-error",
            AppError::NotFound => "not-found",
            AppError::InvalidPuzzleData => "bad-payload",
            AppError::BadPayload => "bad-payload",
            AppError::OracleVerificationFailed => "unauthorized",
            AppError::TokenIssuanceFailed => "internal-error",
            AppError::NameTaken => "name-already-taken",
            AppError::InvalidName => "bad-payload",
            AppError::AuthModeNotImplemented => "auth-mode-not-implemented",
            AppError::BadId => "bad-id",
            AppError::BadCategory => "bad-category",
            AppError::BadShortKey => "bad-short-key",
            AppError::ShortKeyTaken => "short-key-already-taken",
            AppError::BadTitle => "bad-title-too-many-spaces",
            AppError::ProfaneTitle => "profane-title",
            AppError::NoEmitters => "no-emitters",
            AppError::NoGoals => "no-goals",
            AppError::BadShapeKeyInEmitter => "bad-shape-key-in-emitter",
            AppError::BadShapeKeyInGoal => "bad-shape-key-in-goal",
            AppError::BadBuildingPlacement => "bad-building-placement",
            AppError::CannotReportOwnPuzzle => "can-not-report-your-own-puzzle",
            AppError::DuplicateReport => "bad-payload",
            AppError::NoPermission => "no-permission",
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match &self {
            AppError::Database(err) => {
                // The sole 5xx path: log the underlying sqlx error server-side only, and return a
                // bare 500 with no body -- never fold an infrastructure failure into the
                // business-error taxonomy (docs/adr/0003-all-200-error-taxonomy.md, T-06-05).
                tracing::error!(error = %err, "database error");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            AppError::InvalidPuzzleData => {
                // WR-02 (04-REVIEW.md): every decode failure lands here -- malformed input,
                // oversized payloads, and rejected decompression-bomb attempts alike. Without
                // this, repeated probing of the decode path (including the CR-01 bomb vector)
                // leaves no trace in logs/metrics.
                tracing::warn!("rejected puzzle submission: invalid or oversized data");
            }
            AppError::OracleVerificationFailed => {
                // Never log the official token itself here (T-05-13) -- only the fact that
                // verification failed. No account is created either way (D-05).
                tracing::warn!("oracle verification failed: no account created");
            }
            AppError::TokenIssuanceFailed => {
                tracing::error!("failed to issue server JWT after successful oracle verification");
            }
            AppError::NameTaken => {
                // D-03: hard rejection, never a silent merge/reuse of an existing account.
                tracing::warn!("rejected registration: name already taken");
            }
            AppError::InvalidName => {
                tracing::warn!("rejected registration: invalid name");
            }
            AppError::AuthModeNotImplemented => {
                // `open`/`steam-openid` are declared in config (SC4) but not functional in v1 --
                // a deployment switched to `open` must refuse registrations, not silently accept
                // them without verification.
                tracing::warn!("rejected: configured AUTH_MODE is not implemented in v1");
            }
            _ => {
                tracing::warn!(code = self.code(), "rejected request");
            }
        }

        (
            StatusCode::OK,
            Json(serde_json::json!({ "error": self.code() })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    /// The 21 codes of the client's `T.backendErrors` taxonomy (verbatim,
    /// `translations/base-en.yaml` — identical across both real clients, see 06-RESEARCH.md).
    const T_BACKEND_ERRORS: &[&str] = &[
        "ratelimit",
        "invalid-api-key",
        "unauthorized",
        "bad-token",
        "bad-id",
        "not-found",
        "bad-category",
        "bad-short-key",
        "profane-title",
        "bad-title-too-many-spaces",
        "bad-shape-key-in-emitter",
        "bad-shape-key-in-goal",
        "no-emitters",
        "no-goals",
        "short-key-already-taken",
        "can-not-report-your-own-puzzle",
        "bad-payload",
        "bad-building-placement",
        "timeout",
        "too-many-likes-already",
        "no-permission",
    ];

    /// Codes with no equivalent in `T.backendErrors` — the client falls back to displaying the
    /// raw string for these (see `docs/adr/0003-all-200-error-taxonomy.md`).
    const PROJECT_SPECIFIC_CODES: &[&str] = &[
        "name-already-taken",
        "auth-mode-not-implemented",
        "internal-error",
    ];

    async fn body_to_json(response: Response) -> serde_json::Value {
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect response body")
            .to_bytes();
        serde_json::from_slice(&bytes).expect("response body is valid JSON")
    }

    /// Every business variant built here is checked against the taxonomy tables. `Database` is
    /// deliberately excluded (it never reaches `code()` through `into_response`, and is covered
    /// separately by `database_variant_stays_5xx`).
    fn all_business_variants() -> Vec<AppError> {
        vec![
            AppError::NotFound,
            AppError::InvalidPuzzleData,
            AppError::BadPayload,
            AppError::OracleVerificationFailed,
            AppError::TokenIssuanceFailed,
            AppError::NameTaken,
            AppError::InvalidName,
            AppError::AuthModeNotImplemented,
            AppError::BadId,
            AppError::BadCategory,
            AppError::BadShortKey,
            AppError::ShortKeyTaken,
            AppError::BadTitle,
            AppError::ProfaneTitle,
            AppError::NoEmitters,
            AppError::NoGoals,
            AppError::BadShapeKeyInEmitter,
            AppError::BadShapeKeyInGoal,
            AppError::BadBuildingPlacement,
            AppError::CannotReportOwnPuzzle,
            AppError::DuplicateReport,
            AppError::NoPermission,
        ]
    }

    #[test]
    fn every_business_variant_has_a_known_code() {
        for variant in all_business_variants() {
            let code = variant.code();
            assert!(
                T_BACKEND_ERRORS.contains(&code) || PROJECT_SPECIFIC_CODES.contains(&code),
                "variant {variant:?} produced code {code:?}, which belongs to neither \
                 T_BACKEND_ERRORS nor PROJECT_SPECIFIC_CODES"
            );
        }
    }

    #[tokio::test]
    async fn business_variants_respond_200_with_error_body() {
        let response = AppError::NotFound.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_to_json(response).await,
            serde_json::json!({ "error": "not-found" })
        );

        let response = AppError::InvalidPuzzleData.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_to_json(response).await,
            serde_json::json!({ "error": "bad-payload" })
        );

        let response = AppError::NoPermission.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_to_json(response).await,
            serde_json::json!({ "error": "no-permission" })
        );

        let response = AppError::ShortKeyTaken.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_to_json(response).await,
            serde_json::json!({ "error": "short-key-already-taken" })
        );
    }

    #[tokio::test]
    async fn database_variant_stays_5xx() {
        let response = AppError::Database(sqlx::Error::PoolClosed).into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect response body")
            .to_bytes();
        assert!(bytes.is_empty(), "expected empty body, got: {bytes:?}");
    }
}
