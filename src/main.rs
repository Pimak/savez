/// Maximum number of connection attempts before giving up. Phase 3 does not containerize the app
/// itself (that's `DEC-deployment-architecture`'s Phase 8 scope), so nothing else guards against a
/// cold-start race with the Compose Postgres healthcheck — this bounded retry is cheap insurance.
const DB_CONNECT_MAX_ATTEMPTS: u32 = 5;
const DB_CONNECT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

/// Retries `savez::db::connect` up to `DB_CONNECT_MAX_ATTEMPTS` times, `DB_CONNECT_RETRY_DELAY`
/// apart, warning on every failed intermediate attempt. Returns the last error if every attempt
/// fails — the caller (`main`) is the only place that `.expect()`s on it.
async fn connect_with_retry(database_url: &str) -> Result<sqlx::PgPool, sqlx::Error> {
    let mut last_err = None;
    for attempt in 1..=DB_CONNECT_MAX_ATTEMPTS {
        match savez::db::connect(database_url).await {
            Ok(pool) => return Ok(pool),
            Err(err) => {
                tracing::warn!(attempt, max_attempts = DB_CONNECT_MAX_ATTEMPTS, error = %err, "db connect attempt failed");
                last_err = Some(err);
                if attempt < DB_CONNECT_MAX_ATTEMPTS {
                    tokio::time::sleep(DB_CONNECT_RETRY_DELAY).await;
                }
            }
        }
    }
    Err(last_err.expect("at least one connect attempt was made"))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // Local dev convenience only; production loads env vars via systemd `EnvironmentFile=`
    // (DEC-deployment-architecture). Silently no-ops if `.env` doesn't exist.
    dotenvy::dotenv().ok();

    let config = savez::config::Config::from_env().expect("invalid configuration");
    tracing::info!(port = config.port, "starting savez");

    let pool = connect_with_retry(&config.database_url)
        .await
        .expect("db connect failed");

    sqlx::migrate!()
        .run(&pool)
        .await
        .expect("migrations failed");
    tracing::info!("migrations applied");

    let state = savez::db::AppState { pool };

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], config.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind listener");

    tracing::info!(%addr, "listening");
    axum::serve(listener, savez::app(state))
        .await
        .expect("server error");
}
