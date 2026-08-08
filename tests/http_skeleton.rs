mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use sqlx::PgPool;
use tower::ServiceExt; // for `oneshot`

#[sqlx::test]
async fn healthz_returns_200(pool: PgPool) {
    let state = common::test_state(pool);
    let response = savez::app(state)
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[sqlx::test]
async fn list_new_returns_200_json_array(pool: PgPool) {
    let state = common::test_state(pool);
    let response = savez::app(state)
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/list/new")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/json"
    );
}

#[sqlx::test]
async fn cors_preflight_allows_expected_headers(pool: PgPool) {
    let state = common::test_state(pool);
    let response = savez::app(state)
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/v1/puzzles/list/new")
                .header("origin", "http://localhost:3005")
                .header("access-control-request-method", "GET")
                .header("access-control-request-headers", "x-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_some()
    );
}
