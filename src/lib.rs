use axum::Router;
use axum::http::{Method, header};
use axum::routing::{get, post};
use tower_http::cors::{Any, CorsLayer};

pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod profanity;
pub mod ratelimit;
pub mod repository;
pub mod routes;
pub mod validation;

pub fn app(state: db::AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([
            header::CONTENT_TYPE,
            "x-token".parse().unwrap(),
            "x-api-key".parse().unwrap(),
        ]);

    Router::new()
        .route("/healthz", get(routes::health::healthz))
        .route("/v1/public/login", post(routes::auth::login))
        .route("/v1/puzzles/list/{category}", get(routes::puzzles::list))
        .route("/v1/puzzles/submit", post(routes::puzzles::submit))
        .route(
            "/v1/puzzles/download/{id_or_key}",
            get(routes::puzzles::download),
        )
        .route("/v1/puzzles/search", post(routes::puzzles::search))
        .route("/v1/puzzles/complete/{id}", post(routes::puzzles::complete))
        .route("/v1/puzzles/report/{id}", post(routes::puzzles::report))
        .route("/v1/puzzles/delete/{id}", post(routes::puzzles::delete))
        .layer(cors)
        .with_state(state)
}
