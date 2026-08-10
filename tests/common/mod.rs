#![allow(dead_code)]

//! Shared test helpers for integration tests (`tests/persistence.rs`, `tests/http_skeleton.rs`).
//! Cargo treats `tests/common/mod.rs` as a shared module, never as an additional test binary.

/// Not a real secret — used only to construct `AppState` in tests.
pub const TEST_JWT_KEY: &str = "test-jwt-key-not-a-secret";

/// Non-routable loopback port: any test that accidentally makes an oracle call fails fast instead
/// of reaching the real `api.shapez.io`. This is a test-suite safety guarantee, not a convenience
/// detail — no test may ever reach the official service (CON-official-api-usage).
pub const UNREACHABLE_ORACLE_URL: &str = "http://127.0.0.1:1";

/// A deliberately tiny TTL for every `AppState` built by `test_state`/`test_state_with_oracle`.
/// Without this, a test that bans a user directly in `user_bans` and then immediately calls a
/// route through the same `AppState` would observe a stale pre-ban cache entry (D-09's whole
/// point) and become flaky/order-dependent. The cache mechanism itself (D-09/D-10's TTL-bounded
/// staleness) is proven separately by a dedicated integration test file, which builds its own
/// cache wrapper at a real TTL.
pub const TEST_AUTH_CACHE_TTL: std::time::Duration = std::time::Duration::from_millis(1);

/// Same rationale as `TEST_AUTH_CACHE_TTL`, applied to the profanity word-list cache: a test that
/// adds/removes a word via `profanity::add_word`/`remove_word` and immediately submits a title
/// through the same `AppState` must observe the change without waiting out
/// `db::PROFANITY_CACHE_TTL`'s real 60s window.
pub const TEST_PROFANITY_CACHE_TTL: std::time::Duration = std::time::Duration::from_millis(1);

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
        auth_cache: savez::auth::cache::AuthCache::new(TEST_AUTH_CACHE_TTL),
        profanity_cache: savez::profanity::ProfanityCache::new(TEST_PROFANITY_CACHE_TTL),
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

/// Same as `register_test_user`, but inserts a specific `role` directly — the `users_role_check`
/// CHECK constraint (D-12, migration 20260810000002) still validates the value either way, so an
/// invalid `role` argument fails the same as it would through any other write path.
pub async fn register_test_user_with_role(
    pool: &sqlx::PgPool,
    name: &str,
    role: &str,
) -> uuid::Uuid {
    sqlx::query_scalar(
        "INSERT INTO users (name, verified_via, role) VALUES ($1, 'test-fixture', $2) RETURNING id",
    )
    .bind(name)
    .bind(role)
    .fetch_one(pool)
    .await
    .expect("insert test user with role")
}

/// Removes every configured rate-limit threshold, so `src/ratelimit.rs::check_and_record` finds
/// zero rows for any class and returns `Ok(())` immediately -- an unconfigured class imposes no
/// limit at all (this is the exact same behavior a fresh, never-configured deployment would have,
/// not a test-only bypass). Call this ONLY in tests that intentionally exceed the migration's
/// seeded defaults (`20260810000003_rate_limiting.sql`: 5/h + 20/j write, 500/h read) -- never
/// raise the seeded default values themselves to make a test pass; this helper is the one
/// sanctioned lever for that.
pub async fn relax_rate_limits(pool: &sqlx::PgPool) {
    sqlx::query("DELETE FROM rate_limit_config")
        .execute(pool)
        .await
        .expect("relax rate limits");
}

/// Inserts a row directly into `user_bans` (bypassing any CLI/HTTP ban flow entirely, none of
/// which exist yet in this plan) and returns its `id` — needed by a later plan's ban-lifting
/// tests. `expires_at: None` produces a permanent ban; `Some(...)` a temporary one, mirroring
/// D-11's derivation (`lifted_at IS NULL AND (expires_at IS NULL OR expires_at > now())`).
pub async fn ban_test_user(
    pool: &sqlx::PgPool,
    user_id: uuid::Uuid,
    moderator_id: uuid::Uuid,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
) -> i32 {
    sqlx::query_scalar(
        "INSERT INTO user_bans (user_id, reason, moderator_id, expires_at) VALUES ($1, 'test-fixture', $2, $3) RETURNING id",
    )
    .bind(user_id)
    .bind(moderator_id)
    .bind(expires_at)
    .fetch_one(pool)
    .await
    .expect("insert test ban")
}
