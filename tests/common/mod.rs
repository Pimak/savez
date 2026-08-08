#![allow(dead_code)]

//! Shared test helpers for integration tests (`tests/persistence.rs`, `tests/http_skeleton.rs`).
//! Cargo treats `tests/common/mod.rs` as a shared module, never as an additional test binary.

/// Not a real secret — used only to construct `AppState` in tests.
pub const TEST_JWT_KEY: &str = "test-jwt-key-not-a-secret";

/// Non-routable loopback port: any test that accidentally makes an oracle call fails fast instead
/// of reaching the real `api.shapez.io`. This is a test-suite safety guarantee, not a convenience
/// detail — no test may ever reach the official service (CON-official-api-usage).
pub const UNREACHABLE_ORACLE_URL: &str = "http://127.0.0.1:1";

static CRYPTO_PROVIDER_INIT: std::sync::Once = std::sync::Once::new();

/// Installs the rustls `ring` crypto provider exactly once per test binary process. `main.rs`
/// installs it at real server startup, but each `tests/*.rs` integration binary is a separate
/// process that never runs `main.rs` — without this, the first `reqwest::Client` built via
/// `savez::db::build_http_client()` in a test would panic on its first real TLS handshake attempt
/// ("No rustls crypto provider is configured"), same root cause as the 05-01 follow-up for
/// `main.rs`. `ring` is already in the dependency tree via sqlx's `tls-rustls-ring-webpki` feature.
fn ensure_crypto_provider_installed() {
    CRYPTO_PROVIDER_INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Builds an `AppState` for tests that don't care about the oracle endpoint: `auth_mode` is
/// `Oracle` and the oracle URL is deliberately unreachable (see `UNREACHABLE_ORACLE_URL`).
pub fn test_state(pool: sqlx::PgPool) -> savez::db::AppState {
    test_state_with_oracle(pool, UNREACHABLE_ORACLE_URL)
}

/// Builds an `AppState` with a caller-supplied oracle URL (used by wiremock-based tests in a later
/// plan that need the client to actually reach a mock server).
pub fn test_state_with_oracle(pool: sqlx::PgPool, oracle_url: &str) -> savez::db::AppState {
    ensure_crypto_provider_installed();
    savez::db::AppState {
        pool,
        jwt_key: TEST_JWT_KEY.to_string(),
        official_api_url: oracle_url.to_string(),
        auth_mode: savez::config::AuthMode::Oracle,
        http_client: savez::db::build_http_client().expect("test http client"),
    }
}

/// Creates a throwaway user row directly (bypassing the oracle login flow entirely) for tests
/// that only need *a* valid, existing `users.id` to build an authenticated request around. Uses a
/// dynamic query (`sqlx::query_scalar`, not the `query!` macro) deliberately: a test-only insert
/// has no business growing the versioned `.sqlx` offline cache.
pub async fn register_test_user(pool: &sqlx::PgPool, name: &str) -> uuid::Uuid {
    sqlx::query_scalar(
        "INSERT INTO users (name, verified_via) VALUES ($1, 'test-fixture') RETURNING id",
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .expect("insert test user")
}

/// Issues a server JWT for `user_id` using the same key (`TEST_JWT_KEY`) every `test_state*`
/// helper above configures `AppState.jwt_key` with, so a token minted here always validates
/// against an `AppState` built by this module.
pub fn jwt_for(user_id: uuid::Uuid) -> String {
    savez::auth::jwt::issue(
        TEST_JWT_KEY,
        user_id,
        savez::auth::jwt::DEFAULT_LIFETIME_SECS,
    )
    .expect("issue test jwt")
}
