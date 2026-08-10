mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt; // for `oneshot`

// D-01/D-02/D-03: proves the two-table, per-user, per-class, sliding-window design end to end
// against the real HTTP routes, using the migration's own seeded defaults
// (`migrations/20260810000003_rate_limiting.sql`: write 5/h + 20/j, read 500/h) wherever a test
// does not deliberately override them via a direct `UPDATE rate_limit_config` or
// `common::relax_rate_limits`.

async fn body_to_json(response: axum::response::Response) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect response body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("response body is valid JSON")
}

/// D-17: every rejection assertion below checks BOTH halves of the contract -- HTTP 200, never a
/// 4xx/5xx, and the exact literal body `{"error":"ratelimit"}`, per ADR 0007.
async fn assert_rate_limited(response: axum::response::Response) {
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_to_json(response).await;
    assert_eq!(body, json!({ "error": "ratelimit" }));
}

/// Asserts a `submit` response is a genuinely successful `PuzzleMetadata` body (an `id` field
/// present, no `error` key) -- stronger than a bare HTTP 200 status check, which this taxonomy
/// returns for both success AND every business rejection alike (`error_taxonomy` in
/// `tests/persistence.rs`). Every "this write must succeed" assertion in this file goes through
/// this helper so a validation failure unrelated to rate limiting is never silently mistaken for
/// quota being available.
async fn assert_puzzle_created(response: axum::response::Response) {
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_to_json(response).await;
    assert!(
        body.get("id").is_some() && body.get("error").is_none(),
        "expected a successful PuzzleMetadata body, got: {body}"
    );
}

/// Produces a distinct, grammar-valid shape short key per index (`validation::is_valid_shape_short_key`:
/// a single 8-character layer, 4 quadrants of `<shape><color>`). Varying only the first quadrant
/// yields 4 shapes * 8 colors = 32 unique keys, comfortably above this file's per-test write counts.
fn unique_short_key(n: usize) -> String {
    const SHAPES: [char; 4] = ['R', 'C', 'S', 'W'];
    const COLORS: [char; 8] = ['r', 'g', 'b', 'y', 'p', 'c', 'w', 'u'];
    let shape = SHAPES[n % SHAPES.len()];
    let color = COLORS[(n / SHAPES.len()) % COLORS.len()];
    // Exactly 8 characters total (one layer, 4 quadrants of 2 chars): the varying quadrant first,
    // three fixed `Cu` quadrants after.
    format!("{shape}{color}CuCuCu")
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
        "title": "Ratelimit Puzzle",
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

async fn count_write_events(pool: &PgPool, user_id: uuid::Uuid) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM rate_limit_events WHERE user_id = $1 AND route_class = 'write'",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .expect("count rate_limit_events")
}

/// D-01/D-02: with the migration's seeded default (5/hour), a same user's 6th write within the
/// hour is rejected with the taxonomy `ratelimit` code -- HTTP 200, never a bare 4xx.
#[sqlx::test]
async fn write_limit_blocks_sixth_submission_within_the_hour(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let user_id = common::register_test_user(&pool, "ratelimit-write-user").await;
    let token = common::jwt_for(user_id);

    for i in 0..5 {
        let response = submit_puzzle(app.clone(), &token, &unique_short_key(i)).await;
        assert_puzzle_created(response).await;
    }

    let sixth = submit_puzzle(app, &token, &unique_short_key(5)).await;
    assert_rate_limited(sixth).await;
}

/// D-01: the quota is keyed by `user_id` -- a second user's writes are entirely unaffected by the
/// first user's exhausted quota.
#[sqlx::test]
async fn write_limit_is_per_user(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let first_user = common::register_test_user(&pool, "ratelimit-user-a").await;
    let first_token = common::jwt_for(first_user);
    for i in 0..5 {
        let response = submit_puzzle(app.clone(), &first_token, &unique_short_key(i)).await;
        assert_puzzle_created(response).await;
    }
    let sixth = submit_puzzle(app.clone(), &first_token, &unique_short_key(5)).await;
    assert_rate_limited(sixth).await;

    let second_user = common::register_test_user(&pool, "ratelimit-user-b").await;
    let second_token = common::jwt_for(second_user);
    let response = submit_puzzle(app, &second_token, &unique_short_key(6)).await;
    assert_puzzle_created(response).await;
}

/// D-02: `read` and `write` have entirely separate quotas -- saturating `write` never affects
/// `list/new` (a `read`-class route).
#[sqlx::test]
async fn read_limit_is_separate_from_write_limit(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let user_id = common::register_test_user(&pool, "ratelimit-read-vs-write").await;
    let token = common::jwt_for(user_id);

    for i in 0..5 {
        let response = submit_puzzle(app.clone(), &token, &unique_short_key(i)).await;
        assert_puzzle_created(response).await;
    }
    let sixth = submit_puzzle(app.clone(), &token, &unique_short_key(5)).await;
    assert_rate_limited(sixth).await;

    let list_response = list_new(app, &token).await;
    assert_eq!(
        list_response.status(),
        StatusCode::OK,
        "the write quota being exhausted must never affect the separate read quota"
    );
    let body = body_to_json(list_response).await;
    assert!(
        body.as_array().is_some(),
        "list/new must still respond with a normal JSON array, not a ratelimit rejection"
    );
}

/// D-01/07-RESEARCH.md "Don't Hand-Roll": the window is SLIDING, counted by timestamped events,
/// never a calendar bucket that resets on a fixed schedule -- a request rejected for exceeding a
/// 1-second window becomes acceptable again after a REAL wait past that window, proven with
/// `tokio::time::sleep`, never by rewriting a stored timestamp.
#[sqlx::test]
async fn sliding_window_releases_quota(pool: PgPool) {
    sqlx::query(
        "UPDATE rate_limit_config SET window_seconds = 1 WHERE route_class = 'write' AND window_seconds = 3600",
    )
    .execute(&pool)
    .await
    .expect("narrow the hourly write window to 1 second for this test");

    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let user_id = common::register_test_user(&pool, "ratelimit-sliding-window").await;
    let token = common::jwt_for(user_id);

    for i in 0..5 {
        let response = submit_puzzle(app.clone(), &token, &unique_short_key(i)).await;
        assert_puzzle_created(response).await;
    }
    let sixth = submit_puzzle(app.clone(), &token, &unique_short_key(5)).await;
    assert_rate_limited(sixth).await;

    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

    let seventh = submit_puzzle(app, &token, &unique_short_key(6)).await;
    assert_puzzle_created(seventh).await;
}

/// D-02: several windows coexist for the same class, and the MOST restrictive one wins. Here the
/// hourly window is raised well above this test's write count (so it never blocks), while the
/// daily window is lowered to exactly this test's write count -- the 6th write passes the raised
/// hourly cap but is rejected by the daily cap.
#[sqlx::test]
async fn both_configured_windows_apply(pool: PgPool) {
    sqlx::query(
        "UPDATE rate_limit_config SET limit_count = 1000 WHERE route_class = 'write' AND window_seconds = 3600",
    )
    .execute(&pool)
    .await
    .expect("raise the hourly write limit for this test");
    sqlx::query(
        "UPDATE rate_limit_config SET limit_count = 5 WHERE route_class = 'write' AND window_seconds = 86400",
    )
    .execute(&pool)
    .await
    .expect("lower the daily write limit for this test");

    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let user_id = common::register_test_user(&pool, "ratelimit-both-windows").await;
    let token = common::jwt_for(user_id);

    for i in 0..5 {
        let response = submit_puzzle(app.clone(), &token, &unique_short_key(i)).await;
        assert_puzzle_created(response).await;
    }

    let sixth = submit_puzzle(app, &token, &unique_short_key(5)).await;
    assert_rate_limited(sixth).await;
}

/// D-01: a class with zero configured rows imposes no limit at all -- `common::relax_rate_limits`
/// (which deletes every `rate_limit_config` row) lets 10 consecutive writes all succeed.
#[sqlx::test]
async fn zero_config_means_no_limit(pool: PgPool) {
    common::relax_rate_limits(&pool).await;

    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let user_id = common::register_test_user(&pool, "ratelimit-zero-config").await;
    let token = common::jwt_for(user_id);

    for i in 0..10 {
        let response = submit_puzzle(app.clone(), &token, &unique_short_key(i)).await;
        assert_puzzle_created(response).await;
    }
}

/// D-01/T-07-36: `check_and_record`'s opportunistic purge removes this user+class's events older
/// than the largest configured window -- a stale event manually pushed 3 days into the past
/// disappears from `rate_limit_events` by the time the next write's purge step runs.
#[sqlx::test]
async fn rate_limit_events_are_pruned(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let user_id = common::register_test_user(&pool, "ratelimit-pruned").await;
    let token = common::jwt_for(user_id);

    let first = submit_puzzle(app.clone(), &token, &unique_short_key(0)).await;
    assert_puzzle_created(first).await;
    assert_eq!(count_write_events(&pool, user_id).await, 1);

    sqlx::query(
        "UPDATE rate_limit_events SET occurred_at = now() - interval '3 days' WHERE user_id = $1 AND route_class = 'write'",
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .expect("simulate a stale event 3 days in the past");

    let second = submit_puzzle(app, &token, &unique_short_key(1)).await;
    assert_puzzle_created(second).await;

    assert_eq!(
        count_write_events(&pool, user_id).await,
        1,
        "the 3-day-old event (well past the largest configured write window, 1 day) must have \
         been purged by the second write, leaving only that second write's own fresh event"
    );
}

/// D-01: a request rejected for exceeding its quota must never itself be recorded as an event --
/// otherwise a rejected request would perpetually keep the user pinned at the limit.
#[sqlx::test]
async fn rejected_request_records_no_event(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let user_id = common::register_test_user(&pool, "ratelimit-rejected-no-event").await;
    let token = common::jwt_for(user_id);

    for i in 0..5 {
        let response = submit_puzzle(app.clone(), &token, &unique_short_key(i)).await;
        assert_puzzle_created(response).await;
    }
    let count_before = count_write_events(&pool, user_id).await;
    assert_eq!(count_before, 5);

    let sixth = submit_puzzle(app, &token, &unique_short_key(5)).await;
    assert_rate_limited(sixth).await;

    let count_after = count_write_events(&pool, user_id).await;
    assert_eq!(
        count_after, count_before,
        "a rejected request must add no row to rate_limit_events"
    );
}
