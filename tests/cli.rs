//! 07-09-PLAN.md Task 3: proves every `savez mod <action>` subcommand's DATABASE effect by calling
//! `savez::cli::moderation::dispatch` directly with a `ModAction` value constructed in-process --
//! no child process spawned, no argument string parsed here (the grammar itself is already proven
//! by `src/cli/mod.rs`'s own `Cli::try_parse_from` unit tests, Task 1). What remains to prove here
//! is that `dispatch` actually reaches the database and writes the same rows the equivalent HTTP
//! route/repository call would.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use savez::cli::moderation::{CliError, dispatch};
use savez::cli::{
    LangArg, ModAction, ProfanityAction, RatelimitAction, ResolveStatus, RoleArg, RouteClassArg,
};
use savez::profanity;
use savez::repository;
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

/// Produces a distinct, grammar-valid shape short key per index (one 8-character layer, 4
/// quadrants of `<shape><color>`) -- same generator `tests/ratelimit.rs::unique_short_key` uses,
/// duplicated here since `tests/common` is intentionally kept free of puzzle-domain helpers.
fn unique_short_key(n: usize) -> String {
    const SHAPES: [char; 4] = ['R', 'C', 'S', 'W'];
    const COLORS: [char; 8] = ['r', 'g', 'b', 'y', 'p', 'c', 'w', 'u'];
    let shape = SHAPES[n % SHAPES.len()];
    let color = COLORS[(n / SHAPES.len()) % COLORS.len()];
    format!("{shape}{color}CuCuCu")
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

/// Registers a fresh user and reports `puzzle_id` on their behalf -- every distinct pending-report
/// call in this file needs a NEW reporter (`UNIQUE(user_id, puzzle_id)`).
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

/// Reads `hidden_at`/`hidden_by` directly -- the ground truth `cli_mod_hide_then_unhide_writes_log`
/// checks against.
async fn hidden_state(
    pool: &PgPool,
    puzzle_id: i32,
) -> (Option<chrono::DateTime<chrono::Utc>>, Option<Uuid>) {
    sqlx::query_as::<_, (Option<chrono::DateTime<chrono::Utc>>, Option<Uuid>)>(
        "SELECT hidden_at, hidden_by FROM puzzles WHERE id = $1",
    )
    .bind(puzzle_id)
    .fetch_one(pool)
    .await
    .expect("puzzle row must exist")
}

async fn moderation_log_count(pool: &PgPool, target_id: &str, action: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM moderation_log WHERE target_id = $1 AND action = $2",
    )
    .bind(target_id)
    .bind(action)
    .fetch_one(pool)
    .await
    .expect("count moderation_log rows")
}

/// Reads the `moderator_id` of the most recent `moderation_log` row matching `(target_id,
/// action)` -- proves T-07-48's attribution claim, not just that a row exists.
async fn moderation_log_moderator(pool: &PgPool, target_id: &str, action: &str) -> Uuid {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT moderator_id FROM moderation_log WHERE target_id = $1 AND action = $2 ORDER BY id DESC LIMIT 1",
    )
    .bind(target_id)
    .bind(action)
    .fetch_one(pool)
    .await
    .expect("fetch moderation_log moderator_id")
}

#[sqlx::test]
async fn cli_mod_hide_then_unhide_writes_log(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "cli-hide-author").await;
    let author_token = common::jwt_for(author_id);
    let meta = submit_puzzle(
        app.clone(),
        &author_token,
        &unique_short_key(0),
        "Cli Hide Puzzle",
    )
    .await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id is a number") as i32;

    let moderator_id =
        common::register_test_user_with_role(&pool, "cli-hide-moderator", "moderator").await;

    dispatch(
        &pool,
        ModAction::Hide {
            puzzle_id,
            reason: Some("cli test hide".to_string()),
            moderator: "cli-hide-moderator".to_string(),
        },
    )
    .await
    .expect("hide must succeed");

    let (hidden_at, hidden_by) = hidden_state(&pool, puzzle_id).await;
    assert!(hidden_at.is_some(), "hide must set hidden_at");
    assert_eq!(
        hidden_by,
        Some(moderator_id),
        "hidden_by must be the resolved moderator"
    );
    assert_eq!(
        moderation_log_moderator(
            &pool,
            &puzzle_id.to_string(),
            repository::moderation_action::HIDE_PUZZLE
        )
        .await,
        moderator_id
    );

    dispatch(
        &pool,
        ModAction::Unhide {
            puzzle_id,
            reason: None,
            moderator: "cli-hide-moderator".to_string(),
        },
    )
    .await
    .expect("unhide must succeed");

    let (hidden_at, hidden_by) = hidden_state(&pool, puzzle_id).await;
    assert!(hidden_at.is_none(), "unhide must clear hidden_at");
    assert!(hidden_by.is_none(), "unhide must clear hidden_by");

    assert_eq!(
        moderation_log_count(
            &pool,
            &puzzle_id.to_string(),
            repository::moderation_action::HIDE_PUZZLE
        )
        .await,
        1
    );
    assert_eq!(
        moderation_log_count(
            &pool,
            &puzzle_id.to_string(),
            repository::moderation_action::UNHIDE_PUZZLE
        )
        .await,
        1
    );
}

#[sqlx::test]
async fn cli_mod_resolve_marks_sibling_reports(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "cli-resolve-author").await;
    let author_token = common::jwt_for(author_id);
    let meta = submit_puzzle(
        app.clone(),
        &author_token,
        &unique_short_key(1),
        "Cli Resolve Puzzle",
    )
    .await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id is a number") as i32;

    let r1 = report_by_new_user(
        &pool,
        app.clone(),
        "cli-resolve-reporter-1",
        puzzle_id,
        "profane",
    )
    .await;
    assert_eq!(r1.status(), StatusCode::OK);
    let r2 = report_by_new_user(
        &pool,
        app.clone(),
        "cli-resolve-reporter-2",
        puzzle_id,
        "profane",
    )
    .await;
    assert_eq!(r2.status(), StatusCode::OK);
    let r3 = report_by_new_user(
        &pool,
        app.clone(),
        "cli-resolve-reporter-3",
        puzzle_id,
        "trolling",
    )
    .await;
    assert_eq!(r3.status(), StatusCode::OK);

    let report_id: i32 = sqlx::query_scalar(
        "SELECT id FROM puzzle_reports WHERE puzzle_id = $1 AND reason = 'profane' ORDER BY id LIMIT 1",
    )
    .bind(puzzle_id)
    .fetch_one(&pool)
    .await
    .expect("fetch first profane report id");

    let moderator_id =
        common::register_test_user_with_role(&pool, "cli-resolve-moderator", "moderator").await;

    dispatch(
        &pool,
        ModAction::Resolve {
            report_id,
            status: ResolveStatus::Upheld,
            notes: Some("cli resolve".to_string()),
            moderator: "cli-resolve-moderator".to_string(),
        },
    )
    .await
    .expect("resolve must succeed");

    let profane_statuses: Vec<String> = sqlx::query_scalar(
        "SELECT status FROM puzzle_reports WHERE puzzle_id = $1 AND reason = 'profane'",
    )
    .bind(puzzle_id)
    .fetch_all(&pool)
    .await
    .expect("fetch profane statuses");
    assert!(
        profane_statuses.iter().all(|s| s == "upheld"),
        "both profane reports must resolve together (D-08): {profane_statuses:?}"
    );

    let trolling_status: String = sqlx::query_scalar(
        "SELECT status FROM puzzle_reports WHERE puzzle_id = $1 AND reason = 'trolling'",
    )
    .bind(puzzle_id)
    .fetch_one(&pool)
    .await
    .expect("fetch trolling status");
    assert_eq!(
        trolling_status, "pending",
        "a different-reason sibling report must stay untouched (D-08)"
    );

    assert_eq!(
        moderation_log_moderator(
            &pool,
            &report_id.to_string(),
            repository::moderation_action::RESOLVE_REPORT
        )
        .await,
        moderator_id
    );
}

#[sqlx::test]
async fn cli_mod_delete_frees_short_key(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "cli-delete-author").await;
    let author_token = common::jwt_for(author_id);
    let short_key = unique_short_key(2);
    let meta = submit_puzzle(app.clone(), &author_token, &short_key, "Cli Delete Puzzle").await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id is a number") as i32;

    let moderator_id =
        common::register_test_user_with_role(&pool, "cli-delete-moderator", "admin").await;

    dispatch(
        &pool,
        ModAction::Delete {
            puzzle_id,
            moderator: "cli-delete-moderator".to_string(),
        },
    )
    .await
    .expect("delete must succeed");

    let puzzle_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM puzzles WHERE id = $1")
        .bind(puzzle_id)
        .fetch_one(&pool)
        .await
        .expect("count puzzles");
    assert_eq!(puzzle_count, 0, "the puzzle row itself must be gone");

    let new_author_id = common::register_test_user(&pool, "cli-delete-new-author").await;
    let new_author_token = common::jwt_for(new_author_id);
    let new_meta =
        submit_puzzle(app.clone(), &new_author_token, &short_key, "Reused Cli Key").await;
    assert!(
        new_meta.get("id").is_some(),
        "the freed shortKey must be immediately reusable by a brand new submission"
    );

    assert_eq!(
        moderation_log_moderator(
            &pool,
            &puzzle_id.to_string(),
            repository::moderation_action::DELETE_PUZZLE
        )
        .await,
        moderator_id
    );
}

#[sqlx::test]
async fn cli_mod_ban_then_unban_round_trip(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let target_id = common::register_test_user(&pool, "cli-ban-target").await;
    let target_token = common::jwt_for(target_id);
    let moderator_id =
        common::register_test_user_with_role(&pool, "cli-ban-moderator", "moderator").await;

    dispatch(
        &pool,
        ModAction::Ban {
            user: "cli-ban-target".to_string(),
            reason: "cli round trip test".to_string(),
            expires_in: None,
            moderator: "cli-ban-moderator".to_string(),
        },
    )
    .await
    .expect("ban must succeed");

    let ban_id: i32 =
        sqlx::query_scalar("SELECT id FROM user_bans WHERE user_id = $1 ORDER BY id DESC LIMIT 1")
            .bind(target_id)
            .fetch_one(&pool)
            .await
            .expect("fetch created ban id");

    let short_key = unique_short_key(3);
    let banned_body = submit_puzzle(app.clone(), &target_token, &short_key, "Banned Submit").await;
    assert_eq!(
        banned_body,
        json!({ "error": "banned" }),
        "a CLI-created ban must block POST /v1/puzzles/submit"
    );

    dispatch(
        &pool,
        ModAction::Unban {
            ban_id,
            reason: "cli round trip lift".to_string(),
            moderator: "cli-ban-moderator".to_string(),
        },
    )
    .await
    .expect("unban must succeed");

    let unbanned_body =
        submit_puzzle(app.clone(), &target_token, &short_key, "Unbanned Submit").await;
    assert!(
        unbanned_body.get("id").is_some(),
        "after lifting the ban, submission must succeed: {unbanned_body:?}"
    );

    assert_eq!(
        moderation_log_moderator(
            &pool,
            &target_id.to_string(),
            repository::moderation_action::BAN_USER
        )
        .await,
        moderator_id
    );
    assert_eq!(
        moderation_log_moderator(
            &pool,
            &ban_id.to_string(),
            repository::moderation_action::LIFT_BAN
        )
        .await,
        moderator_id
    );
}

#[sqlx::test]
async fn cli_mod_promote_changes_role(pool: PgPool) {
    let target_id = common::register_test_user(&pool, "cli-promote-target").await;
    let moderator_id =
        common::register_test_user_with_role(&pool, "cli-promote-moderator", "admin").await;

    dispatch(
        &pool,
        ModAction::Promote {
            user: "cli-promote-target".to_string(),
            role: RoleArg::Moderator,
            moderator: "cli-promote-moderator".to_string(),
        },
    )
    .await
    .expect("promote must succeed");

    let role: String = sqlx::query_scalar("SELECT role FROM users WHERE id = $1")
        .bind(target_id)
        .fetch_one(&pool)
        .await
        .expect("fetch role");
    assert_eq!(role, "moderator");

    assert_eq!(
        moderation_log_moderator(
            &pool,
            &target_id.to_string(),
            repository::moderation_action::SET_ROLE
        )
        .await,
        moderator_id
    );
}

#[sqlx::test]
async fn cli_mod_ratelimit_set_is_upsert(pool: PgPool) {
    let moderator_id =
        common::register_test_user_with_role(&pool, "cli-ratelimit-moderator", "admin").await;

    dispatch(
        &pool,
        ModAction::Ratelimit {
            action: RatelimitAction::Set {
                class: RouteClassArg::Write,
                window: 3600,
                limit: 5,
                moderator: "cli-ratelimit-moderator".to_string(),
            },
        },
    )
    .await
    .expect("first set must succeed");

    dispatch(
        &pool,
        ModAction::Ratelimit {
            action: RatelimitAction::Set {
                class: RouteClassArg::Write,
                window: 3600,
                limit: 9,
                moderator: "cli-ratelimit-moderator".to_string(),
            },
        },
    )
    .await
    .expect("second set must succeed");

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM rate_limit_config WHERE route_class = 'write' AND window_seconds = 3600")
            .fetch_one(&pool)
            .await
            .expect("count rows");
    assert_eq!(
        count, 1,
        "an upsert on the same class+window must leave exactly one row"
    );

    let limit: i32 =
        sqlx::query_scalar("SELECT limit_count FROM rate_limit_config WHERE route_class = 'write' AND window_seconds = 3600")
            .fetch_one(&pool)
            .await
            .expect("fetch limit");
    assert_eq!(limit, 9, "the last set call's limit must win");

    assert_eq!(
        moderation_log_moderator(&pool, "write", repository::moderation_action::RATELIMIT_SET)
            .await,
        moderator_id
    );
}

#[sqlx::test]
async fn cli_mod_profanity_add_and_list(pool: PgPool) {
    let moderator_id =
        common::register_test_user_with_role(&pool, "cli-profanity-moderator", "moderator").await;

    dispatch(
        &pool,
        ModAction::Profanity {
            action: ProfanityAction::Add {
                word: "cliswearword".to_string(),
                lang: LangArg::En,
                moderator: "cli-profanity-moderator".to_string(),
            },
        },
    )
    .await
    .expect("add must succeed");

    let words = profanity::list_words(&pool, Some("en"))
        .await
        .expect("list words");
    assert!(
        words.iter().any(|(w, l)| w == "cliswearword" && l == "en"),
        "the added word must appear in list_words: {words:?}"
    );

    assert_eq!(
        moderation_log_moderator(
            &pool,
            "cliswearword",
            repository::moderation_action::PROFANITY_UPDATE
        )
        .await,
        moderator_id
    );
}

#[sqlx::test]
async fn cli_mod_unknown_moderator_is_an_error(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "cli-unknown-mod-author").await;
    let author_token = common::jwt_for(author_id);
    let meta = submit_puzzle(
        app.clone(),
        &author_token,
        &unique_short_key(4),
        "Cli Unknown Puzzle",
    )
    .await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id is a number") as i32;

    let log_count_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM moderation_log")
        .fetch_one(&pool)
        .await
        .expect("count log before");

    let result = dispatch(
        &pool,
        ModAction::Hide {
            puzzle_id,
            reason: Some("should never apply".to_string()),
            moderator: "no-such-moderator".to_string(),
        },
    )
    .await;

    assert!(
        matches!(&result, Err(CliError::UnknownUser(name)) if name == "no-such-moderator"),
        "an unresolvable --moderator must produce CliError::UnknownUser, got {result:?}"
    );

    let (hidden_at, _) = hidden_state(&pool, puzzle_id).await;
    assert!(
        hidden_at.is_none(),
        "an unresolvable moderator must not hide the puzzle"
    );

    let log_count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM moderation_log")
        .fetch_one(&pool)
        .await
        .expect("count log after");
    assert_eq!(
        log_count_before, log_count_after,
        "no moderation_log row must be written when the moderator cannot be resolved"
    );
}

#[sqlx::test]
async fn cli_mod_every_write_action_logs(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let moderator_id =
        common::register_test_user_with_role(&pool, "cli-every-action-moderator", "admin").await;
    let moderator_name = "cli-every-action-moderator";

    let author_id = common::register_test_user(&pool, "cli-every-action-author").await;
    let author_token = common::jwt_for(author_id);
    let meta = submit_puzzle(
        app.clone(),
        &author_token,
        &unique_short_key(5),
        "Cli Every Action",
    )
    .await;
    let puzzle_id = meta["id"].as_i64().expect("submitted id is a number") as i32;

    dispatch(
        &pool,
        ModAction::Hide {
            puzzle_id,
            reason: None,
            moderator: moderator_name.to_string(),
        },
    )
    .await
    .expect("hide");
    dispatch(
        &pool,
        ModAction::Unhide {
            puzzle_id,
            reason: None,
            moderator: moderator_name.to_string(),
        },
    )
    .await
    .expect("unhide");

    let r = report_by_new_user(
        &pool,
        app.clone(),
        "cli-every-action-reporter",
        puzzle_id,
        "profane",
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let report_id: i32 =
        sqlx::query_scalar("SELECT id FROM puzzle_reports WHERE puzzle_id = $1 LIMIT 1")
            .bind(puzzle_id)
            .fetch_one(&pool)
            .await
            .expect("fetch report id");
    dispatch(
        &pool,
        ModAction::Resolve {
            report_id,
            status: ResolveStatus::Rejected,
            notes: None,
            moderator: moderator_name.to_string(),
        },
    )
    .await
    .expect("resolve");

    let ban_target_id = common::register_test_user(&pool, "cli-every-action-ban-target").await;
    dispatch(
        &pool,
        ModAction::Ban {
            user: "cli-every-action-ban-target".to_string(),
            reason: "test".to_string(),
            expires_in: None,
            moderator: moderator_name.to_string(),
        },
    )
    .await
    .expect("ban");
    let ban_id: i32 = sqlx::query_scalar("SELECT id FROM user_bans WHERE user_id = $1")
        .bind(ban_target_id)
        .fetch_one(&pool)
        .await
        .expect("fetch ban id");
    dispatch(
        &pool,
        ModAction::Unban {
            ban_id,
            reason: "test lift".to_string(),
            moderator: moderator_name.to_string(),
        },
    )
    .await
    .expect("unban");

    let promote_target_id =
        common::register_test_user(&pool, "cli-every-action-promote-target").await;
    dispatch(
        &pool,
        ModAction::Promote {
            user: "cli-every-action-promote-target".to_string(),
            role: RoleArg::Moderator,
            moderator: moderator_name.to_string(),
        },
    )
    .await
    .expect("promote");
    let _ = promote_target_id;

    let delete_meta = submit_puzzle(
        app.clone(),
        &author_token,
        &unique_short_key(6),
        "Cli Every Delete",
    )
    .await;
    let delete_puzzle_id = delete_meta["id"]
        .as_i64()
        .expect("submitted id is a number") as i32;
    dispatch(
        &pool,
        ModAction::Delete {
            puzzle_id: delete_puzzle_id,
            moderator: moderator_name.to_string(),
        },
    )
    .await
    .expect("delete");

    dispatch(
        &pool,
        ModAction::Ratelimit {
            action: RatelimitAction::Set {
                class: RouteClassArg::Read,
                window: 60,
                limit: 100,
                moderator: moderator_name.to_string(),
            },
        },
    )
    .await
    .expect("ratelimit set");

    dispatch(
        &pool,
        ModAction::Profanity {
            action: ProfanityAction::Add {
                word: "everyactionword".to_string(),
                lang: LangArg::Fr,
                moderator: moderator_name.to_string(),
            },
        },
    )
    .await
    .expect("profanity add");
    dispatch(
        &pool,
        ModAction::Profanity {
            action: ProfanityAction::Remove {
                word: "everyactionword".to_string(),
                moderator: moderator_name.to_string(),
            },
        },
    )
    .await
    .expect("profanity remove");

    for action in [
        repository::moderation_action::HIDE_PUZZLE,
        repository::moderation_action::UNHIDE_PUZZLE,
        repository::moderation_action::RESOLVE_REPORT,
        repository::moderation_action::BAN_USER,
        repository::moderation_action::LIFT_BAN,
        repository::moderation_action::SET_ROLE,
        repository::moderation_action::DELETE_PUZZLE,
        repository::moderation_action::RATELIMIT_SET,
        repository::moderation_action::PROFANITY_UPDATE,
    ] {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM moderation_log WHERE moderator_id = $1 AND action = $2",
        )
        .bind(moderator_id)
        .bind(action)
        .fetch_one(&pool)
        .await
        .expect("count action rows");
        assert!(
            count >= 1,
            "expected at least one moderation_log row for action {action:?}"
        );
    }
}
