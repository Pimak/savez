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

/// Minimal, always-decodable `POST /v1/puzzles/submit` body (Community Edition raw-JSON wire
/// format, D-08) — this file only exercises the auth boundary, not `decode_puzzle_data` itself
/// (covered by `tests/persistence.rs` and `src/routes/puzzles.rs`'s own unit tests).
fn sample_puzzle_body(short_key: &str) -> Value {
    json!({
        "title": "Auth Test Puzzle",
        "shortKey": short_key,
        "data": json!({
            "version": 1,
            "bounds": { "w": 10, "h": 8 },
            "buildings": [],
            "excludedBuildings": [],
        })
        .to_string(),
    })
}

/// SC3 (ROADMAP): a protected route validates the server JWT's signature and expiration only —
/// it never makes a second round-trip to the oracle. The mock's `.expect(1)` covers the login
/// call; if `submit` triggered even one more oracle call, the mock would fail on drop.
#[sqlx::test]
async fn protected_route_does_not_call_oracle(pool: PgPool) {
    let server = mock_oracle_ok(1).await;
    let state = common::test_state_with_oracle(pool.clone(), &server.uri());
    let app = savez::app(state);

    let login_response = post_login(app.clone(), "valid-token", "oracle-once-player").await;
    assert_eq!(login_response.status(), StatusCode::OK);
    let login_body = body_to_json(login_response).await;
    let jwt = login_body["token"]
        .as_str()
        .expect("login response has a token")
        .to_string();

    let submit_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/puzzles/submit")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-token", &jwt)
                .body(Body::from(
                    sample_puzzle_body("protected-route-1").to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(submit_response.status(), StatusCode::OK);
    // `server`'s `.expect(1)` is verified when it is dropped at the end of this test: the oracle
    // saw exactly the one call made during login, never one for the subsequent submit.
}

#[sqlx::test]
async fn submit_without_token_is_401(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/puzzles/submit")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(sample_puzzle_body("no-token-1").to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM puzzles")
        .fetch_one(&pool)
        .await
        .expect("count query");
    assert_eq!(count, 0);
}

/// T-05-10: a valid JWT placed anywhere other than `x-token` must not authenticate — neither
/// `Authorization: Bearer` nor `x-api-key` (the real client's own, application-identifying
/// header) may substitute for it.
#[sqlx::test]
async fn submit_rejects_bearer_and_api_key(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let user_id = common::register_test_user(&pool, "bearer-test-user").await;
    let token = common::jwt_for(user_id);

    let bearer_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/puzzles/submit")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::from(sample_puzzle_body("bearer-1").to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bearer_response.status(), StatusCode::UNAUTHORIZED);

    let api_key_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/puzzles/submit")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-api-key", &token)
                .body(Body::from(sample_puzzle_body("api-key-1").to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(api_key_response.status(), StatusCode::UNAUTHORIZED);

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM puzzles")
        .fetch_one(&pool)
        .await
        .expect("count query");
    assert_eq!(count, 0);
}

/// T-05-05: the author is always the JWT holder (user A), never a value the client asserts in the
/// body (here, user B's id under a parasite `authorId` key) — mirrors T-03-21's precedent.
#[sqlx::test]
async fn submit_attributes_puzzle_to_jwt_user(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let user_a = common::register_test_user(&pool, "user-a").await;
    let user_b = common::register_test_user(&pool, "user-b").await;
    let token_a = common::jwt_for(user_a);

    let mut body = sample_puzzle_body("attribution-1");
    body["authorId"] = json!(user_b.to_string());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/puzzles/submit")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-token", &token_a)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let author_id: String =
        sqlx::query_scalar("SELECT author_id::text FROM puzzles WHERE short_key = $1")
            .bind("attribution-1")
            .fetch_one(&pool)
            .await
            .expect("submitted puzzle row must exist");
    assert_eq!(author_id, user_a.to_string());
}

/// T-05-25: a JWT can be correctly signed and unexpired yet still name a user that no longer (or
/// never did) exist in `users` — the `puzzles.author_id` foreign key rejects the write, proving a
/// signed token alone is not sufficient to create data for an arbitrary `sub`.
#[sqlx::test]
async fn submit_with_jwt_of_unknown_user_is_rejected(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let unknown_user_id = uuid::Uuid::new_v4();
    let token = common::jwt_for(unknown_user_id);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/puzzles/submit")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-token", &token)
                .body(Body::from(sample_puzzle_body("unknown-user-1").to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM puzzles WHERE short_key = $1")
        .bind("unknown-user-1")
        .fetch_one(&pool)
        .await
        .expect("count query");
    assert_eq!(count, 0);
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

/// D-08/SC4: no route anywhere in the router creates an administrator. These three paths are
/// unregistered in `src/lib.rs`'s `Router::new()` — axum's default fallback answers 404 for any
/// unmatched route, which is itself the proof: there is no public surface to even attempt.
#[sqlx::test]
async fn no_public_admin_creation_route(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    for uri in [
        "/v1/public/admin",
        "/v1/admin/users",
        "/v1/public/register-admin",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "uri={uri}");
    }
}

/// D-08/T-05-29: the seed migration occupies `name = 'admin'` (see
/// migrations/20260808000001_seed_first_admin.sql), so `UNIQUE(name)` makes that pseudo
/// unreachable through the public login/registration path — even with a fully successful oracle
/// verification, the attempt is refused and no second row is created or merged.
#[sqlx::test]
async fn admin_name_is_reserved_by_seed(pool: PgPool) {
    let server = mock_oracle_ok(1).await;
    let state = common::test_state_with_oracle(pool.clone(), &server.uri());
    let app = savez::app(state);

    let response = post_login(app, "valid-token", "admin").await;
    assert_eq!(response.status(), StatusCode::CONFLICT);

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE name = $1")
        .bind("admin")
        .fetch_one(&pool)
        .await
        .expect("count query");
    assert_eq!(
        count, 1,
        "only the seed row must exist for name='admin', never a second one"
    );
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
