mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use savez::routes::puzzles::{Bounds, Pos, PuzzleGameBuilding, PuzzleGameData};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt; // for `oneshot`

/// Proves `PuzzleGameData` (and its nested types) serialize with exactly the field names/casing
/// of `savegame_typedefs.js`'s `PuzzleGameData` typedef, and that the JSON round-trips losslessly
/// through Deserialize -> Serialize.
#[test]
fn puzzle_game_data_field_names_match_typedefs() {
    let data = PuzzleGameData {
        version: 1,
        bounds: Bounds { w: 10, h: 8 },
        buildings: vec![
            PuzzleGameBuilding::Emitter {
                item: "CuCuCuCu".to_string(),
                pos: Pos { x: 0, y: 0, r: 0 },
            },
            PuzzleGameBuilding::Goal {
                item: "CuCuCuCu".to_string(),
                pos: Pos { x: 4, y: 3, r: 90 },
            },
            PuzzleGameBuilding::Block {
                pos: Pos { x: 2, y: 2, r: 180 },
            },
        ],
        excluded_buildings: vec!["CutterMirrored".to_string()],
    };

    let value = serde_json::to_value(&data).expect("serialize PuzzleGameData");

    assert!(value.get("version").is_some());
    assert!(value.get("bounds").is_some());
    assert_eq!(value["bounds"]["w"], 10);
    assert_eq!(value["bounds"]["h"], 8);
    assert!(value.get("buildings").is_some());
    assert!(
        value.get("excludedBuildings").is_some(),
        "expected literal camelCase key `excludedBuildings`, got: {value}"
    );

    let buildings = value["buildings"].as_array().expect("buildings array");
    assert_eq!(buildings.len(), 3);

    let emitter = &buildings[0];
    assert_eq!(emitter["type"], "emitter");
    assert_eq!(emitter["item"], "CuCuCuCu");
    assert!(emitter.get("pos").is_some());
    assert_eq!(emitter["pos"]["x"], 0);
    assert_eq!(emitter["pos"]["y"], 0);
    assert_eq!(emitter["pos"]["r"], 0);

    let goal = &buildings[1];
    assert_eq!(goal["type"], "goal");
    assert_eq!(goal["item"], "CuCuCuCu");

    let block = &buildings[2];
    assert_eq!(block["type"], "block");
    assert!(
        block.get("item").is_none(),
        "block variant must not serialize an `item` key, got: {block}"
    );
    assert!(block.get("pos").is_some());

    // Round-trip: Deserialize the produced JSON back into PuzzleGameData, re-serialize, and
    // compare the two `Value`s for equality — proves no information is lost either direction.
    let round_tripped: PuzzleGameData =
        serde_json::from_value(value.clone()).expect("deserialize PuzzleGameData");
    let round_tripped_value = serde_json::to_value(&round_tripped).expect("re-serialize");
    assert_eq!(value, round_tripped_value);
}

/// `#[sqlx::test]` creates a fresh, throwaway Postgres database and applies every migration in
/// `migrations/` to it before this test body runs — proving the migrations produce the full
/// 6-table SPEC §4.3 schema plus the D-02 seed author row (see the assertion below for its exact
/// `name`/`verified_via` values), without any manual setup here.
#[sqlx::test]
async fn migrations_create_all_tables(pool: PgPool) {
    let expected_tables = [
        "users",
        "puzzles",
        "puzzle_completions",
        "puzzle_reports",
        "user_bans",
        "moderation_log",
    ];

    for table in expected_tables {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM information_schema.tables
                WHERE table_schema = 'public' AND table_name = $1
            )",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap_or_else(|err| panic!("querying information_schema for {table} failed: {err}"));
        assert!(exists, "expected table `{table}` to exist in public schema");
    }

    // Phase 5 (D-08): the Phase 3 temporary submission-author row (verified_via='dev-seed') is
    // retired on every fresh database — JWT auth now supplies a real author_id for every
    // submission, so the mechanism is gone (see migrations/20260808000002_retire_dev_seed_author.sql).
    let retired_seed_author_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM users WHERE id = '00000000-0000-0000-0000-000000000001'",
    )
    .fetch_one(&pool)
    .await
    .expect("count query must succeed");
    assert_eq!(
        retired_seed_author_count, 0,
        "the retired Phase 3 seed-author row must not exist on a fresh database"
    );

    // D-08: the first administrator exists and was only ever created by this seed migration.
    let admin_row: (String, String, String) = sqlx::query_as(
        "SELECT name, verified_via, role FROM users WHERE id = '00000000-0000-0000-0000-000000000002'",
    )
    .fetch_one(&pool)
    .await
    .expect("seed admin row must exist after migrations run");
    assert_eq!(
        admin_row,
        (
            "admin".to_string(),
            "seed-admin".to_string(),
            "admin".to_string()
        )
    );
}

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

/// D-17: every business/auth rejection answers HTTP 200 with `{ "error": "<code>" }` — this
/// helper asserts both halves of that contract in one call.
async fn assert_error_code(response: axum::response::Response, expected_code: &str) {
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_to_json(response).await;
    assert_eq!(body, json!({ "error": expected_code }));
}

/// T-03-21/T-05-05 mitigation proof: a submission body carrying client-supplied `author`/
/// `authorId` fields is accepted, but the row that lands in the database is always attributed to
/// the JWT-authenticated user — never to the values the client sent.
#[sqlx::test]
async fn submit_persists_puzzle(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "test-author").await;
    let token = common::jwt_for(author_id);

    let body = json!({
        "title": "Test Puzzle",
        "shortKey": "CuCuCuCu",
        "data": sample_game_data().to_string(),
        "author": "attaquant",
        "authorId": "11111111-1111-1111-1111-111111111111",
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/puzzles/submit")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-token", &token)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let row: (String, String) =
        sqlx::query_as("SELECT title, author_id::text FROM puzzles WHERE short_key = $1")
            .bind("CuCuCuCu")
            .fetch_one(&pool)
            .await
            .expect("submitted puzzle row must exist");
    assert_eq!(row.0, "Test Puzzle");
    assert_eq!(row.1, author_id.to_string());
}

/// A puzzle downloads identically by numeric `id` and by `shortKey`, in the `{ meta, game }`
/// shape, and an unknown id resolves to 404.
#[sqlx::test]
async fn download_by_id_and_by_short_key(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "test-author").await;
    let token = common::jwt_for(author_id);

    let submit_body = json!({
        "title": "Download Test Puzzle",
        "shortKey": "RuRuRuRu",
        "data": sample_game_data().to_string(),
    });

    let submit_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/puzzles/submit")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-token", &token)
                .body(Body::from(submit_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(submit_response.status(), StatusCode::OK);
    let submitted_meta = body_to_json(submit_response).await;
    let id = submitted_meta["id"]
        .as_u64()
        .expect("submitted id is a number");

    let by_id_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/puzzles/download/{id}"))
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(by_id_response.status(), StatusCode::OK);
    let by_id_body = body_to_json(by_id_response).await;

    let by_short_key_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/download/RuRuRuRu")
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(by_short_key_response.status(), StatusCode::OK);
    let by_short_key_body = body_to_json(by_short_key_response).await;

    assert_eq!(by_id_body, by_short_key_body);
    assert!(by_id_body.get("meta").is_some());
    assert!(by_id_body.get("game").is_some());
    assert_eq!(by_id_body["game"], sample_game_data());

    let not_found_response = app
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/download/999999")
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_error_code(not_found_response, "not-found").await;
}

/// Regression test (code review CR-01): a puzzle whose `short_key` happens to look numeric must
/// still resolve to itself, not to a different puzzle that happens to share that numeric `id`.
/// `short_key` lookup must take priority over a coincidental numeric parse of `id_or_key`.
///
/// The shape-key grammar (D-14) admits no digits, so a numeric `short_key` can no longer reach the
/// database through `submit` once Phase 6's validation lands. The regression this test guards
/// against lives in `download`'s resolution order, not in `submit`, so the second row is inserted
/// directly, bypassing the API and its validation entirely.
#[sqlx::test]
async fn download_resolves_numeric_short_key_over_coincidental_id(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "test-author").await;
    let token = common::jwt_for(author_id);

    // First puzzle: whatever numeric `id` the DB assigns it (e.g. 1).
    let decoy = submit_puzzle(app.clone(), &token, "SuSuSuSu", "Decoy").await;
    let decoy_id = decoy["id"].as_u64().expect("decoy id is a number");

    // Second puzzle: its `short_key` is literally the decoy's numeric id as a string. Inserted
    // directly via `query_scalar` (not the `query!` macro, matching `common::register_test_user`)
    // so this test-only write does not grow the versioned `.sqlx` offline cache.
    let numeric_key = decoy_id.to_string();
    let target_id: i32 = sqlx::query_scalar(
        "INSERT INTO puzzles (short_key, title, author_id, data) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(&numeric_key)
    .bind("Numeric Key Puzzle")
    .bind(author_id)
    .bind(sqlx::types::Json(sample_game_data()))
    .fetch_one(&pool)
    .await
    .expect("direct insert of numeric-short_key puzzle must succeed");
    assert_ne!(
        decoy_id as i32, target_id,
        "test setup requires two distinct puzzle ids"
    );

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/puzzles/download/{numeric_key}"))
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_to_json(response).await;

    assert_eq!(
        body["meta"]["id"].as_u64(),
        Some(target_id as u64),
        "download/{numeric_key} must resolve by short_key match, not by coincidentally parsing \
         as the decoy puzzle's numeric id"
    );
    assert_eq!(body["meta"]["shortKey"], numeric_key);
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

/// D-04: `new` is the only real category. Two puzzles submitted successively must both come back,
/// most-recently-submitted first (`created_at DESC, id DESC`), each attributed to the
/// JWT-authenticated `test-author` user.
#[sqlx::test]
async fn list_new_returns_submitted_puzzles_newest_first(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "test-author").await;
    let token = common::jwt_for(author_id);

    submit_puzzle(app.clone(), &token, "CrCrCrCr", "First Puzzle").await;
    submit_puzzle(app.clone(), &token, "CgCgCgCg", "Second Puzzle").await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/list/new")
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_to_json(response).await;
    let list = body.as_array().expect("list/new returns a JSON array");
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["shortKey"], "CgCgCgCg");
    assert_eq!(list[1]["shortKey"], "CrCrCrCr");
    assert_eq!(list[0]["author"], "test-author");
    assert_eq!(list[1]["author"], "test-author");
}

/// D-05: `top-rated` and `mine` always answer 200 with an empty array, never an error and never a
/// copy of `new`'s content, even once puzzles exist.
#[sqlx::test]
async fn list_top_rated_and_mine_return_empty(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "test-author").await;
    let token = common::jwt_for(author_id);

    submit_puzzle(app.clone(), &token, "CbCbCbCb", "Some Puzzle").await;

    for category in ["top-rated", "mine"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/puzzles/list/{category}"))
                    .header("x-token", &token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "category={category}");
        let body = body_to_json(response).await;
        assert_eq!(
            body,
            json!([]),
            "category={category} must return an empty array"
        );
    }
}

/// D-06/D-07: downloading a puzzle never increments `downloads` and never flips `completed`.
#[sqlx::test]
async fn download_does_not_increment_counter(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "test-author").await;
    let token = common::jwt_for(author_id);

    submit_puzzle(app.clone(), &token, "CyCyCyCy", "Counter Puzzle").await;

    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/puzzles/download/CyCyCyCy")
                    .header("x-token", &token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/list/new")
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_to_json(response).await;
    let list = body.as_array().expect("list/new returns a JSON array");
    let entry = list
        .iter()
        .find(|p| p["shortKey"] == "CyCyCyCy")
        .expect("submitted puzzle present in list/new");
    assert_eq!(entry["downloads"], 0);
    assert_eq!(entry["completed"], false);

    let downloads: i32 = sqlx::query_scalar("SELECT downloads FROM puzzles WHERE short_key = $1")
        .bind("CyCyCyCy")
        .fetch_one(&pool)
        .await
        .expect("puzzle row must exist");
    assert_eq!(downloads, 0);
}

/// D-04/D-05: a `data` value that is neither valid JSON (Community Edition format) nor a valid
/// lz-string (official `compressX64` format) is rejected with `bad-payload` and creates no row.
#[sqlx::test]
async fn submit_rejects_undecodable_payload(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "test-author").await;
    let token = common::jwt_for(author_id);

    let body = json!({
        "title": "Undecodable Puzzle",
        "shortKey": "CpCpCpCp",
        "data": "not-valid-lzstring-!!@@##",
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/puzzles/submit")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-token", &token)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_error_code(response, "bad-payload").await;

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM puzzles WHERE short_key = $1")
        .bind("CpCpCpCp")
        .fetch_one(&pool)
        .await
        .expect("count query must succeed");
    assert_eq!(
        count, 0,
        "no puzzle row must be created for an undecodable payload"
    );
}

/// Pitfall 1 lock-in: Postgres has no unsigned integer type, so `likes`/`downloads`/`completions`
/// are stored `i32` and cast to `u32` on the way out. This proves non-zero values round-trip
/// intact through that cast rather than silently wrapping or truncating.
#[sqlx::test]
async fn counters_round_trip_as_u32(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "test-author").await;
    let token = common::jwt_for(author_id);

    submit_puzzle(app.clone(), &token, "CcCcCcCc", "Roundtrip Puzzle").await;

    sqlx::query(
        "UPDATE puzzles SET likes = $1, downloads = $2, completions = $3 WHERE short_key = $4",
    )
    .bind(42_i32)
    .bind(7_i32)
    .bind(3_i32)
    .bind("CcCcCcCc")
    .execute(&pool)
    .await
    .expect("counter update must succeed");

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/list/new")
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_to_json(response).await;
    let list = body.as_array().expect("list/new returns a JSON array");
    let entry = list
        .iter()
        .find(|p| p["shortKey"] == "CcCcCcCc")
        .expect("submitted puzzle present in list/new");
    assert_eq!(entry["likes"], 42);
    assert_eq!(entry["downloads"], 7);
    assert_eq!(entry["completions"], 3);
}

/// ROADMAP SC3 / D-14/D-15/D-16/D-17: table-driven proof that every submission-rejection rule
/// this plan implements answers its own exact `T.backendErrors` code and, per SC3's "no rejected
/// puzzle leaves a row" guarantee, creates no row. Each case starts from the canonical submission
/// (a valid title, a distinct valid `shortKey`, `sample_game_data()`) and mutates exactly one
/// aspect, so a failing assertion is never ambiguous about which rule broke.
#[sqlx::test]
async fn submit_rejects_invalid_puzzles(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "rejects-author").await;
    let token = common::jwt_for(author_id);

    let long_title = "a".repeat(21);

    let mut no_emitters_data = sample_game_data();
    no_emitters_data["buildings"] =
        json!([{ "type": "goal", "item": "CuCuCuCu", "pos": { "x": 4, "y": 3, "r": 90 } }]);

    let mut no_goals_data = sample_game_data();
    no_goals_data["buildings"] =
        json!([{ "type": "emitter", "item": "CuCuCuCu", "pos": { "x": 0, "y": 0, "r": 0 } }]);

    let mut bad_emitter_item_data = sample_game_data();
    bad_emitter_item_data["buildings"][0]["item"] = json!("nope");

    let mut bad_goal_item_data = sample_game_data();
    bad_goal_item_data["buildings"][1]["item"] = json!("nope");

    let mut bad_placement_data = sample_game_data();
    bad_placement_data["buildings"][1]["pos"] = json!({ "x": 5, "y": 5, "r": 90 });

    let cases: Vec<(&str, String, &str, Value, &str)> = vec![
        (
            "title too short (3 chars)",
            "abc".to_string(),
            "RrRrRrRr",
            sample_game_data(),
            "bad-title-too-many-spaces",
        ),
        (
            "title too long (21 chars)",
            long_title,
            "CgCgCgCg",
            sample_game_data(),
            "bad-title-too-many-spaces",
        ),
        (
            "profane title",
            "FUCK Puzzle".to_string(),
            "SbSbSbSb",
            sample_game_data(),
            "profane-title",
        ),
        (
            "malformed shortKey",
            "Malformed Key".to_string(),
            "shape:CuCuCuCu",
            sample_game_data(),
            "bad-short-key",
        ),
        (
            "no emitters",
            "No Emitters".to_string(),
            "WyWyWyWy",
            no_emitters_data,
            "no-emitters",
        ),
        (
            "no goals",
            "No Goals".to_string(),
            "RpRpRpRp",
            no_goals_data,
            "no-goals",
        ),
        (
            "bad emitter item",
            "Bad Emitter".to_string(),
            "CcCcCcCc",
            bad_emitter_item_data,
            "bad-shape-key-in-emitter",
        ),
        (
            "bad goal item",
            "Bad Goal".to_string(),
            "SwSwSwSw",
            bad_goal_item_data,
            "bad-shape-key-in-goal",
        ),
        (
            "bad placement",
            "Bad Placement".to_string(),
            "WuWuWuWu",
            bad_placement_data,
            "bad-building-placement",
        ),
    ];

    for (label, title, short_key, data, expected_code) in cases {
        let body = json!({
            "title": title,
            "shortKey": short_key,
            "data": data.to_string(),
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/puzzles/submit")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-token", &token)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "case {label}: expected HTTP 200"
        );
        let response_body = body_to_json(response).await;
        assert_eq!(
            response_body,
            json!({ "error": expected_code }),
            "case {label}: unexpected error code"
        );

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM puzzles WHERE short_key = $1")
            .bind(short_key)
            .fetch_one(&pool)
            .await
            .expect("count query must succeed");
        assert_eq!(
            count, 0,
            "case {label}: a rejected puzzle must leave no row"
        );
    }
}

/// Locks in the distinction between `bad-short-key` (malformed, above) and
/// `short-key-already-taken` (well-formed but a duplicate): the first submission of a well-formed
/// `shortKey` succeeds, the second answers `short-key-already-taken`, and exactly one row exists.
#[sqlx::test]
async fn submit_rejects_duplicate_short_key(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "dup-author").await;
    let token = common::jwt_for(author_id);

    let body = json!({
        "title": "Duplicate Key Puzzle",
        "shortKey": "CuCuCuCu",
        "data": sample_game_data().to_string(),
    });

    let first_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/puzzles/submit")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-token", &token)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    let first_body = body_to_json(first_response).await;
    assert!(
        first_body.get("id").is_some(),
        "first submission must return a PuzzleMetadata, got: {first_body}"
    );

    let second_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/puzzles/submit")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-token", &token)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_error_code(second_response, "short-key-already-taken").await;

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM puzzles WHERE short_key = $1")
        .bind("CuCuCuCu")
        .fetch_one(&pool)
        .await
        .expect("count query must succeed");
    assert_eq!(
        count, 1,
        "exactly one row must exist after the duplicate is rejected"
    );
}

/// D-17/06-VALIDATION.md: sweeps the four error paths visible at this stage of the phase in one
/// test, proving the all-200 contract holds across handler categories -- not-found (repository),
/// bad-payload (decode), unauthorized (missing token), bad-token (invalid token). None of the
/// four responses may have a status other than 200.
#[sqlx::test]
async fn error_taxonomy(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "taxonomy-author").await;
    let token = common::jwt_for(author_id);

    let not_found_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/download/999999")
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_error_code(not_found_response, "not-found").await;

    let bad_payload_body = json!({
        "title": "Taxonomy Undecodable",
        "shortKey": "TxTxTxTx",
        "data": "not-valid-lzstring-!!@@##",
    });
    let bad_payload_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/puzzles/submit")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-token", &token)
                .body(Body::from(bad_payload_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_error_code(bad_payload_response, "bad-payload").await;

    let unauthorized_body = json!({
        "title": "Taxonomy No Token",
        "shortKey": "TyTyTyTy",
        "data": sample_game_data().to_string(),
    });
    let unauthorized_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/puzzles/submit")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(unauthorized_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_error_code(unauthorized_response, "unauthorized").await;

    let bad_token_body = json!({
        "title": "Taxonomy Bad Token",
        "shortKey": "TzTzTzTz",
        "data": sample_game_data().to_string(),
    });
    let bad_token_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/puzzles/submit")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-token", "not-a-jwt")
                .body(Body::from(bad_token_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_error_code(bad_token_response, "bad-token").await;
}
