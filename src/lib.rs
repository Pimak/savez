use axum::Router;
use axum::http::{Method, header};
use axum::routing::{get, post};
use tower_http::cors::{Any, CorsLayer};

pub mod config;
pub mod db;
pub mod error;
pub mod repository;
pub mod routes;

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
        .route("/v1/puzzles/list/new", get(routes::puzzles::list_new))
        .route("/v1/puzzles/submit", post(routes::puzzles::submit))
        .route(
            "/v1/puzzles/download/{id_or_key}",
            get(routes::puzzles::download),
        )
        .layer(cors)
        .with_state(state)
}
