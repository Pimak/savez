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
    moderation_log_count_str(pool, &target_id.to_string(), action).await
}

/// Same as `moderation_log_count`, for a non-`i32` `target_id` (a `Uuid`'s decimal-free string
/// form, e.g. `user`/`ban` targets in 07-06's sanction functions) -- `moderation_log.target_id` is
/// always `TEXT` regardless of what it identifies.
async fn moderation_log_count_str(pool: &PgPool, target_id: &str, action: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM moderation_log WHERE target_id = $1 AND action = $2",
    )
    .bind(target_id)
    .bind(action)
    .fetch_one(pool)
    .await
    .expect("count moderation_log rows")
}

/// Fetches every `(id, status)` row of `puzzle_reports` for a given `puzzle_id`/`reason` pair,
/// ordered by `id` -- the ground truth 07-06's D-08 same-reason-siblings tests check against.
async fn report_rows_for(pool: &PgPool, puzzle_id: i32, reason: &str) -> Vec<(i32, String)> {
    sqlx::query_as::<_, (i32, String)>(
        "SELECT id, status FROM puzzle_reports WHERE puzzle_id = $1 AND reason = $2 ORDER BY id",
    )
    .bind(puzzle_id)
    .bind(reason)
    .fetch_all(pool)
    .await
    .expect("fetch report rows")
}

/// Directly inserts one pending `puzzle_reports` row, bypassing `repository::insert_report`'s own
/// visibility `SELECT` entirely. Needed by the 4th-report idempotence test and the self-hide
/// preservation test below: both scenarios require pending reports to exist against a puzzle that
/// is ALREADY hidden, which `insert_report`'s D-10 visibility guard correctly refuses for any
/// non-author caller through the real HTTP/repository path.
async fn insert_pending_report_row(
    pool: &PgPool,
    reporter_id: uuid::Uuid,
    puzzle_id: i32,
    reason: &str,
) {
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

    for (i, reason) in ["trolling", "profane", "unsolvable"]
        .into_iter()
        .enumerate()
    {
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
    assert_eq!(
        hidden_by, None,
        "fixture setup must be the automatic (NULL) hide"
    );

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

    let r1 = report_by_new_user(
        &pool,
        app.clone(),
        "below-threshold-reporter-1",
        puzzle_id,
        "trolling",
    )
    .await;
    assert_eq!(r1.status(), StatusCode::OK);
    let r2 = report_by_new_user(
        &pool,
        app.clone(),
        "below-threshold-reporter-2",
        puzzle_id,
        "profane",
    )
    .await;
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
    let meta = submit_puzzle(
        app.clone(),
        &author_token,
        "CuCuCuCu",
        "Third Report Puzzle",
    )
    .await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id") as i32;

    for (i, reason) in ["trolling", "profane", "unsolvable"]
        .into_iter()
        .enumerate()
    {
        let response = report_by_new_user(
            &pool,
            app.clone(),
            &format!("third-report-reporter-{i}"),
            puzzle_id,
            reason,
        )
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
    let meta = submit_puzzle(
        app.clone(),
        &author_token,
        "CuCuCuCu",
        "Fourth Report Puzzle",
    )
    .await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id") as i32;

    for (i, reason) in ["trolling", "profane", "unsolvable"]
        .into_iter()
        .enumerate()
    {
        let response = report_by_new_user(
            &pool,
            app.clone(),
            &format!("fourth-report-reporter-{i}"),
            puzzle_id,
            reason,
        )
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

    let r1 = report_by_new_user(
        &pool,
        app.clone(),
        "resolved-reports-reporter-1",
        puzzle_id,
        "trolling",
    )
    .await;
    assert_eq!(r1.status(), StatusCode::OK);
    let r2 = report_by_new_user(
        &pool,
        app.clone(),
        "resolved-reports-reporter-2",
        puzzle_id,
        "profane",
    )
    .await;
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

    let r3 = report_by_new_user(
        &pool,
        app.clone(),
        "resolved-reports-reporter-3",
        puzzle_id,
        "unsolvable",
    )
    .await;
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

    let moderator_id =
        common::register_test_user_with_role(&pool, "manual-hide-moderator", "moderator").await;

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

    let logged =
        moderation_log_count(&pool, puzzle_id, repository::moderation_action::HIDE_PUZZLE).await;
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
    let meta = submit_puzzle(
        app.clone(),
        &author_token,
        "CuCuCuCu",
        "Manual Unhide Puzzle",
    )
    .await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id") as i32;

    let moderator_id =
        common::register_test_user_with_role(&pool, "manual-unhide-moderator", "moderator").await;

    repository::hide_puzzle(&pool, puzzle_id, moderator_id, None)
        .await
        .expect("hide_puzzle must succeed");
    repository::unhide_puzzle(&pool, puzzle_id, moderator_id, Some("appeal accepted"))
        .await
        .expect("unhide_puzzle must succeed");

    let (hidden_at, hidden_by) = hidden_state(&pool, puzzle_id).await;
    assert!(hidden_at.is_none(), "unhide must clear hidden_at");
    assert!(hidden_by.is_none(), "unhide must clear hidden_by");

    let hide_logged =
        moderation_log_count(&pool, puzzle_id, repository::moderation_action::HIDE_PUZZLE).await;
    assert_eq!(
        hide_logged, 1,
        "the earlier hide_puzzle call must still have its own log row"
    );
    let unhide_logged = moderation_log_count(
        &pool,
        puzzle_id,
        repository::moderation_action::UNHIDE_PUZZLE,
    )
    .await;
    assert_eq!(
        unhide_logged, 1,
        "unhide_puzzle must append its own second moderation_log row with action = unhide_puzzle"
    );
}

#[sqlx::test]
async fn hide_unknown_puzzle_is_not_found(pool: PgPool) {
    let moderator_id =
        common::register_test_user_with_role(&pool, "hide-unknown-moderator", "moderator").await;

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

    let moderator_id =
        common::register_test_user_with_role(&pool, "download-moderator", "moderator").await;
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

    let moderator_id =
        common::register_test_user_with_role(&pool, "catalog-moderator", "moderator").await;
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

// --- 07-06: resolve_report (D-06/D-07/D-08) ---------------------------------------------------

#[sqlx::test]
async fn resolve_report_resolves_same_reason_siblings_only(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "resolve-siblings-author").await;
    let author_token = common::jwt_for(author_id);
    let meta = submit_puzzle(app.clone(), &author_token, "CuCuCuCu", "Resolve Siblings").await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id") as i32;

    let r1 = report_by_new_user(
        &pool,
        app.clone(),
        "resolve-siblings-reporter-1",
        puzzle_id,
        "profane",
    )
    .await;
    assert_eq!(r1.status(), StatusCode::OK);
    let r2 = report_by_new_user(
        &pool,
        app.clone(),
        "resolve-siblings-reporter-2",
        puzzle_id,
        "profane",
    )
    .await;
    assert_eq!(r2.status(), StatusCode::OK);
    let r3 = report_by_new_user(
        &pool,
        app.clone(),
        "resolve-siblings-reporter-3",
        puzzle_id,
        "trolling",
    )
    .await;
    assert_eq!(r3.status(), StatusCode::OK);

    let profane_rows = report_rows_for(&pool, puzzle_id, "profane").await;
    assert_eq!(profane_rows.len(), 2);
    let profane_ids: Vec<i32> = profane_rows.iter().map(|(id, _)| *id).collect();

    let reviewer_id =
        common::register_test_user_with_role(&pool, "resolve-siblings-moderator", "moderator")
            .await;

    let mut resolved =
        repository::resolve_report(&pool, profane_ids[0], "upheld", reviewer_id, Some("policy"))
            .await
            .expect("resolve_report must succeed");
    resolved.sort();
    let mut expected = profane_ids.clone();
    expected.sort();
    assert_eq!(
        resolved, expected,
        "resolving one profane report must resolve exactly its profane sibling too"
    );

    let profane_after = report_rows_for(&pool, puzzle_id, "profane").await;
    assert!(
        profane_after.iter().all(|(_, status)| status == "upheld"),
        "every profane report must now be upheld"
    );
    let trolling_after = report_rows_for(&pool, puzzle_id, "trolling").await;
    assert!(
        trolling_after.iter().all(|(_, status)| status == "pending"),
        "the trolling report of the same puzzle must be left untouched"
    );
}

#[sqlx::test]
async fn resolve_report_does_not_hide_or_unhide(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let reviewer_id =
        common::register_test_user_with_role(&pool, "resolve-no-hide-moderator", "moderator").await;

    // A single resolved-upheld report on an otherwise-unhidden puzzle must not hide it.
    let author_id = common::register_test_user(&pool, "resolve-no-hide-author").await;
    let author_token = common::jwt_for(author_id);
    // Distinct shortKey from `build_auto_hidden_puzzle`'s own hardcoded "CuCuCuCu" below --
    // `puzzles.short_key` is UNIQUE across the whole database, not per author.
    let meta = submit_puzzle(app.clone(), &author_token, "RrRrRrRr", "Resolve No Hide").await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id") as i32;
    let r1 = report_by_new_user(
        &pool,
        app.clone(),
        "resolve-no-hide-reporter",
        puzzle_id,
        "profane",
    )
    .await;
    assert_eq!(r1.status(), StatusCode::OK);
    let report_rows = report_rows_for(&pool, puzzle_id, "profane").await;
    repository::resolve_report(&pool, report_rows[0].0, "upheld", reviewer_id, None)
        .await
        .expect("resolve_report must succeed");
    let (hidden_at, _) = hidden_state(&pool, puzzle_id).await;
    assert!(
        hidden_at.is_none(),
        "resolving a report upheld must never hide the puzzle"
    );

    // Resolving EVERY report of an already auto-hidden puzzle must not unhide it.
    let (hidden_puzzle_id, _hidden_author_id, _hidden_author_token) =
        build_auto_hidden_puzzle(&pool, app.clone()).await;
    for reason in ["trolling", "profane", "unsolvable"] {
        let rows = report_rows_for(&pool, hidden_puzzle_id, reason).await;
        repository::resolve_report(&pool, rows[0].0, "upheld", reviewer_id, None)
            .await
            .expect("resolve_report must succeed");
    }
    let (hidden_at_after, hidden_by_after) = hidden_state(&pool, hidden_puzzle_id).await;
    assert!(
        hidden_at_after.is_some(),
        "resolving every report of an auto-hidden puzzle must not unhide it"
    );
    assert_eq!(hidden_by_after, None);
}

#[sqlx::test]
async fn resolve_report_does_not_ban_author(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "resolve-no-ban-author").await;
    let author_token = common::jwt_for(author_id);
    let meta = submit_puzzle(app.clone(), &author_token, "CuCuCuCu", "Resolve No Ban").await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id") as i32;
    let r1 = report_by_new_user(
        &pool,
        app.clone(),
        "resolve-no-ban-reporter",
        puzzle_id,
        "profane",
    )
    .await;
    assert_eq!(r1.status(), StatusCode::OK);
    let report_rows = report_rows_for(&pool, puzzle_id, "profane").await;
    let reviewer_id =
        common::register_test_user_with_role(&pool, "resolve-no-ban-moderator", "moderator").await;

    repository::resolve_report(&pool, report_rows[0].0, "upheld", reviewer_id, None)
        .await
        .expect("resolve_report must succeed");

    let ban_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_bans WHERE user_id = $1")
        .bind(author_id)
        .fetch_one(&pool)
        .await
        .expect("count user_bans");
    assert_eq!(
        ban_count, 0,
        "resolving a report upheld must never insert a ban row"
    );
}

#[sqlx::test]
async fn resolve_unknown_or_already_resolved_report_is_not_found(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);
    let reviewer_id =
        common::register_test_user_with_role(&pool, "resolve-nf-moderator", "moderator").await;

    let unknown = repository::resolve_report(&pool, 999_999, "upheld", reviewer_id, None).await;
    assert!(matches!(unknown, Err(AppError::NotFound)));

    let author_id = common::register_test_user(&pool, "resolve-nf-author").await;
    let author_token = common::jwt_for(author_id);
    let meta = submit_puzzle(app.clone(), &author_token, "CuCuCuCu", "Resolve Not Found").await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id") as i32;
    let r1 = report_by_new_user(
        &pool,
        app.clone(),
        "resolve-nf-reporter",
        puzzle_id,
        "profane",
    )
    .await;
    assert_eq!(r1.status(), StatusCode::OK);
    let report_rows = report_rows_for(&pool, puzzle_id, "profane").await;
    let report_id = report_rows[0].0;

    repository::resolve_report(&pool, report_id, "upheld", reviewer_id, None)
        .await
        .expect("first resolution must succeed");

    let already = repository::resolve_report(&pool, report_id, "upheld", reviewer_id, None).await;
    assert!(matches!(already, Err(AppError::NotFound)));
}

#[sqlx::test]
async fn resolve_report_rejects_unknown_status(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "resolve-bad-status-author").await;
    let author_token = common::jwt_for(author_id);
    let meta = submit_puzzle(app.clone(), &author_token, "CuCuCuCu", "Resolve Bad Status").await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id") as i32;
    let r1 = report_by_new_user(
        &pool,
        app.clone(),
        "resolve-bad-status-reporter",
        puzzle_id,
        "profane",
    )
    .await;
    assert_eq!(r1.status(), StatusCode::OK);
    let report_rows = report_rows_for(&pool, puzzle_id, "profane").await;
    let reviewer_id =
        common::register_test_user_with_role(&pool, "resolve-bad-status-moderator", "moderator")
            .await;

    let result =
        repository::resolve_report(&pool, report_rows[0].0, "spam", reviewer_id, None).await;
    assert!(matches!(result, Err(AppError::BadPayload)));

    let rows_after = report_rows_for(&pool, puzzle_id, "profane").await;
    assert_eq!(
        rows_after[0].1, "pending",
        "a rejected status value must not modify any row"
    );
}

#[sqlx::test]
async fn report_queue_shows_author_upheld_counter(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "queue-counter-author").await;
    let author_token = common::jwt_for(author_id);
    let reviewer_id =
        common::register_test_user_with_role(&pool, "queue-counter-moderator", "moderator").await;

    let meta1 = submit_puzzle(app.clone(), &author_token, "CuCuCuCu", "Queue Counter One").await;
    let puzzle_id1 = meta1["id"].as_i64().expect("submitted id") as i32;
    let r1 = report_by_new_user(
        &pool,
        app.clone(),
        "queue-counter-reporter-1",
        puzzle_id1,
        "profane",
    )
    .await;
    assert_eq!(r1.status(), StatusCode::OK);
    let rows1 = report_rows_for(&pool, puzzle_id1, "profane").await;
    repository::resolve_report(&pool, rows1[0].0, "upheld", reviewer_id, None)
        .await
        .expect("resolve puzzle 1 report");

    let meta2 = submit_puzzle(app.clone(), &author_token, "RrRrRrRr", "Queue Counter Two").await;
    let puzzle_id2 = meta2["id"].as_i64().expect("submitted id") as i32;
    let r2 = report_by_new_user(
        &pool,
        app.clone(),
        "queue-counter-reporter-2",
        puzzle_id2,
        "trolling",
    )
    .await;
    assert_eq!(r2.status(), StatusCode::OK);
    let rows2 = report_rows_for(&pool, puzzle_id2, "trolling").await;
    repository::resolve_report(&pool, rows2[0].0, "upheld", reviewer_id, None)
        .await
        .expect("resolve puzzle 2 report");

    // A third, still-pending report on the first puzzle -- proves the queue lists it AND carries
    // the right author-wide counter even for an entry that is itself not yet resolved.
    let r3 = report_by_new_user(
        &pool,
        app.clone(),
        "queue-counter-reporter-3",
        puzzle_id1,
        "unsolvable",
    )
    .await;
    assert_eq!(r3.status(), StatusCode::OK);

    let queue = repository::list_reports(&pool, None, 100, 0)
        .await
        .expect("list_reports must succeed");
    let author_entries: Vec<_> = queue
        .iter()
        .filter(|entry| entry.author_name == "queue-counter-author")
        .collect();
    assert!(
        !author_entries.is_empty(),
        "the queue must contain at least one entry for this author"
    );
    for entry in author_entries {
        assert_eq!(
            entry.author_upheld_reports, 2,
            "every queue entry for this author must show 2 upheld reports across all their puzzles"
        );
    }
}

// --- 07-06: ban_user / lift_ban (D-11) ---------------------------------------------------------

#[sqlx::test]
async fn ban_user_does_not_lift_previous_ban(pool: PgPool) {
    let user_id = common::register_test_user(&pool, "ban-coexist-user").await;
    let moderator_id =
        common::register_test_user_with_role(&pool, "ban-coexist-moderator", "moderator").await;

    let ban1 = repository::ban_user(&pool, user_id, "first offense", moderator_id, None)
        .await
        .expect("first ban must succeed");
    let ban2 = repository::ban_user(&pool, user_id, "second offense", moderator_id, None)
        .await
        .expect("second ban must succeed");
    assert_ne!(ban1, ban2, "two bans must be two distinct rows");

    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_bans WHERE user_id = $1 AND lifted_at IS NULL",
    )
    .bind(user_id)
    .fetch_one(&pool)
    .await
    .expect("count active bans");
    assert_eq!(
        active_count, 2,
        "two successive bans must both remain active and distinct"
    );
}

#[sqlx::test]
async fn lift_ban_targets_a_single_row(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let user_id = common::register_test_user(&pool, "lift-single-user").await;
    let moderator_id =
        common::register_test_user_with_role(&pool, "lift-single-moderator", "moderator").await;

    let ban1 = repository::ban_user(&pool, user_id, "first offense", moderator_id, None)
        .await
        .expect("first ban must succeed");
    let ban2 = repository::ban_user(&pool, user_id, "second offense", moderator_id, None)
        .await
        .expect("second ban must succeed");

    repository::lift_ban(&pool, ban1, "appeal accepted", moderator_id)
        .await
        .expect("lift_ban must succeed");

    let lifted1: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT lifted_at FROM user_bans WHERE id = $1")
            .bind(ban1)
            .fetch_one(&pool)
            .await
            .expect("fetch ban1");
    assert!(lifted1.is_some(), "the targeted ban must be lifted");
    let lifted2: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT lifted_at FROM user_bans WHERE id = $1")
            .bind(ban2)
            .fetch_one(&pool)
            .await
            .expect("fetch ban2");
    assert!(
        lifted2.is_none(),
        "lift_ban must target only the requested row, leaving the second active"
    );

    // The user must remain blocked: ban2 is still active.
    let token = common::jwt_for(user_id);
    let body = submit_puzzle(app.clone(), &token, "CuCuCuCu", "Lift Single").await;
    assert_eq!(
        body["error"], "banned",
        "the user must still be blocked by the second, unlifted ban"
    );
}

#[sqlx::test]
async fn lift_ban_twice_is_not_found(pool: PgPool) {
    let user_id = common::register_test_user(&pool, "lift-twice-user").await;
    let moderator_id =
        common::register_test_user_with_role(&pool, "lift-twice-moderator", "moderator").await;
    let ban_id = repository::ban_user(&pool, user_id, "offense", moderator_id, None)
        .await
        .expect("ban must succeed");

    repository::lift_ban(&pool, ban_id, "first lift", moderator_id)
        .await
        .expect("first lift must succeed");

    let (lift_reason, lift_moderator_id): (Option<String>, Option<uuid::Uuid>) =
        sqlx::query_as("SELECT lift_reason, lift_moderator_id FROM user_bans WHERE id = $1")
            .bind(ban_id)
            .fetch_one(&pool)
            .await
            .expect("fetch ban after first lift");

    let other_moderator_id =
        common::register_test_user_with_role(&pool, "lift-twice-other-moderator", "moderator")
            .await;
    let second =
        repository::lift_ban(&pool, ban_id, "second lift attempt", other_moderator_id).await;
    assert!(matches!(second, Err(AppError::NotFound)));

    let (lift_reason_after, lift_moderator_id_after): (Option<String>, Option<uuid::Uuid>) =
        sqlx::query_as("SELECT lift_reason, lift_moderator_id FROM user_bans WHERE id = $1")
            .bind(ban_id)
            .fetch_one(&pool)
            .await
            .expect("fetch ban after second attempt");
    assert_eq!(
        lift_reason, lift_reason_after,
        "the second lift attempt must not rewrite lift_reason"
    );
    assert_eq!(
        lift_moderator_id, lift_moderator_id_after,
        "the second lift attempt must not rewrite lift_moderator_id"
    );
}

#[sqlx::test]
async fn ban_unknown_user_is_not_found(pool: PgPool) {
    let moderator_id =
        common::register_test_user_with_role(&pool, "ban-unknown-moderator", "moderator").await;
    let result =
        repository::ban_user(&pool, uuid::Uuid::new_v4(), "offense", moderator_id, None).await;
    assert!(matches!(result, Err(AppError::NotFound)));
}

// --- 07-06: set_user_role -----------------------------------------------------------------------

#[sqlx::test]
async fn set_user_role_promotes_and_logs(pool: PgPool) {
    let user_id = common::register_test_user(&pool, "role-promote-user").await;
    let admin_id = common::register_test_user_with_role(&pool, "role-promote-admin", "admin").await;

    repository::set_user_role(&pool, user_id, "moderator", admin_id)
        .await
        .expect("promotion must succeed");
    let role: String = sqlx::query_scalar("SELECT role FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .expect("fetch role after promotion");
    assert_eq!(role, "moderator");

    repository::set_user_role(&pool, user_id, "user", admin_id)
        .await
        .expect("demotion must succeed");
    let role_after: String = sqlx::query_scalar("SELECT role FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .expect("fetch role after demotion");
    assert_eq!(role_after, "user");

    let logged = moderation_log_count_str(
        &pool,
        &user_id.to_string(),
        repository::moderation_action::SET_ROLE,
    )
    .await;
    assert_eq!(
        logged, 2,
        "both the promotion and the demotion must each append a set_role log row"
    );
}

#[sqlx::test]
async fn set_user_role_rejects_invalid_role(pool: PgPool) {
    let user_id = common::register_test_user(&pool, "role-invalid-user").await;
    let admin_id = common::register_test_user_with_role(&pool, "role-invalid-admin", "admin").await;

    let result = repository::set_user_role(&pool, user_id, "superadmin", admin_id).await;
    assert!(matches!(result, Err(AppError::BadPayload)));

    let role: String = sqlx::query_scalar("SELECT role FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .expect("fetch role after rejected update");
    assert_eq!(role, "user", "an invalid role value must never be written");
}

// --- 07-06: purge_puzzle -------------------------------------------------------------------------

#[sqlx::test]
async fn purge_puzzle_frees_short_key(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "purge-frees-author").await;
    let author_token = common::jwt_for(author_id);
    let meta = submit_puzzle(app.clone(), &author_token, "CuCuCuCu", "Purge Frees Key").await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id") as i32;
    let short_key = meta["shortKey"]
        .as_str()
        .expect("shortKey is a string")
        .to_string();

    let complete_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/puzzles/complete/{puzzle_id}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-token", &author_token)
                .body(Body::from(
                    json!({ "time": 42.0, "liked": true }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(complete_response.status(), StatusCode::OK);

    let r1 = report_by_new_user(
        &pool,
        app.clone(),
        "purge-frees-reporter",
        puzzle_id,
        "profane",
    )
    .await;
    assert_eq!(r1.status(), StatusCode::OK);

    let moderator_id =
        common::register_test_user_with_role(&pool, "purge-frees-moderator", "moderator").await;
    let returned_short_key = repository::purge_puzzle(&pool, puzzle_id, moderator_id)
        .await
        .expect("purge_puzzle must succeed");
    assert_eq!(returned_short_key, short_key);

    let puzzle_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM puzzles WHERE id = $1")
        .bind(puzzle_id)
        .fetch_one(&pool)
        .await
        .expect("count puzzles");
    assert_eq!(puzzle_count, 0, "the puzzle row itself must be gone");
    let completions_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM puzzle_completions WHERE puzzle_id = $1")
            .bind(puzzle_id)
            .fetch_one(&pool)
            .await
            .expect("count completions");
    assert_eq!(completions_count, 0, "no orphaned completions may remain");
    let reports_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM puzzle_reports WHERE puzzle_id = $1")
            .bind(puzzle_id)
            .fetch_one(&pool)
            .await
            .expect("count reports");
    assert_eq!(reports_count, 0, "no orphaned reports may remain");

    // The freed short_key must be immediately reusable by a brand new submission.
    let new_author_id = common::register_test_user(&pool, "purge-frees-new-author").await;
    let new_author_token = common::jwt_for(new_author_id);
    let new_meta = submit_puzzle(
        app.clone(),
        &new_author_token,
        &short_key,
        "Reused Short Key",
    )
    .await;
    assert!(
        new_meta.get("id").is_some(),
        "resubmission with the freed shortKey must succeed, not fail with short-key-already-taken"
    );
}

#[sqlx::test]
async fn purge_puzzle_keeps_audit_trail(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "purge-audit-author").await;
    let author_token = common::jwt_for(author_id);
    let meta = submit_puzzle(app.clone(), &author_token, "CuCuCuCu", "Purge Audit Trail").await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id") as i32;

    let moderator_id =
        common::register_test_user_with_role(&pool, "purge-audit-moderator", "moderator").await;
    repository::purge_puzzle(&pool, puzzle_id, moderator_id)
        .await
        .expect("purge_puzzle must succeed");

    let logged = moderation_log_count(
        &pool,
        puzzle_id,
        repository::moderation_action::DELETE_PUZZLE,
    )
    .await;
    assert_eq!(
        logged, 1,
        "the audit trail must survive the puzzle's own destruction"
    );
}

#[sqlx::test]
async fn purge_unknown_puzzle_is_not_found(pool: PgPool) {
    let moderator_id =
        common::register_test_user_with_role(&pool, "purge-unknown-moderator", "moderator").await;
    let result = repository::purge_puzzle(&pool, 999_999, moderator_id).await;
    assert!(matches!(result, Err(AppError::NotFound)));

    let logged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM moderation_log")
        .fetch_one(&pool)
        .await
        .expect("count moderation_log rows");
    assert_eq!(logged, 0, "a not-found purge must never write an audit row");
}
