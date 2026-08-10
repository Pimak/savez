use axum::Router;
use axum::http::{Method, header};
use axum::routing::{get, post};
use tower_http::cors::{Any, CorsLayer};

pub mod auth;
pub mod cli;
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
        .allow_methods([
            Method::GET,
            Method::POST,
            // 07-10-PLAN.md: the ONLY non-GET/POST route in this project is the permanent-delete
            // moderation route below (`purge`, admin-only) -- outside the shapez client contract
            // entirely (which never uses DELETE, per D-08/D-09/D-11's literal POST-based delete).
            Method::DELETE,
        ])
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
        // Moderation surface (SPEC §4.6, REQ-moderation, 07-10-PLAN.md) -- eight routes below,
        // table reproduced verbatim in that plan's `<interfaces>`. Deliberately no route for role
        // promotion (CLI-only, 07-06/T-07-28) and no rate limiting (07-07/T-07-37).
        .route(
            "/v1/moderation/reports",
            get(routes::moderation::list_reports),
        )
        .route(
            "/v1/moderation/reports/{id}/resolve",
            post(routes::moderation::resolve_report),
        )
        .route(
            "/v1/moderation/puzzles/{id}/hide",
            post(routes::moderation::hide),
        )
        .route(
            "/v1/moderation/puzzles/{id}/unhide",
            post(routes::moderation::unhide),
        )
        .route(
            "/v1/moderation/puzzles/{id}",
            axum::routing::delete(routes::moderation::purge),
        )
        .route(
            "/v1/moderation/users/{id}/ban",
            post(routes::moderation::ban),
        )
        .route(
            "/v1/moderation/users/{id}/lift-ban",
            post(routes::moderation::lift_ban),
        )
        .route("/v1/moderation/log", get(routes::moderation::log))
        .layer(cors)
        .with_state(state)
}
