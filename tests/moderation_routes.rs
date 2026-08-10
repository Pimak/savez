//! 07-10-PLAN.md: role x route authorization matrix for `/v1/moderation/*`, plus the two
//! escalation gaps this plan closes (T-07-54 permanent-ban-by-moderator, T-07-55
//! moderator-bans-a-peer-or-superior) and the functional behaviors `<interfaces>` locks (report
//! queue default/`all` filtering, `bad-payload` on a non-numeric `limit`, D-08 grouped resolution
//! surfaced over HTTP, `purge`'s freed `shortKey`, and the deliberate absence of rate limiting on
//! this whole route family).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{Duration, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt; // for `oneshot`
use uuid::Uuid;

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

async fn body_to_json(response: axum::response::Response) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect response body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("response body is valid JSON")
}

/// D-17: every rejection in this codebase answers HTTP 200 with `{ "error": "<code>" }` — never a
/// bare 401/403. Every assertion below checks BOTH halves.
async fn assert_error_code(response: axum::response::Response, expected_code: &str) {
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_to_json(response).await,
        json!({ "error": expected_code })
    );
}

/// Generic request builder shared by every case below — an optional `x-token` header and an
/// optional JSON body, mirroring `send`-style helpers already established in `tests/bans.rs`/
/// `tests/moderation.rs` but generalized over method/path since this file's matrix spans eight
/// distinct routes.
async fn send(
    app: axum::Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        builder = builder.header("x-token", token);
    }
    let body = match &body {
        Some(v) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    app.oneshot(builder.body(body).unwrap()).await.unwrap()
}

async fn submit_puzzle(app: axum::Router, token: &str, short_key: &str, title: &str) -> Value {
    let body = json!({
        "title": title,
        "shortKey": short_key,
        "data": sample_game_data().to_string(),
    });
    let response = send(app, "POST", "/v1/puzzles/submit", Some(token), Some(body)).await;
    assert_eq!(response.status(), StatusCode::OK);
    body_to_json(response).await
}

async fn report_puzzle(
    app: axum::Router,
    token: &str,
    puzzle_id: i64,
    reason: &str,
) -> axum::response::Response {
    send(
        app,
        "POST",
        &format!("/v1/puzzles/report/{puzzle_id}"),
        Some(token),
        Some(json!({ "reason": reason })),
    )
    .await
}

/// `POST /v1/puzzles/report/:id` returns only `{"success": true}` (no report id) — this reads the
/// row this plan's own `report_puzzle` call above just created directly, the same pattern
/// `tests/moderation.rs::report_rows_for` already established for the same reason.
async fn latest_report_id(pool: &PgPool, puzzle_id: i64, reason: &str) -> i32 {
    sqlx::query_scalar::<_, i32>(
        "SELECT id FROM puzzle_reports WHERE puzzle_id = $1 AND reason = $2 ORDER BY id DESC LIMIT 1",
    )
    .bind(puzzle_id as i32)
    .bind(reason)
    .fetch_one(pool)
    .await
    .expect("report row exists")
}

async fn ban_count_for(pool: &PgPool, user_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM user_bans WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(pool)
        .await
        .expect("count user_bans")
}

/// The eight `/v1/moderation/*` routes (SPEC §4.6, this plan's `<interfaces>`), built against
/// placeholder identifiers: every rejection proven against this table happens at the
/// `ModeratorUser`/`AdminUser` extractor layer, strictly BEFORE any path segment is resolved
/// against real data, so the referenced puzzle/report/user/ban rows never need to exist.
fn all_moderation_routes() -> Vec<(&'static str, String, Option<Value>)> {
    let placeholder_user = Uuid::new_v4();
    vec![
        ("GET", "/v1/moderation/reports".to_string(), None),
        (
            "POST",
            "/v1/moderation/reports/1/resolve".to_string(),
            Some(json!({ "status": "upheld" })),
        ),
        ("POST", "/v1/moderation/puzzles/1/hide".to_string(), None),
        ("POST", "/v1/moderation/puzzles/1/unhide".to_string(), None),
        ("DELETE", "/v1/moderation/puzzles/1".to_string(), None),
        (
            "POST",
            format!("/v1/moderation/users/{placeholder_user}/ban"),
            Some(json!({ "reason": "test" })),
        ),
        (
            "POST",
            "/v1/moderation/users/1/lift-ban".to_string(),
            Some(json!({ "reason": "test" })),
        ),
        ("GET", "/v1/moderation/log".to_string(), None),
    ]
}

/// T-07-56: a `user` account is refused on every single one of the eight routes. Iterating over
/// `all_moderation_routes()` (rather than hand-writing eight call sites) means a future route
/// added to that table without role protection fails this test immediately.
#[sqlx::test]
async fn plain_user_is_refused_on_every_moderation_route(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let user_id = common::register_test_user_with_role(&pool, "plain-user", "user").await;
    let token = common::jwt_for(user_id);

    let routes = all_moderation_routes();
    assert!(routes.len() >= 8, "route table must cover all eight routes");

    for (method, path, body) in routes {
        let app = savez::app(state.clone());
        let response = send(app, method, &path, Some(&token), body).await;
        assert_error_code(response, "no-permission").await;
    }
}

/// Every route above answers `unauthorized` with no `x-token` at all — same table, no auth header.
#[sqlx::test]
async fn missing_token_is_unauthorized(pool: PgPool) {
    let state = common::test_state(pool.clone());

    for (method, path, body) in all_moderation_routes() {
        let app = savez::app(state.clone());
        let response = send(app, method, &path, None, body).await;
        assert_error_code(response, "unauthorized").await;
    }
}

/// A `moderator` is refused on the three admin-only routes: permanent delete, lift-ban, and the
/// audit log.
#[sqlx::test]
async fn moderator_is_refused_on_admin_routes(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let moderator_id =
        common::register_test_user_with_role(&pool, "mod-refused-admin", "moderator").await;
    let token = common::jwt_for(moderator_id);

    let admin_only = [
        ("DELETE", "/v1/moderation/puzzles/1".to_string(), None),
        (
            "POST",
            "/v1/moderation/users/1/lift-ban".to_string(),
            Some(json!({ "reason": "test" })),
        ),
        ("GET", "/v1/moderation/log".to_string(), None),
    ];

    for (method, path, body) in admin_only {
        let app = savez::app(state.clone());
        let response = send(app, method, &path, Some(&token), body).await;
        assert_error_code(response, "no-permission").await;
    }
}

/// The four moderator-level routes respond normally (not `no-permission`) for a real `moderator`
/// account against real resources.
#[sqlx::test]
async fn moderator_is_accepted_on_moderator_routes(pool: PgPool) {
    let state = common::test_state(pool.clone());

    let author_id = common::register_test_user(&pool, "mod-ok-author").await;
    let author_token = common::jwt_for(author_id);
    let app = savez::app(state.clone());
    let submitted = submit_puzzle(app, &author_token, "RgRgRgRg", "Mod Route Test").await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number");

    let reporter_id = common::register_test_user(&pool, "mod-ok-reporter").await;
    let reporter_token = common::jwt_for(reporter_id);
    let app = savez::app(state.clone());
    let report_response = report_puzzle(app, &reporter_token, puzzle_id, "profane").await;
    assert_eq!(report_response.status(), StatusCode::OK);
    let report_id = latest_report_id(&pool, puzzle_id, "profane").await;

    let moderator_id =
        common::register_test_user_with_role(&pool, "mod-ok-moderator", "moderator").await;
    let token = common::jwt_for(moderator_id);

    let app = savez::app(state.clone());
    let response = send(app, "GET", "/v1/moderation/reports", Some(&token), None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body_to_json(response).await.is_array());

    let app = savez::app(state.clone());
    let response = send(
        app,
        "POST",
        &format!("/v1/moderation/reports/{report_id}/resolve"),
        Some(&token),
        Some(json!({ "status": "upheld" })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_to_json(response).await["success"], json!(true));

    let app = savez::app(state.clone());
    let response = send(
        app,
        "POST",
        &format!("/v1/moderation/puzzles/{puzzle_id}/hide"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_to_json(response).await, json!({ "success": true }));

    let app = savez::app(state.clone());
    let response = send(
        app,
        "POST",
        &format!("/v1/moderation/puzzles/{puzzle_id}/unhide"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_to_json(response).await, json!({ "success": true }));
}

/// An `admin` account passes on every one of the eight routes (cumulative rights): the same
/// sequence `moderator_is_accepted_on_moderator_routes` proves, plus the three admin-only routes.
#[sqlx::test]
async fn admin_is_accepted_everywhere(pool: PgPool) {
    let state = common::test_state(pool.clone());

    let author_id = common::register_test_user(&pool, "admin-ok-author").await;
    let author_token = common::jwt_for(author_id);
    let app = savez::app(state.clone());
    let submitted = submit_puzzle(app, &author_token, "CbCbCbCb", "Admin Route Test").await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number");
    let short_key = submitted["shortKey"]
        .as_str()
        .expect("submitted shortKey is a string")
        .to_string();

    let reporter_id = common::register_test_user(&pool, "admin-ok-reporter").await;
    let reporter_token = common::jwt_for(reporter_id);
    let app = savez::app(state.clone());
    let report_response = report_puzzle(app, &reporter_token, puzzle_id, "profane").await;
    assert_eq!(report_response.status(), StatusCode::OK);
    let report_id = latest_report_id(&pool, puzzle_id, "profane").await;

    let admin_id = common::register_test_user_with_role(&pool, "admin-ok-admin", "admin").await;
    let token = common::jwt_for(admin_id);

    let app = savez::app(state.clone());
    let response = send(app, "GET", "/v1/moderation/reports", Some(&token), None).await;
    assert_eq!(response.status(), StatusCode::OK);

    let app = savez::app(state.clone());
    let response = send(
        app,
        "POST",
        &format!("/v1/moderation/reports/{report_id}/resolve"),
        Some(&token),
        Some(json!({ "status": "upheld" })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let app = savez::app(state.clone());
    let response = send(
        app,
        "POST",
        &format!("/v1/moderation/puzzles/{puzzle_id}/hide"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let app = savez::app(state.clone());
    let response = send(
        app,
        "POST",
        &format!("/v1/moderation/puzzles/{puzzle_id}/unhide"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let app = savez::app(state.clone());
    let response = send(
        app,
        "DELETE",
        &format!("/v1/moderation/puzzles/{puzzle_id}"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_to_json(response).await,
        json!({ "success": true, "freedShortKey": short_key })
    );

    let ban_target_id = common::register_test_user(&pool, "admin-ok-ban-target").await;
    let app = savez::app(state.clone());
    let response = send(
        app,
        "POST",
        &format!("/v1/moderation/users/{ban_target_id}/ban"),
        Some(&token),
        Some(json!({ "reason": "admin permanent ban" })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let ban_body = body_to_json(response).await;
    assert_eq!(ban_body["success"], json!(true));
    let ban_id = ban_body["banId"].as_i64().expect("banId is a number");

    let app = savez::app(state.clone());
    let response = send(
        app,
        "POST",
        &format!("/v1/moderation/users/{ban_id}/lift-ban"),
        Some(&token),
        Some(json!({ "reason": "admin lifts own ban" })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_to_json(response).await, json!({ "success": true }));

    let app = savez::app(state);
    let response = send(app, "GET", "/v1/moderation/log", Some(&token), None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body_to_json(response).await.is_array());
}

/// T-07-54: a `moderator` requesting a PERMANENT ban (no `expiresAt`) is refused, and — the other
/// half of the contract — no `user_bans` row is created either.
#[sqlx::test]
async fn moderator_cannot_issue_permanent_ban(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let moderator_id =
        common::register_test_user_with_role(&pool, "mod-perm-ban-mod", "moderator").await;
    let token = common::jwt_for(moderator_id);
    let target_id = common::register_test_user(&pool, "mod-perm-ban-target").await;

    let app = savez::app(state.clone());
    let response = send(
        app,
        "POST",
        &format!("/v1/moderation/users/{target_id}/ban"),
        Some(&token),
        Some(json!({ "reason": "no expiry" })),
    )
    .await;
    assert_error_code(response, "no-permission").await;
    assert_eq!(ban_count_for(&pool, target_id).await, 0);
}

/// A `moderator` requesting a TEMPORARY ban (`expiresAt` present, future) against a lower-role
/// target succeeds.
#[sqlx::test]
async fn moderator_can_issue_temporary_ban(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let moderator_id =
        common::register_test_user_with_role(&pool, "mod-temp-ban-mod", "moderator").await;
    let token = common::jwt_for(moderator_id);
    let target_id = common::register_test_user(&pool, "mod-temp-ban-target").await;
    let expires_at = (Utc::now() + Duration::days(1)).to_rfc3339();

    let app = savez::app(state.clone());
    let response = send(
        app,
        "POST",
        &format!("/v1/moderation/users/{target_id}/ban"),
        Some(&token),
        Some(json!({ "reason": "temp ban", "expiresAt": expires_at })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_to_json(response).await["success"], json!(true));
    assert_eq!(ban_count_for(&pool, target_id).await, 1);
}

/// An `admin` requesting a PERMANENT ban succeeds — the admin half of SPEC's "moderator (temp) /
/// admin (perm)" split.
#[sqlx::test]
async fn admin_can_issue_permanent_ban(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let admin_id = common::register_test_user_with_role(&pool, "admin-perm-ban", "admin").await;
    let token = common::jwt_for(admin_id);
    let target_id = common::register_test_user(&pool, "admin-perm-ban-target").await;

    let app = savez::app(state.clone());
    let response = send(
        app,
        "POST",
        &format!("/v1/moderation/users/{target_id}/ban"),
        Some(&token),
        Some(json!({ "reason": "admin can permanently ban" })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(ban_count_for(&pool, target_id).await, 1);
}

/// T-07-55: a `moderator` may never ban a target whose role is >= their own — proven here against
/// an `admin` target. Uses a TEMPORARY ban body so the rejection is isolated to the role-escalation
/// guard, not T-07-54's separate permanent-ban check.
#[sqlx::test]
async fn moderator_cannot_ban_an_admin(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let moderator_id =
        common::register_test_user_with_role(&pool, "mod-vs-admin-mod", "moderator").await;
    let token = common::jwt_for(moderator_id);
    let target_id =
        common::register_test_user_with_role(&pool, "mod-vs-admin-target", "admin").await;
    let expires_at = (Utc::now() + Duration::days(1)).to_rfc3339();

    let app = savez::app(state.clone());
    let response = send(
        app,
        "POST",
        &format!("/v1/moderation/users/{target_id}/ban"),
        Some(&token),
        Some(json!({ "reason": "escalation attempt", "expiresAt": expires_at })),
    )
    .await;
    assert_error_code(response, "no-permission").await;
    assert_eq!(ban_count_for(&pool, target_id).await, 0);
}

/// T-07-55, same guard, proven against an equal-role target (`moderator` vs `moderator`).
#[sqlx::test]
async fn moderator_cannot_ban_another_moderator(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let moderator_id =
        common::register_test_user_with_role(&pool, "mod-vs-mod-mod", "moderator").await;
    let token = common::jwt_for(moderator_id);
    let target_id =
        common::register_test_user_with_role(&pool, "mod-vs-mod-target", "moderator").await;
    let expires_at = (Utc::now() + Duration::days(1)).to_rfc3339();

    let app = savez::app(state.clone());
    let response = send(
        app,
        "POST",
        &format!("/v1/moderation/users/{target_id}/ban"),
        Some(&token),
        Some(json!({ "reason": "peer ban attempt", "expiresAt": expires_at })),
    )
    .await;
    assert_error_code(response, "no-permission").await;
    assert_eq!(ban_count_for(&pool, target_id).await, 0);
}

/// `status` absent defaults to `pending` (SPEC's own default) — a report resolved away from
/// `pending` no longer appears in the default-filtered queue.
#[sqlx::test]
async fn report_queue_defaults_to_pending(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let author_id = common::register_test_user(&pool, "queue-default-author").await;
    let author_token = common::jwt_for(author_id);
    let app = savez::app(state.clone());
    let submitted = submit_puzzle(app, &author_token, "SySySySy", "Queue Default").await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number");

    let reporter_id = common::register_test_user(&pool, "queue-default-reporter").await;
    let reporter_token = common::jwt_for(reporter_id);
    let app = savez::app(state.clone());
    let report_response = report_puzzle(app, &reporter_token, puzzle_id, "profane").await;
    assert_eq!(report_response.status(), StatusCode::OK);
    let report_id = latest_report_id(&pool, puzzle_id, "profane").await;

    let moderator_id =
        common::register_test_user_with_role(&pool, "queue-default-mod", "moderator").await;
    let token = common::jwt_for(moderator_id);

    let app = savez::app(state.clone());
    let response = send(app, "GET", "/v1/moderation/reports", Some(&token), None).await;
    let entries = body_to_json(response).await;
    let entries = entries.as_array().expect("reports is an array");
    assert!(
        entries
            .iter()
            .any(|e| e["id"] == json!(report_id) && e["status"] == json!("pending")),
        "default-filtered queue must contain the pending report"
    );
}

/// `status=all` returns every status, including one already resolved away from `pending` — the
/// default-filtered queue above must NOT contain it.
#[sqlx::test]
async fn report_queue_status_all_returns_everything(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let author_id = common::register_test_user(&pool, "queue-all-author").await;
    let author_token = common::jwt_for(author_id);
    let app = savez::app(state.clone());
    let submitted = submit_puzzle(app, &author_token, "WpWpWpWp", "Queue All Test").await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number");

    let reporter_id = common::register_test_user(&pool, "queue-all-reporter").await;
    let reporter_token = common::jwt_for(reporter_id);
    let app = savez::app(state.clone());
    let report_response = report_puzzle(app, &reporter_token, puzzle_id, "profane").await;
    assert_eq!(report_response.status(), StatusCode::OK);
    let report_id = latest_report_id(&pool, puzzle_id, "profane").await;

    let moderator_id =
        common::register_test_user_with_role(&pool, "queue-all-mod", "moderator").await;
    let token = common::jwt_for(moderator_id);

    let app = savez::app(state.clone());
    let response = send(
        app,
        "POST",
        &format!("/v1/moderation/reports/{report_id}/resolve"),
        Some(&token),
        Some(json!({ "status": "upheld" })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let app = savez::app(state.clone());
    let default_response = send(app, "GET", "/v1/moderation/reports", Some(&token), None).await;
    let default_entries = body_to_json(default_response).await;
    assert!(
        !default_entries
            .as_array()
            .expect("reports is an array")
            .iter()
            .any(|e| e["id"] == json!(report_id)),
        "resolved report must not appear in the default (pending-only) queue"
    );

    let app = savez::app(state);
    let all_response = send(
        app,
        "GET",
        "/v1/moderation/reports?status=all",
        Some(&token),
        None,
    )
    .await;
    let all_entries = body_to_json(all_response).await;
    assert!(
        all_entries
            .as_array()
            .expect("reports is an array")
            .iter()
            .any(|e| e["id"] == json!(report_id) && e["status"] == json!("upheld")),
        "status=all must surface the now-resolved report"
    );
}

/// A non-numeric `limit` stays inside the all-200 taxonomy (`bad-payload`), never axum's native,
/// non-taxonomy rejection.
#[sqlx::test]
async fn report_queue_rejects_non_numeric_limit(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let moderator_id =
        common::register_test_user_with_role(&pool, "queue-bad-limit-mod", "moderator").await;
    let token = common::jwt_for(moderator_id);

    let app = savez::app(state);
    let response = send(
        app,
        "GET",
        "/v1/moderation/reports?limit=abc",
        Some(&token),
        None,
    )
    .await;
    assert_error_code(response, "bad-payload").await;
}

/// D-08 surfaced over HTTP: resolving one report also resolves every PENDING sibling sharing the
/// SAME puzzle AND the SAME reason — the response's `resolved` array proves the group, not just a
/// single id.
#[sqlx::test]
async fn resolve_route_returns_sibling_ids(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let author_id = common::register_test_user(&pool, "resolve-siblings-author").await;
    let author_token = common::jwt_for(author_id);
    let app = savez::app(state.clone());
    let submitted = submit_puzzle(app, &author_token, "RcRcRcRc", "Resolve Siblings").await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number");

    let reporter_one = common::register_test_user(&pool, "resolve-siblings-r1").await;
    let reporter_one_token = common::jwt_for(reporter_one);
    let app = savez::app(state.clone());
    assert_eq!(
        report_puzzle(app, &reporter_one_token, puzzle_id, "profane")
            .await
            .status(),
        StatusCode::OK
    );
    let first_report_id = latest_report_id(&pool, puzzle_id, "profane").await;

    let reporter_two = common::register_test_user(&pool, "resolve-siblings-r2").await;
    let reporter_two_token = common::jwt_for(reporter_two);
    let app = savez::app(state.clone());
    assert_eq!(
        report_puzzle(app, &reporter_two_token, puzzle_id, "profane")
            .await
            .status(),
        StatusCode::OK
    );
    let second_report_id = latest_report_id(&pool, puzzle_id, "profane").await;
    assert_ne!(first_report_id, second_report_id);

    let moderator_id =
        common::register_test_user_with_role(&pool, "resolve-siblings-mod", "moderator").await;
    let token = common::jwt_for(moderator_id);

    let app = savez::app(state);
    let response = send(
        app,
        "POST",
        &format!("/v1/moderation/reports/{first_report_id}/resolve"),
        Some(&token),
        Some(json!({ "status": "upheld" })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_to_json(response).await;
    let resolved: Vec<i64> = body["resolved"]
        .as_array()
        .expect("resolved is an array")
        .iter()
        .map(|v| v.as_i64().expect("resolved id is a number"))
        .collect();
    assert!(resolved.contains(&(first_report_id as i64)));
    assert!(resolved.contains(&(second_report_id as i64)));
}

/// `purge`'s response surfaces the puzzle's `shortKey`, freed by the transactional delete
/// (`repository::purge_puzzle`) for immediate reuse.
#[sqlx::test]
async fn purge_route_returns_freed_short_key(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let author_id = common::register_test_user(&pool, "purge-author").await;
    let author_token = common::jwt_for(author_id);
    let app = savez::app(state.clone());
    let submitted = submit_puzzle(app, &author_token, "CgCgCgCg", "Purge Target").await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number");
    let short_key = submitted["shortKey"]
        .as_str()
        .expect("submitted shortKey is a string")
        .to_string();

    let admin_id = common::register_test_user_with_role(&pool, "purge-admin", "admin").await;
    let token = common::jwt_for(admin_id);

    let app = savez::app(state);
    let response = send(
        app,
        "DELETE",
        &format!("/v1/moderation/puzzles/{puzzle_id}"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_to_json(response).await,
        json!({ "success": true, "freedShortKey": short_key })
    );
}

/// T-07-60: `/v1/moderation/*` is excluded from `ratelimit::check_and_record` by construction —
/// more calls than the seeded `write` class hourly ceiling (5, `migrations/
/// 20260810000003_rate_limiting.sql`) all succeed for the same moderator, on the same route.
#[sqlx::test]
async fn moderation_routes_are_not_rate_limited(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let author_id = common::register_test_user(&pool, "no-ratelimit-author").await;
    let author_token = common::jwt_for(author_id);
    let app = savez::app(state.clone());
    let submitted = submit_puzzle(app, &author_token, "SbSbSbSb", "No RateLimit Test").await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number");

    let moderator_id =
        common::register_test_user_with_role(&pool, "no-ratelimit-mod", "moderator").await;
    let token = common::jwt_for(moderator_id);

    // Seeded `write` class default is 5/hour + 20/day (07-07); 8 calls exceeds the tighter of the
    // two, and every single one must still succeed since this route never calls
    // `ratelimit::check_and_record` in the first place.
    for _ in 0..8 {
        let app = savez::app(state.clone());
        let response = send(
            app,
            "POST",
            &format!("/v1/moderation/puzzles/{puzzle_id}/hide"),
            Some(&token),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_to_json(response).await, json!({ "success": true }));
    }
}
