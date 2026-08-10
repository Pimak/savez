//! `AuthUser` — the extractor that enforces CON-api-contract's authentication rule for protected
//! routes: only `x-token` authenticates a request, never `Authorization: Bearer` and never
//! `x-api-key` (that header identifies the calling APPLICATION on real client requests, not the
//! user — reading it here would be an authentication bypass). See 05-RESEARCH.md Pattern 3.

use axum::Json;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use crate::auth::cache::{CacheFetchError, Role};
use crate::db::AppState;

/// The authenticated identity of a request, injected by extracting and validating the `x-token`
/// header, then reading the current role/ban state from `AppState.auth_cache` (D-09). Neither
/// `role` nor `banned` is ever a JWT claim — the token stays reduced to `sub`/`exp` (D-06, Phase
/// 5), which is exactly why this state is re-read (behind a short-TTL cache, not on every request)
/// rather than trusted from the token itself.
pub struct AuthUser {
    pub user_id: Uuid,
    pub role: Role,
    pub banned: bool,
}

/// Converges on the same wire format as `src/error.rs::AppError` (D-17): every rejection answers
/// HTTP 200 with `{ "error": "<code>" }`, never a bare 401 — see
/// `docs/adr/0003-all-200-error-taxonomy.md`. `Infrastructure` is the sole exception, mirroring
/// `AppError::Database`: a database failure while reading role/ban state must not be disguised as
/// a business-taxonomy error.
#[derive(Debug)]
pub enum AuthRejection {
    MissingToken,
    InvalidToken,
    Banned,
    InsufficientRole,
    Infrastructure,
}

impl AuthRejection {
    fn code(&self) -> &'static str {
        match self {
            AuthRejection::MissingToken => "unauthorized",
            AuthRejection::InvalidToken => "bad-token",
            AuthRejection::Banned => "banned",
            AuthRejection::InsufficientRole => "no-permission",
            // Never placed on the wire (see `into_response`'s early return below) -- kept here
            // only so `code()` stays a total, exhaustive match, mirroring `AppError::code`'s own
            // discipline for its `Database` variant.
            AuthRejection::Infrastructure => "internal-error",
        }
    }
}

impl IntoResponse for AuthRejection {
    fn into_response(self) -> Response {
        match &self {
            AuthRejection::MissingToken => {
                tracing::warn!("rejected request: no auth token presented");
            }
            AuthRejection::InvalidToken => {
                // T-05-26: never log the token value itself, only the fact that it failed.
                tracing::warn!("rejected request: auth token failed validation");
            }
            AuthRejection::Banned => {
                tracing::warn!("rejected request: user is banned");
            }
            AuthRejection::InsufficientRole => {
                tracing::warn!("rejected request: insufficient role");
            }
            AuthRejection::Infrastructure => {
                // The sole 5xx path: a role/ban read failure is an infrastructure failure, not a
                // business-taxonomy error -- same posture as `AppError::Database`, empty body, no
                // detail leaked to the client.
                tracing::error!("auth infrastructure failure: role/ban state read failed");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
        let code = self.code();
        (StatusCode::OK, Json(serde_json::json!({ "error": code }))).into_response()
    }
}

/// Pure function, deliberately separate from `from_request_parts`: testable directly against a
/// `HeaderMap` with no `AppState`/database connection needed, so these tests never touch a
/// database.
///
/// Two prohibitions, both non-negotiable (CON-api-contract, T-05-10):
/// - `Authorization: Bearer ...` is never read here — the shapez ClientAPI contract has no
///   concept of a Bearer scheme, and accepting it would silently widen the authentication
///   surface beyond the documented contract.
/// - `x-api-key` is present on every real client request, but it identifies the calling
///   APPLICATION (a fixed, publicly-known value — see `auth::oracle::CLIENT_API_KEY`), never the
///   user. Treating it as a credential here would let anyone holding the well-known API key
///   authenticate as an arbitrary user — an authentication bypass, not a convenience.
///
/// Signature + expiration only, no database access — role/ban state is layered on top by
/// `AuthUser::from_request_parts`, which composes this function's `Uuid` with a call to
/// `AppState.auth_cache` (REQ-moderation, D-09).
///
/// Single source-of-truth literal for the header name: referenced by name everywhere else in this
/// module (including tests) so the string appears exactly once in this file.
const AUTH_HEADER_NAME: &str = "x-token";

pub(crate) fn authenticate_headers(
    headers: &HeaderMap,
    jwt_key: &str,
) -> Result<Uuid, AuthRejection> {
    let token = headers
        .get(AUTH_HEADER_NAME)
        .and_then(|value| value.to_str().ok())
        .ok_or(AuthRejection::MissingToken)?;

    let claims =
        crate::auth::jwt::validate(jwt_key, token).map_err(|_| AuthRejection::InvalidToken)?;

    Ok(claims.sub)
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AuthRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user_id = authenticate_headers(&parts.headers, &state.jwt_key)?;
        let cached = state
            .auth_cache
            .get(&state.pool, user_id)
            .await
            .map_err(|err| match err {
                CacheFetchError::UnknownUser => AuthRejection::InvalidToken,
                CacheFetchError::Database => AuthRejection::Infrastructure,
            })?;

        Ok(AuthUser {
            user_id,
            role: cached.role,
            banned: cached.banned,
        })
    }
}

/// Wraps `AuthUser`, additionally rejecting a currently-banned caller (D-13's exact scope: applied
/// only to `submit`/`complete`/`report` by the plan that consumes this type — `login` gets its own
/// check, `list`/`search`/`download`/`delete` stay unaffected). No extra database round-trip
/// beyond `AuthUser`'s own: `banned` is already part of the cached state.
pub struct ActiveUser(pub AuthUser);

impl FromRequestParts<AppState> for ActiveUser {
    type Rejection = AuthRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth = AuthUser::from_request_parts(parts, state).await?;
        if auth.banned {
            return Err(AuthRejection::Banned);
        }
        Ok(ActiveUser(auth))
    }
}

/// Wraps `AuthUser`, additionally requiring `role >= Role::Moderator` (cumulative: an admin also
/// satisfies this). No extra database round-trip beyond `AuthUser`'s own.
pub struct ModeratorUser(pub AuthUser);

impl FromRequestParts<AppState> for ModeratorUser {
    type Rejection = AuthRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth = AuthUser::from_request_parts(parts, state).await?;
        if auth.role < Role::Moderator {
            return Err(AuthRejection::InsufficientRole);
        }
        Ok(ModeratorUser(auth))
    }
}

/// Wraps `AuthUser`, additionally requiring `role >= Role::Admin`. No extra database round-trip
/// beyond `AuthUser`'s own.
pub struct AdminUser(pub AuthUser);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = AuthRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth = AuthUser::from_request_parts(parts, state).await?;
        if auth.role < Role::Admin {
            return Err(AuthRejection::InsufficientRole);
        }
        Ok(AdminUser(auth))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};

    const TEST_KEY: &str = "test-extractor-key-not-a-secret";

    fn headers_with(name: &str, value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).expect("valid header name"),
            value.parse().expect("valid header value"),
        );
        headers
    }

    #[test]
    fn accepts_valid_jwt_in_x_token() {
        let user_id = Uuid::new_v4();
        let token =
            crate::auth::jwt::issue(TEST_KEY, user_id, crate::auth::jwt::DEFAULT_LIFETIME_SECS)
                .expect("issue test jwt");
        let headers = headers_with(AUTH_HEADER_NAME, &token);

        let authenticated_id =
            authenticate_headers(&headers, TEST_KEY).expect("valid x-token must authenticate");
        assert_eq!(authenticated_id, user_id);
    }

    #[test]
    fn rejects_missing_header() {
        let headers = HeaderMap::new();
        assert!(matches!(
            authenticate_headers(&headers, TEST_KEY),
            Err(AuthRejection::MissingToken)
        ));
    }

    #[test]
    fn rejects_bearer_authorization_header() {
        let user_id = Uuid::new_v4();
        let token =
            crate::auth::jwt::issue(TEST_KEY, user_id, crate::auth::jwt::DEFAULT_LIFETIME_SECS)
                .expect("issue test jwt");
        let headers = headers_with("authorization", &format!("Bearer {token}"));

        assert!(matches!(
            authenticate_headers(&headers, TEST_KEY),
            Err(AuthRejection::MissingToken)
        ));
    }

    #[test]
    fn ignores_x_api_key_header() {
        let user_id = Uuid::new_v4();
        let token =
            crate::auth::jwt::issue(TEST_KEY, user_id, crate::auth::jwt::DEFAULT_LIFETIME_SECS)
                .expect("issue test jwt");
        let headers = headers_with("x-api-key", &token);

        assert!(matches!(
            authenticate_headers(&headers, TEST_KEY),
            Err(AuthRejection::MissingToken)
        ));
    }

    #[test]
    fn rejects_garbage_token() {
        let headers = headers_with(AUTH_HEADER_NAME, "not-a-jwt");
        assert!(matches!(
            authenticate_headers(&headers, TEST_KEY),
            Err(AuthRejection::InvalidToken)
        ));
    }

    #[test]
    fn rejects_expired_token() {
        let claims = crate::auth::jwt::Claims {
            sub: Uuid::new_v4(),
            exp: (jsonwebtoken::get_current_timestamp() - 10_000) as usize,
        };
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(TEST_KEY.as_bytes()),
        )
        .expect("encode expired token");
        let headers = headers_with(AUTH_HEADER_NAME, &token);

        assert!(matches!(
            authenticate_headers(&headers, TEST_KEY),
            Err(AuthRejection::InvalidToken)
        ));
    }

    #[test]
    fn rejects_token_signed_with_another_key() {
        let user_id = Uuid::new_v4();
        let token = crate::auth::jwt::issue(
            "a-different-key",
            user_id,
            crate::auth::jwt::DEFAULT_LIFETIME_SECS,
        )
        .expect("issue test jwt");
        let headers = headers_with(AUTH_HEADER_NAME, &token);

        assert!(matches!(
            authenticate_headers(&headers, TEST_KEY),
            Err(AuthRejection::InvalidToken)
        ));
    }

    /// D-17: both pre-existing rejection variants converge on the same wire format as `AppError`
    /// — HTTP 200, never a bare 401, body `{ "error": "<code>" }`.
    #[tokio::test]
    async fn rejections_respond_200_with_error_code() {
        use http_body_util::BodyExt;

        async fn body_to_json(response: Response) -> serde_json::Value {
            let bytes = response
                .into_body()
                .collect()
                .await
                .expect("collect response body")
                .to_bytes();
            serde_json::from_slice(&bytes).expect("response body is valid JSON")
        }

        let response = AuthRejection::MissingToken.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_to_json(response).await,
            serde_json::json!({ "error": "unauthorized" })
        );

        let response = AuthRejection::InvalidToken.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_to_json(response).await,
            serde_json::json!({ "error": "bad-token" })
        );
    }

    /// REQ-moderation: `Banned` and `InsufficientRole` follow the same 200-with-taxonomy-code
    /// convention as every other rejection; `Infrastructure` is the sole 5xx exception, mirroring
    /// `AppError::Database`'s empty-body 500.
    #[tokio::test]
    async fn banned_responds_200_with_banned_code() {
        use http_body_util::BodyExt;

        let response = AuthRejection::Banned.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect response body")
            .to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("valid json body");
        assert_eq!(body, serde_json::json!({ "error": "banned" }));
    }

    #[tokio::test]
    async fn insufficient_role_responds_200_with_no_permission_code() {
        use http_body_util::BodyExt;

        let response = AuthRejection::InsufficientRole.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect response body")
            .to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("valid json body");
        assert_eq!(body, serde_json::json!({ "error": "no-permission" }));
    }

    #[tokio::test]
    async fn infrastructure_responds_500_with_empty_body() {
        use http_body_util::BodyExt;

        let response = AuthRejection::Infrastructure.into_response();
        assert_eq!(response.status().as_u16(), 500);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect response body")
            .to_bytes();
        assert!(bytes.is_empty(), "expected empty body, got: {bytes:?}");
    }
}
