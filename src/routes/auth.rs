use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use crate::auth;
use crate::config::AuthMode;
use crate::db::AppState;
use crate::error::AppError;
use crate::repository;

// Bornes applicatives : `users.name` est un `TEXT` sans contrainte de longueur en base, la borne
// doit donc exister ici (ASVS V5).
const MIN_NAME_LEN: usize = 3;
const MAX_NAME_LEN: usize = 32;

const VERIFIED_VIA_OFFICIAL_API: &str = "official-api";

fn validate_name(name: &str) -> Result<(), AppError> {
    let char_count = name.chars().count();
    if !(MIN_NAME_LEN..=MAX_NAME_LEN).contains(&char_count) {
        return Err(AppError::InvalidName);
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(AppError::InvalidName);
    }
    // No implicit trimming: two names must never be able to differ only by invisible leading/
    // trailing whitespace.
    if name != name.trim() {
        return Err(AppError::InvalidName);
    }
    Ok(())
}

/// Wire body of `POST /v1/public/login`. The `name` field is the D-02 extension assumed by this
/// plan: SPEC §4.2 documents only `{ token }`, but reading the source of both real clients
/// (official + Community Edition) established that the official login flow carries no
/// human-readable identity at all -- the official token is an opaque Steam ticket, and the
/// Community Edition token is a manually-pasted opaque string. `/v1/public/login` is our own
/// endpoint, never called by a stock client: this is an extension of our own contract, not a
/// break of the upstream one.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    pub token: String,
    pub name: String,
}

/// Response body is exactly `{ "token": "<jwt>" }` -- never the underlying user row.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginResponse {
    pub token: String,
}

/// `POST /v1/public/login`. Order of operations below is mandatory and not permutable -- it is
/// the security barrier for the whole phase:
///
/// 1. Refuse immediately if `auth_mode` isn't `Oracle` -- a deployment switched to `open`/
///    `steam-openid` must refuse registrations, never silently accept them without verification.
/// 2. Validate the name BEFORE any network call: an invalid name must never consume an oracle
///    call (CON-official-api-usage).
/// 3. Call the oracle exactly once, no retry -- no database write precedes this call (D-05).
/// 4. Create the account.
/// 5. Issue the server JWT. `jsonwebtoken`'s own error is never propagated to the client (it
///    could describe the signing key).
///
/// Never logs `req.token`, `req.name`, nor the issued JWT.
pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, AppError> {
    if state.auth_mode != AuthMode::Oracle {
        return Err(AppError::AuthModeNotImplemented);
    }

    validate_name(&req.name)?;

    auth::oracle::verify_official_ownership(
        &state.http_client,
        &state.official_api_url,
        &req.token,
    )
    .await?;

    let user_id =
        repository::insert_user(&state.pool, &req.name, VERIFIED_VIA_OFFICIAL_API).await?;

    let jwt = auth::jwt::issue(&state.jwt_key, user_id, auth::jwt::DEFAULT_LIFETIME_SECS)
        .map_err(|_| AppError::TokenIssuanceFailed)?;

    Ok(Json(LoginResponse { token: jwt }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_typical_names() {
        assert!(validate_name("Zorg").is_ok());
        assert!(validate_name("joueur_42").is_ok());
        assert!(validate_name("a-b-c").is_ok());
    }

    #[test]
    fn rejects_too_short() {
        assert!(matches!(validate_name("ab"), Err(AppError::InvalidName)));
    }

    #[test]
    fn rejects_too_long() {
        let name = "a".repeat(33);
        assert!(matches!(validate_name(&name), Err(AppError::InvalidName)));
    }

    #[test]
    fn rejects_disallowed_charset() {
        assert!(matches!(
            validate_name("jean dupont"),
            Err(AppError::InvalidName)
        ));
        assert!(matches!(
            validate_name("élodie"),
            Err(AppError::InvalidName)
        ));
        assert!(matches!(
            validate_name("drop;table"),
            Err(AppError::InvalidName)
        ));
    }

    #[test]
    fn rejects_surrounding_whitespace() {
        assert!(matches!(
            validate_name(" zorg "),
            Err(AppError::InvalidName)
        ));
    }
}
