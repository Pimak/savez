//! `AuthUser` — the extractor that enforces CON-api-contract's authentication rule for protected
//! routes: only `x-token` authenticates a request, never `Authorization: Bearer` and never
//! `x-api-key` (that header identifies the calling APPLICATION on real client requests, not the
//! user — reading it here would be an authentication bypass). See 05-RESEARCH.md Pattern 3.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use crate::db::AppState;

/// The authenticated identity of a request, injected by extracting and validating the `x-token`
/// header. Carries only `user_id` (== the JWT's `sub` claim) — no role/ban state (D-07, mirrors
/// `auth::jwt::Claims`'s own minimalism).
pub struct AuthUser {
    pub user_id: Uuid,
}

/// TODO(Phase 6): the project-wide `T.backendErrors` taxonomy (DEC-api-contract-conventions)
/// replaces this bare `401` with `{ "error": "unauthorized" }` / `{ "error": "bad-token" }` —
/// same temporary-mapping regime as `src/error.rs::AppError` until Phase 6 lands.
#[derive(Debug)]
pub enum AuthRejection {
    MissingToken,
    InvalidToken,
}

impl IntoResponse for AuthRejection {
    fn into_response(self) -> Response {
        match self {
            AuthRejection::MissingToken => {
                tracing::warn!("rejected request: no auth token presented");
                StatusCode::UNAUTHORIZED.into_response()
            }
            AuthRejection::InvalidToken => {
                // T-05-26: never log the token value itself, only the fact that it failed.
                tracing::warn!("rejected request: auth token failed validation");
                StatusCode::UNAUTHORIZED.into_response()
            }
        }
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
/// D-07 (Phase 5 scope): signature + expiration only, no database access. TODO(Phase 7): ban/role
/// enforcement re-reads current state from the database by `sub` (REQ-moderation) — never a claim
/// added to the JWT itself (would go stale for the token's full 30-day lifetime, D-06).
///
/// Single source-of-truth literal for the header name: referenced by name everywhere else in this
/// module (including tests) so the string appears exactly once in this file.
const AUTH_HEADER_NAME: &str = "x-token";

pub(crate) fn authenticate_headers(
    headers: &HeaderMap,
    jwt_key: &str,
) -> Result<AuthUser, AuthRejection> {
    let token = headers
        .get(AUTH_HEADER_NAME)
        .and_then(|value| value.to_str().ok())
        .ok_or(AuthRejection::MissingToken)?;

    let claims =
        crate::auth::jwt::validate(jwt_key, token).map_err(|_| AuthRejection::InvalidToken)?;

    Ok(AuthUser {
        user_id: claims.sub,
    })
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AuthRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        authenticate_headers(&parts.headers, &state.jwt_key)
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

        let auth_user =
            authenticate_headers(&headers, TEST_KEY).expect("valid x-token must authenticate");
        assert_eq!(auth_user.user_id, user_id);
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
}
