//! 07-05-PLAN.md: proves D-05's threshold/idempotence, ADR 0002's `hidden_by` tri-state, and
//! ROADMAP SC2's moderator direct-access visibility, via real HTTP round-trips through
//! `savez::app` wherever the production flow can reach the scenario, and direct `sqlx::query`
//! manipulation only where D-10's own (correct) visibility rule would otherwise make a scenario
//! unreachable through the API (see `run_auto_hide_update`/`insert_pending_report_row`'s doc
//! comments for exactly which tests need this and why).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use savez::error::AppError;
use savez::repository;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt; // for `oneshot`

fn sample_game_data() -> Value {
    json!({
        "version": 1,
        "bounds": { "w": 10, "h": 8 },
        "buildings": [
            { "type": "emitter", "item": "CuCuCuCu", "pos": { "x": 0, "y": 0, "r": 0 } },
            { "type": "goal", "item": "CuCuCuCu", "pos": { "x": 4, "y": 3, "r": 90 } },
            { "type": "block", "pos": { "x": 2, "y": 2, "r": 180 } },
        ],
        "excludedBuildings": ["CutterMirrored"],
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

/// D-17: every business/auth rejection answers HTTP 200 with `{ "error": "<code>" }`.
async fn assert_error_code(response: axum::response::Response, expected_code: &str) {
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_to_json(response).await;
    assert_eq!(body, json!({ "error": expected_code }));
}

async fn submit_puzzle(app: axum::Router, token: &str, short_key: &str, title: &str) -> Value {
    let body = json!({
        "title": title,
        "shortKey": short_key,
        "data": sample_game_data().to_string(),
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/puzzles/submit")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-token", token)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_to_json(response).await
}

async fn report_request(
    app: axum::Router,
    token: &str,
    id: i32,
    reason: &str,
) -> axum::response::Response {
    let body = json!({ "reason": reason });
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

/// Registers a fresh user and reports `puzzle_id` on their behalf in one call -- every distinct
/// pending-report test needs a NEW reporter (`UNIQUE(user_id, puzzle_id)`), so this is the shape
/// every threshold test below reaches for.
async fn report_by_new_user(
    pool: &PgPool,
    app: axum::Router,
    name: &str,
    puzzle_id: i32,
    reason: &str,
) -> axum::response::Response {
    let reporter_id = common::register_test_user(pool, name).await;
    let token = common::jwt_for(reporter_id);
    report_request(app, &token, puzzle_id, reason).await
}

async fn download_request(
    app: axum::Router,
    token: &str,
    id_or_key: &str,
) -> axum::response::Response {
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

async fn delete_request(app: axum::Router, token: &str, id: i32) -> axum::response::Response {
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

async fn list_new_request(app: axum::Router, token: &str) -> axum::response::Response {
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

async fn search_request(
    app: axum::Router,
    token: &str,
    search_term: &str,
) -> axum::response::Response {
    let body = json!({ "searchTerm": search_term });
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

/// Reads `hidden_at`/`hidden_by` directly -- the ground truth ADR 0002's tri-state and D-05's
/// idempotence are checked against throughout this file.
async fn hidden_state(
    pool: &PgPool,
    puzzle_id: i32,
) -> (Option<chrono::DateTime<chrono::Utc>>, Option<uuid::Uuid>) {
    sqlx::query_as::<_, (Option<chrono::DateTime<chrono::Utc>>, Option<uuid::Uuid>)>(
        "SELECT hidden_at, hidden_by FROM puzzles WHERE id = $1",
    )
    .bind(puzzle_id)
    .fetch_one(pool)
    .await
    .expect("puzzle row must exist")
}

async fn moderation_log_count(pool: &PgPool, target_id: i32, action: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM moderation_log WHERE target_id = $1 AND action = $2",
    )
    .bind(target_id.to_string())
    .bind(action)
    .fetch_one(pool)
    .await
    .expect("count moderation_log rows")
}

/// Directly inserts one pending `puzzle_reports` row, bypassing `repository::insert_report`'s own
/// visibility `SELECT` entirely. Needed by the 4th-report idempotence test and the self-hide
/// preservation test below: both scenarios require pending reports to exist against a puzzle that
/// is ALREADY hidden, which `insert_report`'s D-10 visibility guard correctly refuses for any
/// non-author caller through the real HTTP/repository path.
async fn insert_pending_report_row(pool: &PgPool, reporter_id: uuid::Uuid, puzzle_id: i32, reason: &str) {
    sqlx::query("INSERT INTO puzzle_reports (user_id, puzzle_id, reason) VALUES ($1, $2, $3)")
        .bind(reporter_id)
        .bind(puzzle_id)
        .bind(reason)
        .execute(pool)
        .await
        .expect("insert direct pending report row");
}

/// Re-runs the exact D-05 conditional `UPDATE` (byte-for-byte the statement
/// `repository::insert_report` executes after its own `INSERT`) directly, for the same reason
/// `insert_pending_report_row` exists: proving the guard's own idempotence/preservation behavior
/// in a state that can no longer be reached by calling `insert_report` itself, since the puzzle is
/// already hidden by the time these tests seed further pending reports.
async fn run_auto_hide_update(pool: &PgPool, puzzle_id: i32) {
    sqlx::query(
        r#"
        UPDATE puzzles
        SET hidden_at = now(), hidden_by = NULL
        WHERE id = $1
          AND hidden_at IS NULL
          AND (SELECT COUNT(*) FROM puzzle_reports WHERE puzzle_id = $1 AND status = 'pending') >= $2
        "#,
    )
    .bind(puzzle_id)
    .bind(repository::AUTO_HIDE_REPORT_THRESHOLD)
    .execute(pool)
    .await
    .expect("run auto-hide update");
}

/// Submits a puzzle for a fresh author, then drives 3 distinct fresh reporters through the real
/// `POST /v1/puzzles/report/:id` route to auto-hide it via D-05's threshold. Returns
/// `(puzzle_id, author_id, author_token)`. Shared by every visibility test (9-12 of
/// 07-05-PLAN.md's Task 3 list) so each only has to assert its own access outcome.
async fn build_auto_hidden_puzzle(pool: &PgPool, app: axum::Router) -> (i32, uuid::Uuid, String) {
    let author_id = common::register_test_user(pool, "hidden-puzzle-author").await;
    let author_token = common::jwt_for(author_id);
    let meta = submit_puzzle(app.clone(), &author_token, "CuCuCuCu", "Reported Puzzle").await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id is a number") as i32;

    for (i, reason) in ["trolling", "profane", "unsolvable"].into_iter().enumerate() {
        let response = report_by_new_user(
            pool,
            app.clone(),
            &format!("hidden-puzzle-reporter-{i}"),
            puzzle_id,
            reason,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    let (hidden_at, hidden_by) = hidden_state(pool, puzzle_id).await;
    assert!(
        hidden_at.is_some(),
        "fixture setup must actually auto-hide the puzzle"
    );
    assert_eq!(hidden_by, None, "fixture setup must be the automatic (NULL) hide");

    (puzzle_id, author_id, author_token)
}

// --- D-05 threshold and idempotence ---------------------------------------------------------

#[sqlx::test]
async fn auto_hide_does_not_fire_below_threshold(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "below-threshold-author").await;
    let author_token = common::jwt_for(author_id);
    let meta = submit_puzzle(app.clone(), &author_token, "CuCuCuCu", "Below Threshold").await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id") as i32;

    let r1 = report_by_new_user(&pool, app.clone(), "below-threshold-reporter-1", puzzle_id, "trolling").await;
    assert_eq!(r1.status(), StatusCode::OK);
    let r2 = report_by_new_user(&pool, app.clone(), "below-threshold-reporter-2", puzzle_id, "profane").await;
    assert_eq!(r2.status(), StatusCode::OK);

    let (hidden_at, hidden_by) = hidden_state(&pool, puzzle_id).await;
    assert!(
        hidden_at.is_none(),
        "2 distinct pending reports must not cross the threshold of {}",
        repository::AUTO_HIDE_REPORT_THRESHOLD
    );
    assert!(hidden_by.is_none());
}

#[sqlx::test]
async fn auto_hide_fires_at_third_distinct_pending_report(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "third-report-author").await;
    let author_token = common::jwt_for(author_id);
    let meta = submit_puzzle(app.clone(), &author_token, "CuCuCuCu", "Third Report Puzzle").await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id") as i32;

    for (i, reason) in ["trolling", "profane", "unsolvable"].into_iter().enumerate() {
        let response =
            report_by_new_user(&pool, app.clone(), &format!("third-report-reporter-{i}"), puzzle_id, reason)
                .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    let (hidden_at, hidden_by) = hidden_state(&pool, puzzle_id).await;
    assert!(
        hidden_at.is_some(),
        "the 3rd distinct pending report must cross the threshold and hide the puzzle"
    );
    assert_eq!(
        hidden_by, None,
        "D-05/ADR 0002: the automatic threshold hide must leave hidden_by NULL, not attribute it \
         to any of the reporters or a fabricated moderator"
    );
}

#[sqlx::test]
async fn auto_hide_is_idempotent_on_fourth_report(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "fourth-report-author").await;
    let author_token = common::jwt_for(author_id);
    let meta = submit_puzzle(app.clone(), &author_token, "CuCuCuCu", "Fourth Report Puzzle").await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id") as i32;

    for (i, reason) in ["trolling", "profane", "unsolvable"].into_iter().enumerate() {
        let response =
            report_by_new_user(&pool, app.clone(), &format!("fourth-report-reporter-{i}"), puzzle_id, reason)
                .await;
        assert_eq!(response.status(), StatusCode::OK);
    }
    let (hidden_at_after_third, hidden_by_after_third) = hidden_state(&pool, puzzle_id).await;
    assert!(hidden_at_after_third.is_some());
    assert_eq!(hidden_by_after_third, None);

    // A 4th distinct reporter can no longer reach `insert_report`'s own visibility check for an
    // already-hidden puzzle (D-10 correctly refuses it) -- seed the 4th pending report row
    // directly and re-run the exact auto-hide UPDATE to prove it stays a safe no-op.
    let fourth_reporter = common::register_test_user(&pool, "fourth-report-reporter-extra").await;
    insert_pending_report_row(&pool, fourth_reporter, puzzle_id, "trolling").await;
    run_auto_hide_update(&pool, puzzle_id).await;

    let (hidden_at_after_fourth, hidden_by_after_fourth) = hidden_state(&pool, puzzle_id).await;
    assert_eq!(
        hidden_at_after_fourth, hidden_at_after_third,
        "a 4th pending report must not move hidden_at again"
    );
    assert_eq!(hidden_by_after_fourth, None);
}

#[sqlx::test]
async fn auto_hide_preserves_author_self_hide(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "self-hide-author").await;
    let author_token = common::jwt_for(author_id);
    let meta = submit_puzzle(app.clone(), &author_token, "CuCuCuCu", "Self Hidden Puzzle").await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id") as i32;

    // Phase 6 self-delete: hidden_by = the author's own id, never NULL.
    let delete_response = delete_request(app.clone(), &author_token, puzzle_id).await;
    assert_eq!(delete_response.status(), StatusCode::OK);

    let (hidden_at_before, hidden_by_before) = hidden_state(&pool, puzzle_id).await;
    assert!(hidden_at_before.is_some());
    assert_eq!(hidden_by_before, Some(author_id));

    // 3 distinct pending reports seeded directly -- a real reporter can no longer reach
    // `insert_report` once the puzzle is hidden (D-10), so this proves the auto-hide UPDATE's own
    // guard in isolation: it must never overwrite an author's pre-existing self-hide.
    for i in 0..3 {
        let reporter = common::register_test_user(&pool, &format!("self-hide-reporter-{i}")).await;
        insert_pending_report_row(&pool, reporter, puzzle_id, "trolling").await;
    }
    run_auto_hide_update(&pool, puzzle_id).await;

    let (hidden_at_after, hidden_by_after) = hidden_state(&pool, puzzle_id).await;
    assert_eq!(
        hidden_at_after, hidden_at_before,
        "the guard must not touch an already-set hidden_at"
    );
    assert_eq!(
        hidden_by_after,
        Some(author_id),
        "hidden_by must remain the author's own id, never overwritten to NULL"
    );
}

#[sqlx::test]
async fn resolved_reports_do_not_count_toward_threshold(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "resolved-reports-author").await;
    let author_token = common::jwt_for(author_id);
    let meta = submit_puzzle(app.clone(), &author_token, "CuCuCuCu", "Resolved Reports").await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id") as i32;

    let r1 = report_by_new_user(&pool, app.clone(), "resolved-reports-reporter-1", puzzle_id, "trolling").await;
    assert_eq!(r1.status(), StatusCode::OK);
    let r2 = report_by_new_user(&pool, app.clone(), "resolved-reports-reporter-2", puzzle_id, "profane").await;
    assert_eq!(r2.status(), StatusCode::OK);

    // Directly resolve one pending report to `upheld` BEFORE the 3rd arrives -- only `pending`
    // reports count toward D-05's threshold, so at most 2 are ever pending at once here.
    sqlx::query(
        "UPDATE puzzle_reports SET status = 'upheld' \
         WHERE id = (SELECT id FROM puzzle_reports WHERE puzzle_id = $1 AND status = 'pending' ORDER BY id LIMIT 1)",
    )
    .bind(puzzle_id)
    .execute(&pool)
    .await
    .expect("resolve one report to upheld");

    let r3 = report_by_new_user(&pool, app.clone(), "resolved-reports-reporter-3", puzzle_id, "unsolvable").await;
    assert_eq!(r3.status(), StatusCode::OK);

    let (hidden_at, hidden_by) = hidden_state(&pool, puzzle_id).await;
    assert!(
        hidden_at.is_none(),
        "3 reports were created but never more than 2 pending at once -- must never auto-hide"
    );
    assert!(hidden_by.is_none());
}

// --- Manual hide/unhide and the audit journal -----------------------------------------------

#[sqlx::test]
async fn moderator_hide_sets_hidden_by_to_moderator(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "manual-hide-author").await;
    let author_token = common::jwt_for(author_id);
    let meta = submit_puzzle(app.clone(), &author_token, "CuCuCuCu", "Manual Hide Puzzle").await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id") as i32;

    let moderator_id = common::register_test_user_with_role(&pool, "manual-hide-moderator", "moderator").await;

    repository::hide_puzzle(&pool, puzzle_id, moderator_id, Some("policy violation"))
        .await
        .expect("hide_puzzle must succeed");

    let (hidden_at, hidden_by) = hidden_state(&pool, puzzle_id).await;
    assert!(hidden_at.is_some());
    assert_eq!(
        hidden_by,
        Some(moderator_id),
        "manual hide must attribute hidden_by to the acting moderator, never NULL"
    );

    let logged = moderation_log_count(&pool, puzzle_id, repository::moderation_action::HIDE_PUZZLE).await;
    assert_eq!(
        logged, 1,
        "hide_puzzle must append exactly one moderation_log row with action = hide_puzzle"
    );
}

#[sqlx::test]
async fn moderator_unhide_clears_hidden_state_and_logs(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "manual-unhide-author").await;
    let author_token = common::jwt_for(author_id);
    let meta = submit_puzzle(app.clone(), &author_token, "CuCuCuCu", "Manual Unhide Puzzle").await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id") as i32;

    let moderator_id = common::register_test_user_with_role(&pool, "manual-unhide-moderator", "moderator").await;

    repository::hide_puzzle(&pool, puzzle_id, moderator_id, None)
        .await
        .expect("hide_puzzle must succeed");
    repository::unhide_puzzle(&pool, puzzle_id, moderator_id, Some("appeal accepted"))
        .await
        .expect("unhide_puzzle must succeed");

    let (hidden_at, hidden_by) = hidden_state(&pool, puzzle_id).await;
    assert!(hidden_at.is_none(), "unhide must clear hidden_at");
    assert!(hidden_by.is_none(), "unhide must clear hidden_by");

    let hide_logged = moderation_log_count(&pool, puzzle_id, repository::moderation_action::HIDE_PUZZLE).await;
    assert_eq!(hide_logged, 1, "the earlier hide_puzzle call must still have its own log row");
    let unhide_logged = moderation_log_count(&pool, puzzle_id, repository::moderation_action::UNHIDE_PUZZLE).await;
    assert_eq!(
        unhide_logged, 1,
        "unhide_puzzle must append its own second moderation_log row with action = unhide_puzzle"
    );
}

#[sqlx::test]
async fn hide_unknown_puzzle_is_not_found(pool: PgPool) {
    let moderator_id = common::register_test_user_with_role(&pool, "hide-unknown-moderator", "moderator").await;

    let result = repository::hide_puzzle(&pool, 999_999, moderator_id, None).await;
    assert!(matches!(result, Err(AppError::NotFound)));

    let logged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM moderation_log")
        .fetch_one(&pool)
        .await
        .expect("count moderation_log rows");
    assert_eq!(logged, 0, "a not-found hide must never write an audit row");
}

// --- Moderator direct-access visibility (ROADMAP SC2) ----------------------------------------

#[sqlx::test]
async fn hidden_puzzle_is_downloadable_by_moderator(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let (puzzle_id, _author_id, _author_token) = build_auto_hidden_puzzle(&pool, app.clone()).await;

    let moderator_id = common::register_test_user_with_role(&pool, "download-moderator", "moderator").await;
    let moderator_token = common::jwt_for(moderator_id);

    let response = download_request(app.clone(), &moderator_token, &puzzle_id.to_string()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_to_json(response).await;
    assert_eq!(body["meta"]["id"], puzzle_id);
    assert!(body.get("game").is_some());
}

#[sqlx::test]
async fn hidden_puzzle_is_not_downloadable_by_plain_user(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let (puzzle_id, _author_id, _author_token) = build_auto_hidden_puzzle(&pool, app.clone()).await;

    let plain_user_id = common::register_test_user(&pool, "download-plain-user").await;
    let plain_user_token = common::jwt_for(plain_user_id);

    let response = download_request(app.clone(), &plain_user_token, &puzzle_id.to_string()).await;
    assert_error_code(response, "not-found").await;
}

#[sqlx::test]
async fn hidden_puzzle_is_downloadable_by_author(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let (puzzle_id, _author_id, author_token) = build_auto_hidden_puzzle(&pool, app.clone()).await;

    let response = download_request(app.clone(), &author_token, &puzzle_id.to_string()).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[sqlx::test]
async fn hidden_puzzle_stays_out_of_catalog_for_moderator(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let (puzzle_id, _author_id, _author_token) = build_auto_hidden_puzzle(&pool, app.clone()).await;

    let moderator_id = common::register_test_user_with_role(&pool, "catalog-moderator", "moderator").await;
    let moderator_token = common::jwt_for(moderator_id);

    let list_response = list_new_request(app.clone(), &moderator_token).await;
    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = body_to_json(list_response).await;
    let list_ids: Vec<i64> = list_body
        .as_array()
        .expect("list/new returns an array")
        .iter()
        .map(|p| p["id"].as_i64().expect("puzzle id is a number"))
        .collect();
    assert!(
        !list_ids.contains(&i64::from(puzzle_id)),
        "list/new must never resurface a hidden puzzle, even to a moderator"
    );

    let search_response = search_request(app.clone(), &moderator_token, "Reported Puzzle").await;
    assert_eq!(search_response.status(), StatusCode::OK);
    let search_body = body_to_json(search_response).await;
    let search_ids: Vec<i64> = search_body
        .as_array()
        .expect("search returns an array")
        .iter()
        .map(|p| p["id"].as_i64().expect("puzzle id is a number"))
        .collect();
    assert!(
        !search_ids.contains(&i64::from(puzzle_id)),
        "search must never resurface a hidden puzzle, even to a moderator"
    );
}
