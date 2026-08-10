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

/// D-04 / ADR 0004 / `docs/adr/0004-cli-serve-subcommand.md`: before this phase, the binary had no
/// argument parsing at all — it did exactly one thing (launch the HTTP server), invoked with no
/// argument. Phase 7 splits the binary into two explicit, mutually exclusive modes of the SAME
/// process: `savez serve` (this file's former entire behavior, unchanged) and `savez mod <action>`
/// (direct database access for moderation, no running server involved — `src/cli/moderation.rs`).
///
/// `Cli::parse()` runs FIRST, before any configuration/database work: a bare `savez` (no
/// subcommand) or `savez --help` must exit immediately via clap's own usage/help output, without
/// requiring `DATABASE_URL`/`JWT_KEY`/`OFFICIAL_API_URL` to be set at all.
///
/// Both branches share one startup sequence — config load, pooled connection with retry, and
/// migrations — so this is written once, not duplicated: a fresh VPS on which an operator runs
/// `savez mod ratelimit set` before ever starting `savez serve` (e.g. deployment automation
/// seeding rate limits pre-launch) must not fail with "table does not exist." Migrations are
/// idempotent (sqlx tracks applied migrations in `_sqlx_migrations`), so running them on every
/// invocation is cheap insurance, not redundant work in the common case.
///
/// Impact to anticipate for Phase 8 (Dockerfile, systemd unit): both must now invoke `savez serve`
/// explicitly — a bare no-argument invocation no longer starts the server.
#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt::init();

    let cli = <savez::cli::Cli as clap::Parser>::parse();

    // `reqwest` is built with the `rustls-no-provider` feature (05-01 decision: avoids pulling in
    // the aws-lc-sys/cmake C build dependency) which does NOT install a default rustls
    // `CryptoProvider` automatically. `ring` is already in the dependency tree via sqlx's
    // `tls-rustls-ring-webpki` feature, so installing it here adds no new crypto backend — but
    // without this call, the first real TLS handshake made by `build_http_client()`'s client would
    // panic with "no process-level CryptoProvider available." Installed unconditionally (harmless,
    // cheap) rather than only in the `Serve` branch, to avoid a second startup code path.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install rustls ring crypto provider");

    // Local dev convenience only; production loads env vars via systemd `EnvironmentFile=`
    // (DEC-deployment-architecture). Silently no-ops if `.env` doesn't exist.
    dotenvy::dotenv().ok();

    let config = savez::config::Config::from_env().expect("invalid configuration");

    let pool = connect_with_retry(&config.database_url)
        .await
        .expect("db connect failed");

    sqlx::migrate!()
        .run(&pool)
        .await
        .expect("migrations failed");
    tracing::info!("migrations applied");

    match cli.command {
        savez::cli::Commands::Serve => {
            tracing::info!(port = config.port, "starting savez");
            tracing::info!(auth_mode = ?config.auth_mode, "auth mode");

            let http_client = savez::db::build_http_client().expect("reqwest client build");
            let auth_cache = savez::auth::cache::AuthCache::new(savez::db::AUTH_CACHE_TTL);
            let profanity_cache =
                savez::profanity::ProfanityCache::new(savez::db::PROFANITY_CACHE_TTL);
            let state = savez::db::AppState {
                pool,
                jwt_key: config.jwt_key,
                official_api_url: config.official_api_url,
                auth_mode: config.auth_mode,
                http_client,
                auth_cache,
                profanity_cache,
            };

            let addr = std::net::SocketAddr::from(([127, 0, 0, 1], config.port));
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .expect("failed to bind listener");

            tracing::info!(%addr, "listening");
            axum::serve(listener, savez::app(state))
                .await
                .expect("server error");

            std::process::ExitCode::SUCCESS
        }
        savez::cli::Commands::Mod { action } => match savez::cli::moderation::dispatch(&pool, action).await
        {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("{err}");
                std::process::ExitCode::FAILURE
            }
        },
    }
}
