#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    // Local dev convenience only; production loads env vars via systemd `EnvironmentFile=`
    // (DEC-deployment-architecture). Silently no-ops if `.env` doesn't exist.
    dotenvy::dotenv().ok();

    let config = savez::config::Config::from_env().expect("invalid configuration");
    tracing::info!(port = config.port, "starting savez");

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], config.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind listener");

    tracing::info!(%addr, "listening");
    axum::serve(listener, savez::app())
        .await
        .expect("server error");
}
