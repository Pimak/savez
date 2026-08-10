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
/// 3. Refuse a pseudo that is currently banned (D-13) BEFORE any network call: a banned account
///    must never consume an oracle call either, for the same reason step 2 doesn't let an invalid
///    name through first. This checks the pseudo, never a `user_id` -- `/v1/public/login` has no
///    authenticated identity yet at this point, and the pseudo is the only stable link between
///    this request and an existing account. Reads the database directly, bypassing the request-
///    scoped role/ban cache entirely: that cache is keyed by an identity this step doesn't have
///    yet, and identity establishment is precisely the moment that calls for the freshest read
///    (ASVS V2) -- see the repository function called below for the full rationale.
/// 4. Resolve the pseudo to an existing `user_id` (`repository::find_user_id`) and, ONLY if the
///    account already exists, check the D-02 `read`-class rate limit BEFORE the oracle call below
///    -- saving the (comparatively expensive, network-bound) oracle round-trip for a request
///    already over quota. A brand-new registration has no `user_id` yet at this point
///    (`rate_limit_events.user_id` is a foreign key against `users`) and so is structurally exempt
///    from this check. `TODO(post-v1)`: this leaves first-time registrations unbounded by
///    rate-limiting; the only remaining barrier for them is the oracle's own DLC-ownership check
///    below, which is already mandatory for every registration regardless.
/// 5. Call the oracle exactly once, no retry -- no database write precedes this call (D-05).
/// 6. Create the account.
/// 7. Issue the server JWT. `jsonwebtoken`'s own error is never propagated to the client (it
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

    if repository::is_name_banned(&state.pool, &req.name).await? {
        return Err(AppError::Banned);
    }

    if let Some(existing_user_id) = repository::find_user_id(&state.pool, &req.name).await? {
        crate::ratelimit::check_and_record(
            &state.pool,
            existing_user_id,
            crate::ratelimit::RouteClass::Read,
        )
        .await?;
    }

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
