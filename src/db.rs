use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// Shared application state threaded through the Axum `Router` via `.with_state()`.
///
/// Does NOT derive `Debug`: `jwt_key` is a secret and must never be trivially printable — same
/// convention as `Config`. All seven fields are cheap to clone: `PgPool` and `reqwest::Client` are
/// internally `Arc`-backed, `AuthMode` is `Copy`, and `AuthCache`/`ProfanityCache` both wrap a
/// `moka::future::Cache`, itself internally `Arc`-backed.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub jwt_key: String,
    pub official_api_url: String,
    pub auth_mode: crate::config::AuthMode,
    pub http_client: reqwest::Client,
    pub auth_cache: crate::auth::cache::AuthCache,
    pub profanity_cache: crate::profanity::ProfanityCache,
}

/// TTL for the role/ban cache wrapping every protected-route read of `users.role`/`user_bans`
/// (D-09, ADR 0006). 10s is the midpoint of D-09's locked 5-15s range: no data pointed more
/// strongly either direction (07-RESEARCH.md Pattern 5), so the midpoint is the defensible,
/// non-arbitrary choice. Named constant, never an inline literal — same idiom as
/// `ORACLE_HTTP_TIMEOUT` above.
pub const AUTH_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(10);

/// TTL for the profanity word-list cache (`crate::profanity::ProfanityCache`). Six times
/// `AUTH_CACHE_TTL`: a word list carries no real-time security stakes the way a ban does (T-07-47
/// is a performance concern, not a freshness one) — it changes very rarely, and a stale window up
/// to a minute long costs nothing an attacker could exploit, unlike a stale ban (ADR 0006).
/// Sixty seconds rather than an even longer value keeps "add a word, see it enforced within a
/// minute, no restart" (07-08-PLAN.md `must_haves`) true without any cache invalidation machinery.
pub const PROFANITY_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// Bound on every outbound HTTP call to the oracle (`api.shapez.io`). The official shapez client
/// itself wraps each call in a 15s `timeoutPromise` (05-RESEARCH.md Assumptions Log A2); a shorter,
/// explicit server-side timeout is mandatory regardless — without one, a slow or hung oracle could
/// leave a login request pending indefinitely (T-05-07).
pub const ORACLE_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Builds the outbound `reqwest::Client` used for oracle verification calls, bounded by
/// `ORACLE_HTTP_TIMEOUT`. Never panics internally (fail-fast, typed-error discipline mirrored from
/// `connect`) — `main.rs` is the only place in the binary that `.expect()`s on the result.
pub fn build_http_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(ORACLE_HTTP_TIMEOUT)
        .build()
}

/// Opens a PostgreSQL connection pool. Never panics internally (fail-fast, typed-error
/// discipline mirrored from `config.rs`) — `main.rs` is the only place in the binary that
/// `.expect()`s on the result.
///
/// `acquire_timeout` is set well below sqlx's 30s default: `PgPoolOptions::connect()` opens (and
/// internally retries establishing) at least one real connection before returning, so with the
/// default 30s timeout a single unreachable-database attempt would silently retry internally for
/// up to 30 seconds — turning `main.rs`'s bounded 5-attempt/1s-apart retry loop into a multi-minute
/// hang instead of the intended fast-fail-and-retry behavior (found while verifying Task 3's
/// "fails after ~5s instead of looping indefinitely" acceptance criterion).
pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_millis(500))
        .connect(database_url)
        .await
}
