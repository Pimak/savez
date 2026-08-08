mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt; // for `oneshot`
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn body_to_json(response: Response) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect response body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("response body is valid JSON")
}

async fn post_login(app: axum::Router, token: &str, name: &str) -> Response {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/v1/public/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "token": token, "name": name }).to_string(),
            ))
            .unwrap(),
    )
    .await
    .unwrap()
}

/// Mounts a `GET /v1/puzzles/list/mine` mock returning `200 []` on a fresh `MockServer`, with an
/// exact call-count expectation checked when `server` is dropped (wiremock's built-in behavior).
async fn mock_oracle_ok(expected_calls: u64) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/puzzles/list/mine"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(expected_calls)
        .mount(&server)
        .await;
    server
}

#[sqlx::test]
async fn creates_account_on_valid_oracle_token(pool: PgPool) {
    let server = mock_oracle_ok(1).await;
    let state = common::test_state_with_oracle(pool.clone(), &server.uri());
    let app = savez::app(state);

    let response = post_login(app, "valid-token", "zorg-player").await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_to_json(response).await;
    let token = body["token"].as_str().expect("response has a token field");
    assert!(!token.is_empty());

    let row: (String, String, String) =
        sqlx::query_as("SELECT name, verified_via, role FROM users WHERE name = $1")
            .bind("zorg-player")
            .fetch_one(&pool)
            .await
            .expect("created user row must exist");
    assert_eq!(
        row,
        (
            "zorg-player".to_string(),
            "official-api".to_string(),
            "user".to_string()
        )
    );
}

#[sqlx::test]
async fn calls_oracle_exactly_once(pool: PgPool) {
    let server = mock_oracle_ok(1).await;
    let state = common::test_state_with_oracle(pool.clone(), &server.uri());
    let app = savez::app(state);

    let response = post_login(app, "valid-token", "once-player").await;
    assert_eq!(response.status(), StatusCode::OK);
    // The `.expect(1)` set on the mock is verified when `server` is dropped at the end of this
    // test (CON-official-api-usage proof).
}

#[sqlx::test]
async fn rejects_invalid_oracle_token(pool: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/puzzles/list/mine"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let state = common::test_state_with_oracle(pool.clone(), &server.uri());
    let app = savez::app(state);

    let response = post_login(app, "invalid-token", "refused-player").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE name = $1")
        .bind("refused-player")
        .fetch_one(&pool)
        .await
        .expect("count query");
    assert_eq!(count, 0);

    // AppError::OracleVerificationFailed produces a bare 401 with no body -- no JWT anywhere in
    // the response.
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect response body")
        .to_bytes();
    assert!(
        bytes.is_empty(),
        "no JWT must be returned on refusal, got body: {bytes:?}"
    );
}

#[sqlx::test]
async fn rejects_on_oracle_unreachable(pool: PgPool) {
    let state = common::test_state_with_oracle(pool.clone(), common::UNREACHABLE_ORACLE_URL);
    let app = savez::app(state);

    let response = post_login(app, "any-token", "unreachable-player").await;
    // D-05: a client must not be able to distinguish an explicit refusal from an unreachable
    // oracle -- same 401 as `rejects_invalid_oracle_token`.
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE name = $1")
        .bind("unreachable-player")
        .fetch_one(&pool)
        .await
        .expect("count query");
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn rejects_duplicate_name(pool: PgPool) {
    let server = mock_oracle_ok(2).await;

    let state = common::test_state_with_oracle(pool.clone(), &server.uri());
    let first_response = post_login(savez::app(state.clone()), "token-1", "dupe-player").await;
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = post_login(savez::app(state), "token-2", "dupe-player").await;
    assert_eq!(second_response.status(), StatusCode::CONFLICT);

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE name = $1")
        .bind("dupe-player")
        .fetch_one(&pool)
        .await
        .expect("count query");
    assert_eq!(count, 1, "D-03: never a second row, never a silent merge");
}

#[sqlx::test]
async fn rejects_invalid_name_without_calling_oracle(pool: PgPool) {
    let server = mock_oracle_ok(0).await;
    let state = common::test_state_with_oracle(pool.clone(), &server.uri());
    let app = savez::app(state);

    let response = post_login(app, "any-token", "a b").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // The `.expect(0)` set on the mock is verified when `server` is dropped: no oracle call was
    // ever made for an invalid name.
}

#[sqlx::test]
async fn login_never_creates_an_admin_role(pool: PgPool) {
    let server = mock_oracle_ok(1).await;
    let state = common::test_state_with_oracle(pool.clone(), &server.uri());
    let app = savez::app(state);

    // Parasite keys "role"/"verifiedVia" are structurally without effect: serde ignores unknown
    // JSON keys on `LoginRequest`, and `insert_user` never binds a client-supplied `role` anyway.
    let body = json!({
        "token": "valid-token",
        "name": "not-an-admin",
        "role": "admin",
        "verifiedVia": "seed-admin",
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/public/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let role: String = sqlx::query_scalar("SELECT role FROM users WHERE name = $1")
        .bind("not-an-admin")
        .fetch_one(&pool)
        .await
        .expect("created user row must exist");
    assert_eq!(role, "user");
}

#[sqlx::test]
async fn non_oracle_auth_mode_returns_not_implemented(pool: PgPool) {
    let server = mock_oracle_ok(0).await;
    let mut state = common::test_state_with_oracle(pool.clone(), &server.uri());
    state.auth_mode = savez::config::AuthMode::Open;
    let app = savez::app(state);

    let response = post_login(app, "any-token", "irrelevant-name").await;
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    // The `.expect(0)` set on the mock is verified when `server` is dropped: no oracle call was
    // ever made for a non-oracle auth mode.
}
