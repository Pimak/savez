//! 07-08-PLAN.md: proves the profanity list lives in the database (not a hardcoded constant),
//! that it is enforced through `POST /v1/puzzles/submit` via `AppState.profanity_cache`, that a
//! runtime change (`profanity::add_word`/`remove_word`) becomes visible without a server restart
//! once `TEST_PROFANITY_CACHE_TTL` elapses, that the filter stays token-exact (never a substring
//! match, the "Scunthorpe problem"), and that every list modification is journaled.
//!
//! Selectable with `cargo test --all-targets profane` (the "profane" substring matches this
//! binary's own name and every test function name below).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use savez::error::AppError;
use savez::profanity;
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

/// Submits a puzzle with the given `title`/`short_key` and returns the parsed JSON body,
/// regardless of whether the submission succeeded (metadata) or was rejected (`{"error": ...}`) —
/// callers inspect the body themselves, since both outcomes are exercised by this file.
async fn submit_titled_puzzle(
    app: axum::Router,
    token: &str,
    short_key: &str,
    title: &str,
) -> Value {
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

/// Sleeps well past `common::TEST_PROFANITY_CACHE_TTL` (1ms) so the NEXT `AppState.profanity_cache
/// .get()` call is guaranteed to be a cache miss and re-read the database — proving a runtime
/// change becomes visible without any server restart.
async fn wait_past_test_ttl() {
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
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

// --- Enforcement through the real HTTP route ---------------------------------------------------

#[sqlx::test]
async fn seeded_word_rejects_title(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "profanity-seeded-author").await;
    let token = common::jwt_for(author_id);

    let body = submit_titled_puzzle(app, &token, "CuCuCuCu", "Fuck This Puzzle").await;
    assert_eq!(body, json!({ "error": "profane-title" }));
}

#[sqlx::test]
async fn word_added_at_runtime_rejects_title(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state.clone());

    let author_id = common::register_test_user(&pool, "profanity-added-author").await;
    let token = common::jwt_for(author_id);
    let moderator_id =
        common::register_test_user_with_role(&pool, "profanity-added-moderator", "moderator").await;

    // "zzztest" is not part of the seed -- accepted first.
    let first = submit_titled_puzzle(app.clone(), &token, "RrRrRrRr", "Zzztest Puzzle").await;
    assert!(
        first.get("id").is_some(),
        "a word absent from the seed must be accepted before it is added: {first:?}"
    );

    profanity::add_word(&pool, "zzztest", "en", moderator_id)
        .await
        .expect("add_word must succeed");
    wait_past_test_ttl().await;

    let second = submit_titled_puzzle(app, &token, "SwSwSwSw", "Zzztest Again").await;
    assert_eq!(
        second,
        json!({ "error": "profane-title" }),
        "a word added at runtime must be enforced without a server restart"
    );
}

#[sqlx::test]
async fn word_removed_at_runtime_is_accepted(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state.clone());

    let author_id = common::register_test_user(&pool, "profanity-removed-author").await;
    let token = common::jwt_for(author_id);
    let moderator_id =
        common::register_test_user_with_role(&pool, "profanity-removed-moderator", "moderator")
            .await;

    // "moron" is part of the seed -- rejected first.
    let first = submit_titled_puzzle(app.clone(), &token, "RrRrRrRr", "Moron Puzzle").await;
    assert_eq!(first, json!({ "error": "profane-title" }));

    profanity::remove_word(&pool, "moron", moderator_id)
        .await
        .expect("remove_word must succeed");
    wait_past_test_ttl().await;

    let second = submit_titled_puzzle(app, &token, "SwSwSwSw", "Moron Again").await;
    assert!(
        second.get("id").is_some(),
        "a word removed at runtime must no longer be enforced, without a server restart: {second:?}"
    );
}

#[sqlx::test]
async fn substring_does_not_trigger_filter(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "profanity-substring-author").await;
    let token = common::jwt_for(author_id);

    // "cunt" is part of the seed, but "Scunthorpe" containing it as a substring must not match a
    // token-exact filter -- the canonical Scunthorpe-problem regression.
    let body = submit_titled_puzzle(app, &token, "CuCuCuCu", "Scunthorpe Puzzle").await;
    assert!(
        body.get("id").is_some(),
        "a legitimate word containing a forbidden word as a substring must be accepted: {body:?}"
    );
    assert!(body.get("error").is_none());
}

// --- Audit journal and input validation (direct repository/profanity calls) --------------------

#[sqlx::test]
async fn profanity_change_is_logged(pool: PgPool) {
    let moderator_id =
        common::register_test_user_with_role(&pool, "profanity-log-moderator", "moderator").await;

    profanity::add_word(&pool, "zzzlogword", "en", moderator_id)
        .await
        .expect("add_word must succeed");
    profanity::remove_word(&pool, "zzzlogword", moderator_id)
        .await
        .expect("remove_word must succeed");

    let logged = moderation_log_count(
        &pool,
        "zzzlogword",
        repository::moderation_action::PROFANITY_UPDATE,
    )
    .await;
    assert_eq!(
        logged, 2,
        "add_word and remove_word must each append their own moderation_log row"
    );
}

#[sqlx::test]
async fn add_word_rejects_bad_input(pool: PgPool) {
    let moderator_id =
        common::register_test_user_with_role(&pool, "profanity-bad-input-moderator", "moderator")
            .await;

    let empty = profanity::add_word(&pool, "", "en", moderator_id).await;
    assert!(matches!(empty, Err(AppError::BadPayload)));

    let spaced = profanity::add_word(&pool, "bad word", "en", moderator_id).await;
    assert!(matches!(spaced, Err(AppError::BadPayload)));

    let bad_lang = profanity::add_word(&pool, "zzzbadlang", "de", moderator_id).await;
    assert!(matches!(bad_lang, Err(AppError::BadPayload)));
}

#[sqlx::test]
async fn add_existing_word_is_a_noop(pool: PgPool) {
    let moderator_id =
        common::register_test_user_with_role(&pool, "profanity-existing-moderator", "moderator")
            .await;

    // "fuck" is already part of the seed.
    let inserted = profanity::add_word(&pool, "fuck", "en", moderator_id)
        .await
        .expect("add_word must succeed even as a no-op");
    assert!(
        !inserted,
        "adding an already-present word must return false"
    );

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM profanity_words WHERE word = 'fuck'")
        .fetch_one(&pool)
        .await
        .expect("count fuck rows");
    assert_eq!(count, 1, "a no-op add must never create a duplicate row");
}
