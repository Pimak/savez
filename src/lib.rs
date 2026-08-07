use axum::Router;
use axum::http::{Method, header};
use axum::routing::get;
use tower_http::cors::{Any, CorsLayer};

pub mod config;
mod routes;

pub fn app() -> Router {
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
        .layer(cors)
}
