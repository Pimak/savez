#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // Local dev convenience only; production loads env vars via systemd `EnvironmentFile=`
    // (DEC-deployment-architecture). Silently no-ops if `.env` doesn't exist.
    dotenvy::dotenv().ok();

    let config = savez::config::Config::from_env().expect("invalid configuration");
    tracing::info!(port = config.port, "starting savez");

    // NOTE: bounded retry loop around this connect + `sqlx::migrate!()` application arrives in
    // plan 03-02 Task 3 (Phase 3 does not containerize the app yet, so nothing guards against a
    // cold-start race against the Compose Postgres healthcheck).
    let pool = savez::db::connect(&config.database_url)
        .await
        .expect("db connect failed");
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
