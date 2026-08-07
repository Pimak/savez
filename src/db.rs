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
pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
}
