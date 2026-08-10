//! 07-11-PLAN.md: ROADMAP SC2's literal acceptance criterion for REQ-moderation -- ONE test
//! proving the full report -> automatic hide -> review -> sanction -> lift lifecycle end to end,
//! composing assertions already proven individually by `tests/moderation.rs`,
//! `tests/moderation_routes.rs` and `tests/bans.rs`. Each of the nine steps below asserts an
//! observable state (a response body, a database row, or a row count), never merely the absence
//! of an error -- the value of this test is the enchainment across steps, not fine-grained
//! redundancy already covered by the files above.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
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

/// Generic request builder, same shape as `tests/moderation_routes.rs::send` -- an optional
/// `x-token` header and an optional JSON body, since this scenario spans routes across the
/// `puzzles` AND `moderation` families.
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

async fn submit(app: axum::Router, token: &str, short_key: &str, title: &str) -> Value {
    let body = json!({
        "title": title,
        "shortKey": short_key,
        "data": sample_game_data().to_string(),
    });
    let response = send(app, "POST", "/v1/puzzles/submit", Some(token), Some(body)).await;
    assert_eq!(response.status(), StatusCode::OK);
    body_to_json(response).await
}

async fn complete(app: axum::Router, token: &str, puzzle_id: i64, time: f32, liked: bool) {
    let response = send(
        app,
        "POST",
        &format!("/v1/puzzles/complete/{puzzle_id}"),
        Some(token),
        Some(json!({ "time": time, "liked": liked })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

async fn report(app: axum::Router, token: &str, puzzle_id: i64, reason: &str) {
    let response = send(
        app,
        "POST",
        &format!("/v1/puzzles/report/{puzzle_id}"),
        Some(token),
        Some(json!({ "reason": reason })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

async fn download(app: axum::Router, token: &str, id_or_key: &str) -> axum::response::Response {
    send(
        app,
        "GET",
        &format!("/v1/puzzles/download/{id_or_key}"),
        Some(token),
        None,
    )
    .await
}

async fn list_ids(app: axum::Router, token: &str, category: &str) -> Vec<i64> {
    let response = send(
        app,
        "GET",
        &format!("/v1/puzzles/list/{category}"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    body_to_json(response)
        .await
        .as_array()
        .expect("list response is an array")
        .iter()
        .map(|p| p["id"].as_i64().expect("puzzle id is a number"))
        .collect()
}

async fn search_ids(app: axum::Router, token: &str, term: &str) -> Vec<i64> {
    let response = send(
        app,
        "POST",
        "/v1/puzzles/search",
        Some(token),
        Some(json!({ "searchTerm": term })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    body_to_json(response)
        .await
        .as_array()
        .expect("search response is an array")
        .iter()
        .map(|p| p["id"].as_i64().expect("puzzle id is a number"))
        .collect()
}

/// Reads `hidden_at`/`hidden_by` directly -- the ground truth ADR 0002's tri-state is checked
/// against, same helper shape as `tests/moderation.rs::hidden_state`.
async fn hidden_state(
    pool: &PgPool,
    puzzle_id: i64,
) -> (Option<chrono::DateTime<chrono::Utc>>, Option<uuid::Uuid>) {
    sqlx::query_as::<_, (Option<chrono::DateTime<chrono::Utc>>, Option<uuid::Uuid>)>(
        "SELECT hidden_at, hidden_by FROM puzzles WHERE id = $1",
    )
    .bind(puzzle_id as i32)
    .fetch_one(pool)
    .await
    .expect("puzzle row must exist")
}

/// ROADMAP SC2 / REQ-moderation's literal acceptance criterion: the full
/// signalement -> revue -> sanction scenario, proven end to end in a single test.
///
/// The helper below that lifts every configured write/read threshold runs first: this scenario
/// deliberately drives more write-class requests than the seeded `write` class hourly ceiling
/// (5/h, `migrations/20260810000003_rate_limiting.sql`) through a single test account -- the
/// behavior under test is the moderation flow, not the rate limiter, which already has its own
/// dedicated coverage in `tests/ratelimit.rs`.
#[sqlx::test]
async fn full_report_review_sanction_scenario(pool: PgPool) {
    common::relax_rate_limits(&pool).await;
    let state = common::test_state(pool.clone());

    // --- Step 1 (REQ-business-logic, D-14/D-16/D-17/D-19, ROADMAP SC1): an author submits a
    // puzzle; three other accounts complete it, two of them liking it -- the aggregates on the
    // next download must reflect 3 completions, 2 likes, a non-null averageTime and a non-null
    // difficulty.
    let author_id = common::register_test_user(&pool, "scenario-author").await;
    let author_token = common::jwt_for(author_id);
    let submitted = submit(
        savez::app(state.clone()),
        &author_token,
        "ScScScSc",
        "Scenario Puzzle",
    )
    .await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number");

    let mut participant_tokens = Vec::new();
    for (i, liked) in [true, true, false].into_iter().enumerate() {
        let participant_id =
            common::register_test_user(&pool, &format!("scenario-participant-{i}")).await;
        let participant_token = common::jwt_for(participant_id);
        complete(
            savez::app(state.clone()),
            &participant_token,
            puzzle_id,
            30.0 + i as f32,
            liked,
        )
        .await;
        participant_tokens.push(participant_token);
    }

    // A first download bumps `downloads` from 0 to 1 -- but its OWN response still reports
    // `difficulty: null`, since `routes::puzzles::download` computes `difficulty` from the
    // PRE-increment `downloads` value (`src/routes/puzzles.rs::download`'s own doc comment,
    // 07-02-PLAN.md `<interfaces>`): with `downloads` read as 0 before this increment, the
    // `downloads = 0 => NULL` rule still applies to this first response. A SECOND download is
    // needed to observe `difficulty` as a real, non-null ratio.
    let first_download = download(
        savez::app(state.clone()),
        &author_token,
        &puzzle_id.to_string(),
    )
    .await;
    assert_eq!(first_download.status(), StatusCode::OK);
    assert_eq!(
        body_to_json(first_download).await["meta"]["difficulty"],
        Value::Null
    );

    let download_response = download(
        savez::app(state.clone()),
        &author_token,
        &puzzle_id.to_string(),
    )
    .await;
    assert_eq!(download_response.status(), StatusCode::OK);
    let downloaded = body_to_json(download_response).await;
    assert_eq!(downloaded["meta"]["completions"], json!(3));
    assert_eq!(downloaded["meta"]["likes"], json!(2));
    assert!(
        downloaded["meta"]["averageTime"].is_number(),
        "averageTime must be non-null after 3 completions"
    );
    assert!(
        downloaded["meta"]["difficulty"].is_number(),
        "difficulty must be non-null once a prior download has made the stored counter non-zero"
    );

    // --- Step 2 (D-05, ADR 0002): the same three accounts report the puzzle with the SAME
    // reason; after the third distinct pending report, hidden_at is set and hidden_by is NULL
    // (the automatic-threshold tri-state, distinct from a self-hide or a moderator's manual hide).
    for participant_token in &participant_tokens {
        report(
            savez::app(state.clone()),
            participant_token,
            puzzle_id,
            "profane",
        )
        .await;
    }
    let (hidden_at, hidden_by) = hidden_state(&pool, puzzle_id).await;
    assert!(
        hidden_at.is_some(),
        "3 distinct pending reports must auto-hide the puzzle"
    );
    assert_eq!(
        hidden_by, None,
        "an automatic threshold hide must leave hidden_by NULL, never attribute it to a moderator"
    );

    // --- Step 3 (ADR 0002, ROADMAP SC2): the puzzle disappears from every catalog view for
    // everyone, but stays reachable by direct access for its author and for a moderator.
    let moderator_id =
        common::register_test_user_with_role(&pool, "scenario-moderator", "moderator").await;
    let moderator_token = common::jwt_for(moderator_id);
    assert!(
        !list_ids(savez::app(state.clone()), &moderator_token, "new")
            .await
            .contains(&puzzle_id)
    );
    assert!(
        !list_ids(savez::app(state.clone()), &moderator_token, "top-rated")
            .await
            .contains(&puzzle_id)
    );
    assert!(
        !search_ids(savez::app(state.clone()), &moderator_token, "Scenario")
            .await
            .contains(&puzzle_id)
    );
    assert_eq!(
        download(
            savez::app(state.clone()),
            &author_token,
            &puzzle_id.to_string()
        )
        .await
        .status(),
        StatusCode::OK,
        "the puzzle's own author must still be able to download it while hidden"
    );
    assert_eq!(
        download(
            savez::app(state.clone()),
            &moderator_token,
            &puzzle_id.to_string()
        )
        .await
        .status(),
        StatusCode::OK,
        "a moderator must be able to download a hidden puzzle directly (ROADMAP SC2)"
    );

    // --- Step 4 (SPEC §4.6 report queue): the moderator sees all three reports pending, and the
    // author's confirmed-report counter is still 0 (nothing has been resolved yet).
    let queue_response = send(
        savez::app(state.clone()),
        "GET",
        "/v1/moderation/reports",
        Some(&moderator_token),
        None,
    )
    .await;
    assert_eq!(queue_response.status(), StatusCode::OK);
    let queue_body = body_to_json(queue_response).await;
    let scenario_entries: Vec<Value> = queue_body
        .as_array()
        .expect("report queue is an array")
        .iter()
        .filter(|e| e["puzzleId"] == json!(puzzle_id))
        .cloned()
        .collect();
    assert_eq!(
        scenario_entries.len(),
        3,
        "all three reports must be visible in the default (pending) queue"
    );
    for entry in &scenario_entries {
        assert_eq!(entry["status"], json!("pending"));
        assert_eq!(entry["authorUpheldReports"], json!(0));
    }
    let report_ids: Vec<i64> = scenario_entries
        .iter()
        .map(|e| e["id"].as_i64().expect("report id is a number"))
        .collect();

    // --- Step 5 (D-06/D-07/D-08): resolving one report as `upheld` resolves every same-reason
    // sibling too, the puzzle stays hidden, no ban is created, and the author's counter reaches 3.
    let resolve_response = send(
        savez::app(state.clone()),
        "POST",
        &format!("/v1/moderation/reports/{}/resolve", report_ids[0]),
        Some(&moderator_token),
        Some(json!({ "status": "upheld", "notes": "confirmed profane title" })),
    )
    .await;
    assert_eq!(resolve_response.status(), StatusCode::OK);
    let mut resolved: Vec<i64> = body_to_json(resolve_response).await["resolved"]
        .as_array()
        .expect("resolved is an array")
        .iter()
        .map(|v| v.as_i64().expect("resolved id is a number"))
        .collect();
    resolved.sort();
    let mut expected_report_ids = report_ids.clone();
    expected_report_ids.sort();
    assert_eq!(
        resolved, expected_report_ids,
        "resolving one report must resolve all 3 same-reason siblings (D-08)"
    );

    let (hidden_at_after_resolve, hidden_by_after_resolve) = hidden_state(&pool, puzzle_id).await;
    assert_eq!(
        hidden_at_after_resolve, hidden_at,
        "resolving reports must not touch the pre-existing auto-hide (D-07)"
    );
    assert_eq!(hidden_by_after_resolve, None);
    let ban_count_before_sanction: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_bans WHERE user_id = $1")
            .bind(author_id)
            .fetch_one(&pool)
            .await
            .expect("count user_bans");
    assert_eq!(
        ban_count_before_sanction, 0,
        "resolving reports upheld must never insert a ban on its own (D-06)"
    );

    let all_reports_response = send(
        savez::app(state.clone()),
        "GET",
        "/v1/moderation/reports?status=all",
        Some(&moderator_token),
        None,
    )
    .await;
    let all_reports_body = body_to_json(all_reports_response).await;
    let author_counter_entry = all_reports_body
        .as_array()
        .expect("report queue is an array")
        .iter()
        .find(|e| e["puzzleId"] == json!(puzzle_id))
        .expect(
            "the queue must still list the now-resolved reports of this puzzle under status=all",
        );
    assert_eq!(
        author_counter_entry["authorUpheldReports"],
        json!(3),
        "the author's confirmed-report counter must reach 3 after resolving all 3 same-reason reports"
    );

    // --- Step 6 (D-11/D-13): an admin bans the author with an expiry -- submit/complete/report
    // are refused with `banned`, reading stays available.
    let admin_id = common::register_test_user_with_role(&pool, "scenario-admin", "admin").await;
    let admin_token = common::jwt_for(admin_id);
    let expires_at = (chrono::Utc::now() + chrono::Duration::days(7)).to_rfc3339();
    let ban_response = send(
        savez::app(state.clone()),
        "POST",
        &format!("/v1/moderation/users/{author_id}/ban"),
        Some(&admin_token),
        Some(json!({ "reason": "repeated profane titles", "expiresAt": expires_at })),
    )
    .await;
    assert_eq!(ban_response.status(), StatusCode::OK);
    let ban_body = body_to_json(ban_response).await;
    assert_eq!(ban_body["success"], json!(true));
    let ban_id = ban_body["banId"].as_i64().expect("banId is a number");

    // `TEST_AUTH_CACHE_TTL` is 1ms (tests/common/mod.rs) -- let it elapse so the requests below
    // observe the fresh ban state, same precaution `tests/bans.rs::second_active_ban_coexists`
    // takes for the identical reason.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let banned_submit = send(
        savez::app(state.clone()),
        "POST",
        "/v1/puzzles/submit",
        Some(&author_token),
        Some(json!({
            "title": "Blocked Submit",
            "shortKey": "ScScSySy",
            "data": sample_game_data().to_string(),
        })),
    )
    .await;
    assert_eq!(
        body_to_json(banned_submit).await,
        json!({ "error": "banned" })
    );

    let banned_complete = send(
        savez::app(state.clone()),
        "POST",
        &format!("/v1/puzzles/complete/{puzzle_id}"),
        Some(&author_token),
        Some(json!({ "time": 10.0, "liked": false })),
    )
    .await;
    assert_eq!(
        body_to_json(banned_complete).await,
        json!({ "error": "banned" })
    );

    let banned_report = send(
        savez::app(state.clone()),
        "POST",
        &format!("/v1/puzzles/report/{puzzle_id}"),
        Some(&author_token),
        Some(json!({ "reason": "profane" })),
    )
    .await;
    assert_eq!(
        body_to_json(banned_report).await,
        json!({ "error": "banned" })
    );

    assert_eq!(
        download(
            savez::app(state.clone()),
            &author_token,
            &puzzle_id.to_string()
        )
        .await
        .status(),
        StatusCode::OK,
        "a banned account must still be able to read (D-13 names only submit/complete/report/login)"
    );

    // --- Step 7 (admin-only, transactional purge): the puzzle row disappears, no orphaned
    // completions/reports remain, and the response proves the shortKey is now free.
    let purge_response = send(
        savez::app(state.clone()),
        "DELETE",
        &format!("/v1/moderation/puzzles/{puzzle_id}"),
        Some(&admin_token),
        None,
    )
    .await;
    assert_eq!(purge_response.status(), StatusCode::OK);
    let purge_body = body_to_json(purge_response).await;
    assert_eq!(purge_body["success"], json!(true));
    let freed_short_key = purge_body["freedShortKey"]
        .as_str()
        .expect("freedShortKey is a string")
        .to_string();
    assert_eq!(freed_short_key, "ScScScSc");

    let puzzle_row_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM puzzles WHERE id = $1")
        .bind(puzzle_id as i32)
        .fetch_one(&pool)
        .await
        .expect("count puzzles");
    assert_eq!(puzzle_row_count, 0, "the puzzle row itself must be gone");
    let orphaned_completions: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM puzzle_completions WHERE puzzle_id = $1")
            .bind(puzzle_id as i32)
            .fetch_one(&pool)
            .await
            .expect("count completions");
    assert_eq!(
        orphaned_completions, 0,
        "no orphaned completions may remain"
    );
    let orphaned_reports: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM puzzle_reports WHERE puzzle_id = $1")
            .bind(puzzle_id as i32)
            .fetch_one(&pool)
            .await
            .expect("count reports");
    assert_eq!(orphaned_reports, 0, "no orphaned reports may remain");

    // --- Step 8 (D-11): lifting the ban restores the author's write access, who reuses the freed
    // shortKey with a fresh submission.
    let lift_response = send(
        savez::app(state.clone()),
        "POST",
        &format!("/v1/moderation/users/{ban_id}/lift-ban"),
        Some(&admin_token),
        Some(json!({ "reason": "sanction served" })),
    )
    .await;
    assert_eq!(lift_response.status(), StatusCode::OK);
    assert_eq!(
        body_to_json(lift_response).await,
        json!({ "success": true })
    );

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let resubmitted = submit(
        savez::app(state.clone()),
        &author_token,
        &freed_short_key,
        "Reinstated Puzzle",
    )
    .await;
    assert!(
        resubmitted.get("id").is_some(),
        "the unbanned author must be able to reuse the freed shortKey with a fresh submission"
    );

    // --- Step 9 (SC4, append-only audit trail): the admin log carries exactly the four actions
    // above, in chronological order.
    let log_response = send(
        savez::app(state.clone()),
        "GET",
        "/v1/moderation/log",
        Some(&admin_token),
        None,
    )
    .await;
    assert_eq!(log_response.status(), StatusCode::OK);
    let log_body = body_to_json(log_response).await;
    let mut actions: Vec<String> = log_body
        .as_array()
        .expect("log response is an array")
        .iter()
        .map(|e| {
            e["action"]
                .as_str()
                .expect("action is a string")
                .to_string()
        })
        .collect();
    // `GET /v1/moderation/log` returns newest-first (`created_at DESC, id DESC`) -- reverse to
    // read the actions back in the chronological order they actually happened.
    actions.reverse();
    assert_eq!(
        actions,
        vec!["resolve_report", "ban_user", "delete_puzzle", "lift_ban"],
        "the audit trail must contain exactly these four actions, in this chronological order"
    );
}
