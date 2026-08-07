use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// Shared application state threaded through the Axum `Router` via `.with_state()`.
///
/// Does NOT derive `Debug`: nothing that touches a connection pool (or, in future phases, secrets
/// like a JWT signing key) should be trivially printable — same convention as `Config`.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
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
