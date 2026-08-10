mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt; // for `oneshot`
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// D-13's exact scope, proven in both directions in this file: `login`, `submit`, `complete` and
// `report` are the four routes blocked for a banned account; every other route -- `list`,
// `search`, `download`, and `delete` -- stays fully available. `delete` is deliberately in the
// "stays open" group: D-13 and SPEC §4.6 both name only four blocked routes, and removing one's
// own content is not an act of harm (see `src/routes/puzzles.rs::delete`'s doc comment).

async fn body_to_json(response: axum::response::Response) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect response body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("response body is valid JSON")
}

/// D-17: every blocking assertion below checks BOTH halves of the contract -- HTTP 200, never a
/// 401/403, and the exact literal body `{"error":"banned"}`, never some other taxonomy code that
/// would let a caller confuse a ban with an unrelated rejection.
async fn assert_banned(response: axum::response::Response) {
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_to_json(response).await;
    assert_eq!(body, json!({ "error": "banned" }));
}

fn sample_game_data() -> Value {
    json!({
        "version": 1,
        "bounds": { "w": 10, "h": 8 },
        "buildings": [
            { "type": "emitter", "item": "CuCuCuCu", "pos": { "x": 0, "y": 0, "r": 0 } },
            { "type": "goal", "item": "CuCuCuCu", "pos": { "x": 4, "y": 3, "r": 90 } },
        ],
        "excludedBuildings": [],
    })
}

async fn submit_puzzle(
    app: axum::Router,
    token: &str,
    short_key: &str,
) -> axum::response::Response {
    let body = json!({
        "title": "Ban Test Puzzle",
        "shortKey": short_key,
        "data": sample_game_data().to_string(),
    });
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/v1/puzzles/submit")
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-token", token)
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn complete_puzzle(app: axum::Router, token: &str, id: &str) -> axum::response::Response {
    let body = json!({ "time": 42.0, "liked": false });
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(format!("/v1/puzzles/complete/{id}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-token", token)
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn report_puzzle(app: axum::Router, token: &str, id: &str) -> axum::response::Response {
    let body = json!({ "reason": "profane" });
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(format!("/v1/puzzles/report/{id}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-token", token)
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn delete_puzzle(app: axum::Router, token: &str, id: &str) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(format!("/v1/puzzles/delete/{id}"))
            .header("x-token", token)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn list_new(app: axum::Router, token: &str) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .uri("/v1/puzzles/list/new")
            .header("x-token", token)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn search(app: axum::Router, token: &str) -> axum::response::Response {
    let body = json!({ "searchTerm": "", "difficulty": "any", "duration": "any" });
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/v1/puzzles/search")
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-token", token)
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn download(app: axum::Router, token: &str, id_or_key: &str) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .uri(format!("/v1/puzzles/download/{id_or_key}"))
            .header("x-token", token)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

/// D-13: `submit` is blocked for a banned account.
#[sqlx::test]
async fn banned_user_cannot_submit(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "submit-banned").await;
    let moderator_id = common::register_test_user(&pool, "submit-banned-mod").await;
    common::ban_test_user(&pool, author_id, moderator_id, None).await;
    let token = common::jwt_for(author_id);

    let response = submit_puzzle(app, &token, "WwWwWwWw").await;
    assert_banned(response).await;
}

/// D-13: `complete` is blocked for a banned account.
#[sqlx::test]
async fn banned_user_cannot_complete(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "complete-banned-author").await;
    let author_token = common::jwt_for(author_id);
    let submitted = body_to_json(submit_puzzle(app.clone(), &author_token, "CcCcCcCc").await).await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number");

    let user_id = common::register_test_user(&pool, "complete-banned-user").await;
    let moderator_id = common::register_test_user(&pool, "complete-banned-mod").await;
    common::ban_test_user(&pool, user_id, moderator_id, None).await;
    let token = common::jwt_for(user_id);

    let response = complete_puzzle(app, &token, &puzzle_id.to_string()).await;
    assert_banned(response).await;
}

/// D-13: `report` is blocked for a banned account.
#[sqlx::test]
async fn banned_user_cannot_report(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "report-banned-author").await;
    let author_token = common::jwt_for(author_id);
    let submitted = body_to_json(submit_puzzle(app.clone(), &author_token, "CbCbCbCb").await).await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number");

    let user_id = common::register_test_user(&pool, "report-banned-user").await;
    let moderator_id = common::register_test_user(&pool, "report-banned-mod").await;
    common::ban_test_user(&pool, user_id, moderator_id, None).await;
    let token = common::jwt_for(user_id);

    let response = report_puzzle(app, &token, &puzzle_id.to_string()).await;
    assert_banned(response).await;
}

/// D-13/T-07-18: `login` is blocked for a banned pseudo, and -- crucially -- the block happens
/// BEFORE the oracle is ever called. The wiremock server below would answer 200 (proving ownership)
/// if it received a request at all; `.expect(0)` fails the test on drop if it received even one.
#[sqlx::test]
async fn banned_name_cannot_login(pool: PgPool) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/puzzles/list/mine"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(0)
        .mount(&server)
        .await;

    let state = common::test_state_with_oracle(pool.clone(), &server.uri());
    let app = savez::app(state);

    let moderator_id = common::register_test_user(&pool, "login-banned-mod").await;
    let banned_user_id = common::register_test_user(&pool, "login-banned-player").await;
    common::ban_test_user(&pool, banned_user_id, moderator_id, None).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/public/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "token": "would-be-valid-token", "name": "login-banned-player" })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_banned(response).await;

    // `server`'s `.expect(0)` is verified again when it is dropped here: this is the explicit,
    // second proof (beyond the mount-time expectation) that zero requests reached the oracle.
    assert_eq!(
        server
            .received_requests()
            .await
            .expect("mock server logs requests")
            .len(),
        0,
        "a banned pseudo must never consume an oracle call"
    );
}

/// D-13: reading stays fully available to a banned account -- `list/new`, `search` and `download`
/// all respond normally, none of them ever produce the `banned` code.
#[sqlx::test]
async fn banned_user_can_still_read(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "read-banned-author").await;
    let author_token = common::jwt_for(author_id);
    let submitted = body_to_json(submit_puzzle(app.clone(), &author_token, "SwSwSwSw").await).await;
    let short_key = submitted["shortKey"]
        .as_str()
        .expect("submitted shortKey is a string")
        .to_string();

    let moderator_id = common::register_test_user(&pool, "read-banned-mod").await;
    let user_id = common::register_test_user(&pool, "read-banned-user").await;
    common::ban_test_user(&pool, user_id, moderator_id, None).await;
    let token = common::jwt_for(user_id);

    let list_response = list_new(app.clone(), &token).await;
    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = body_to_json(list_response).await;
    assert!(
        list_body.as_array().is_some(),
        "list/new must still respond with a JSON array for a banned account"
    );

    let search_response = search(app.clone(), &token).await;
    assert_eq!(search_response.status(), StatusCode::OK);
    let search_body = body_to_json(search_response).await;
    assert!(
        search_body.as_array().is_some(),
        "search must still respond with a JSON array for a banned account"
    );

    let download_response = download(app, &token, &short_key).await;
    assert_eq!(download_response.status(), StatusCode::OK);
    let download_body = body_to_json(download_response).await;
    assert!(
        download_body.get("meta").is_some() && download_body.get("game").is_some(),
        "download must still resolve normally for a banned account"
    );
}

/// D-13: a banned author can still remove their own content -- `delete` is deliberately outside
/// D-13's four-route blocklist.
#[sqlx::test]
async fn banned_author_can_still_delete_own_puzzle(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "delete-banned-author").await;
    let author_token = common::jwt_for(author_id);
    let submitted = body_to_json(submit_puzzle(app.clone(), &author_token, "SpSpSpSp").await).await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number");

    let moderator_id = common::register_test_user(&pool, "delete-banned-mod").await;
    common::ban_test_user(&pool, author_id, moderator_id, None).await;

    let response = delete_puzzle(app, &author_token, &puzzle_id.to_string()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_to_json(response).await;
    assert_eq!(body, json!({ "success": true }));
}

/// D-11: an `expires_at` in the past no longer applies -- `submit` succeeds for that account.
#[sqlx::test]
async fn expired_ban_does_not_block(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "expired-ban-user").await;
    let moderator_id = common::register_test_user(&pool, "expired-ban-mod").await;
    let past = chrono::Utc::now() - chrono::Duration::hours(1);
    common::ban_test_user(&pool, author_id, moderator_id, Some(past)).await;
    let token = common::jwt_for(author_id);

    let response = submit_puzzle(app, &token, "SySySySy").await;
    assert_eq!(response.status(), StatusCode::OK);
}

/// D-11: a ban whose `lifted_at` is set no longer applies -- `submit` succeeds for that account.
#[sqlx::test]
async fn lifted_ban_does_not_block(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "lifted-ban-user").await;
    let moderator_id = common::register_test_user(&pool, "lifted-ban-mod").await;
    let ban_id = common::ban_test_user(&pool, author_id, moderator_id, None).await;
    sqlx::query("UPDATE user_bans SET lifted_at = now() WHERE id = $1")
        .bind(ban_id)
        .execute(&pool)
        .await
        .expect("lifting the ban must succeed");
    let token = common::jwt_for(author_id);

    let response = submit_puzzle(app, &token, "CyCyCyCy").await;
    assert_eq!(response.status(), StatusCode::OK);
}

/// D-11: a permanent ban (`expires_at IS NULL`) blocks `submit` indefinitely.
#[sqlx::test]
async fn permanent_ban_blocks(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "permanent-ban-user").await;
    let moderator_id = common::register_test_user(&pool, "permanent-ban-mod").await;
    common::ban_test_user(&pool, author_id, moderator_id, None).await;
    let token = common::jwt_for(author_id);

    let response = submit_puzzle(app, &token, "CpCpCpCp").await;
    assert_banned(response).await;
}

/// D-11: multiple `user_bans` rows may coexist for one account -- lifting one of them leaves the
/// account blocked as long as at least one other active ban remains.
#[sqlx::test]
async fn second_active_ban_coexists(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "coexist-ban-user").await;
    let moderator_id = common::register_test_user(&pool, "coexist-ban-mod").await;
    let first_ban_id = common::ban_test_user(&pool, author_id, moderator_id, None).await;
    let _second_ban_id = common::ban_test_user(&pool, author_id, moderator_id, None).await;
    let token = common::jwt_for(author_id);

    // Both bans active: blocked.
    let response = submit_puzzle(app.clone(), &token, "RpRpRpRp").await;
    assert_banned(response).await;

    // Lift only the first ban directly.
    sqlx::query("UPDATE user_bans SET lifted_at = now() WHERE id = $1")
        .bind(first_ban_id)
        .execute(&pool)
        .await
        .expect("lifting the first ban must succeed");

    // `test_state`'s 1ms auth cache TTL (tests/common/mod.rs) means the first submit above already
    // cached this user's banned state; without letting the TTL elapse, the second submit below
    // could observe that stale cached entry instead of a fresh read reflecting the still-active
    // second ban -- same precaution `tests/auth_cache.rs::role_change_is_visible_after_ttl_expiry`
    // takes for the same 1ms-TTL reason.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // The second ban is still active: still blocked.
    let response = submit_puzzle(app, &token, "RcRcRcRc").await;
    assert_banned(response).await;
}
