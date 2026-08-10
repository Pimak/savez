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

/// A puzzle resolves identically by numeric `id` and by `shortKey`, in the `{ meta, game }` shape,
/// and an unknown id resolves to 404. `game` and every `meta` field EXCEPT `downloads`/`difficulty`
/// must match byte-for-byte across the two lookups -- `downloads`/`difficulty` deliberately differ
/// (D-19: each successful download increments the counter by exactly 1, so the second lookup's
/// `downloads` is one higher than the first's).
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

    assert_eq!(by_id_body["game"], by_short_key_body["game"]);
    assert_eq!(by_id_body["meta"]["id"], by_short_key_body["meta"]["id"]);
    assert_eq!(
        by_id_body["meta"]["shortKey"],
        by_short_key_body["meta"]["shortKey"]
    );
    assert_eq!(
        by_id_body["meta"]["title"],
        by_short_key_body["meta"]["title"]
    );
    assert_eq!(
        by_id_body["meta"]["author"],
        by_short_key_body["meta"]["author"]
    );
    assert_eq!(
        by_id_body["meta"]["completed"],
        by_short_key_body["meta"]["completed"]
    );
    // D-19: each successful download increments the counter -- the second lookup (by short key)
    // must show exactly one more download than the first (by id), not an equal or unrelated value.
    assert_eq!(by_id_body["meta"]["downloads"], 1);
    assert_eq!(by_short_key_body["meta"]["downloads"], 2);
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

/// REQ-business-logic/ROADMAP SC1: `top-rated` ranks by likes descending, ties broken by
/// completions descending. Three puzzles are set up with deliberately non-monotonic likes/
/// completions pairs -- (2 likes, 2 completions), (1 like, 5 completions), (1 like, 2 completions)
/// -- so a naive "sort by completions" or "sort by submission order" would both produce a
/// different, wrong order than the likes-then-completions rule this test proves.
#[sqlx::test]
async fn list_top_rated_orders_by_likes_then_completions(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "top-rated-author").await;
    let token = common::jwt_for(author_id);

    // (short_key, [(liker_name, liked)]) -- one completion per entry.
    let high_likes = submit_puzzle(app.clone(), &token, "SgSgSgSg", "High Likes").await;
    let high_likes_id = high_likes["id"].as_i64().expect("id is a number");
    for (name, liked) in [
        ("tr-high-1", true),
        ("tr-high-2", true),
    ] {
        let user_id = common::register_test_user(&pool, name).await;
        let user_token = common::jwt_for(user_id);
        let call =
            complete_request(app.clone(), Some(&user_token), &high_likes_id.to_string(), 10.0, liked)
                .await;
        assert_eq!(call.status(), StatusCode::OK);
    }

    let high_completions = submit_puzzle(app.clone(), &token, "RcRcRcRc", "High Completions").await;
    let high_completions_id = high_completions["id"].as_i64().expect("id is a number");
    for (i, liked) in [true, false, false, false, false].into_iter().enumerate() {
        let user_id = common::register_test_user(&pool, &format!("tr-mid-{i}")).await;
        let user_token = common::jwt_for(user_id);
        let call = complete_request(
            app.clone(),
            Some(&user_token),
            &high_completions_id.to_string(),
            10.0,
            liked,
        )
        .await;
        assert_eq!(call.status(), StatusCode::OK);
    }

    let low = submit_puzzle(app.clone(), &token, "SbSbSbSb", "Low Everything").await;
    let low_id = low["id"].as_i64().expect("id is a number");
    for (i, liked) in [true, false].into_iter().enumerate() {
        let user_id = common::register_test_user(&pool, &format!("tr-low-{i}")).await;
        let user_token = common::jwt_for(user_id);
        let call =
            complete_request(app.clone(), Some(&user_token), &low_id.to_string(), 10.0, liked)
                .await;
        assert_eq!(call.status(), StatusCode::OK);
    }

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/list/top-rated")
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_to_json(response).await;
    let list = body.as_array().expect("top-rated returns a JSON array");
    let keys: Vec<&str> = list
        .iter()
        .map(|p| p["shortKey"].as_str().expect("shortKey is a string"))
        .collect();
    assert_eq!(
        keys,
        vec!["SgSgSgSg", "RcRcRcRc", "SbSbSbSb"],
        "expected likes-descending order, completions as the tie-break"
    );
}

/// REQ-business-logic/ROADMAP SC1: two puzzles tied at 0 likes and 0 completions must still come
/// back in a deterministic order -- the trailing `id DESC` tie-break, not submission order.
#[sqlx::test]
async fn list_top_rated_tie_breaks_on_id_desc(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "tie-break-author").await;
    let token = common::jwt_for(author_id);

    let first = submit_puzzle(app.clone(), &token, "WrWrWrWr", "Tie First").await;
    let first_id = first["id"].as_i64().expect("id is a number");
    let second = submit_puzzle(app.clone(), &token, "WgWgWgWg", "Tie Second").await;
    let second_id = second["id"].as_i64().expect("id is a number");
    assert!(
        second_id > first_id,
        "test setup requires the second submission to have the higher id"
    );

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/list/top-rated")
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_to_json(response).await;
    let list = body.as_array().expect("top-rated returns a JSON array");
    let keys: Vec<&str> = list
        .iter()
        .map(|p| p["shortKey"].as_str().expect("shortKey is a string"))
        .collect();
    assert_eq!(
        keys,
        vec!["WgWgWgWg", "WrWrWrWr"],
        "puzzles tied on likes and completions must come back id-descending"
    );
}

/// D-10/docs/adr/0002-hidden-by-tri-state.md: `top-rated` uses the same visibility predicate as
/// `list/new` -- a puzzle hidden by its own author (`POST /v1/puzzles/delete/:id`) must disappear
/// from `top-rated` for EVERYONE, including that author, unlike `list/mine`.
#[sqlx::test]
async fn list_top_rated_excludes_hidden_puzzles(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "hidden-top-rated-author").await;
    let token = common::jwt_for(author_id);

    let submitted = submit_puzzle(app.clone(), &token, "WbWbWbWb", "Hidden Top Rated").await;
    let puzzle_id = submitted["id"].as_i64().expect("id is a number");

    let delete_response = delete_request(app.clone(), Some(&token), &puzzle_id.to_string()).await;
    assert_eq!(delete_response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/list/top-rated")
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_to_json(response).await;
    let list = body.as_array().expect("top-rated returns a JSON array");
    assert!(
        !list.iter().any(|p| p["shortKey"] == "WbWbWbWb"),
        "top-rated must never show a puzzle hidden by its own author, even to that author"
    );
}

/// D-19: two successive downloads of the same puzzle return `downloads` = 1 then 2, and the stored
/// row matches 2 afterward -- proves the counter increments by exactly 1 per successful download,
/// evaluated atomically by PostgreSQL.
#[sqlx::test]
async fn download_increments_downloads_counter(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "downloads-author").await;
    let token = common::jwt_for(author_id);

    submit_puzzle(app.clone(), &token, "CyCyCyCy", "Counter Puzzle").await;

    let first_response = app
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
    assert_eq!(first_response.status(), StatusCode::OK);
    let first_body = body_to_json(first_response).await;
    assert_eq!(first_body["meta"]["downloads"], 1);

    let second_response = app
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/download/CyCyCyCy")
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    let second_body = body_to_json(second_response).await;
    assert_eq!(second_body["meta"]["downloads"], 2);

    let downloads: i32 = sqlx::query_scalar("SELECT downloads FROM puzzles WHERE short_key = $1")
        .bind("CyCyCyCy")
        .fetch_one(&pool)
        .await
        .expect("puzzle row must exist");
    assert_eq!(downloads, 2);
}

/// D-19: a `download` on a nonexistent id resolves to `not-found` and leaves every existing
/// puzzle's `downloads` untouched -- resolution failure must return before the counter is ever
/// touched.
#[sqlx::test]
async fn failed_download_does_not_increment(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "failed-download-author").await;
    let token = common::jwt_for(author_id);

    submit_puzzle(app.clone(), &token, "CgCgCgCg", "Untouched Puzzle").await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/download/999999")
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_error_code(response, "not-found").await;

    let downloads: i32 = sqlx::query_scalar("SELECT downloads FROM puzzles WHERE short_key = $1")
        .bind("CgCgCgCg")
        .fetch_one(&pool)
        .await
        .expect("puzzle row must exist");
    assert_eq!(
        downloads, 0,
        "a failed download must never increment an unrelated puzzle's counter"
    );
}

/// D-19: `submit` reads back the puzzle it just created via `find_puzzle_by_id` directly, never
/// through the `download` handler -- the freshly created row must show `downloads = 0`, not 1.
#[sqlx::test]
async fn submit_does_not_increment_downloads(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "submit-no-increment-author").await;
    let token = common::jwt_for(author_id);

    let submitted = submit_puzzle(app.clone(), &token, "CbCbCbCb", "No Phantom Download").await;
    assert_eq!(submitted["downloads"], 0);

    let downloads: i32 = sqlx::query_scalar("SELECT downloads FROM puzzles WHERE short_key = $1")
        .bind("CbCbCbCb")
        .fetch_one(&pool)
        .await
        .expect("puzzle row must exist");
    assert_eq!(
        downloads, 0,
        "submit must never count as a download of the puzzle it just created"
    );
}

/// D-14/D-19: a puzzle downloaded twice then completed once shows `difficulty` ~= 0.5 on the NEXT
/// read (`completions / downloads` = 1 / 2) -- proves `downloads` actually feeds `difficulty` once
/// it stops being permanently 0. Uses a tolerant float comparison, never a strict `==` on `f32`.
#[sqlx::test]
async fn difficulty_appears_after_downloads_and_completions(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "difficulty-appears-author").await;
    let token = common::jwt_for(author_id);

    let submitted = submit_puzzle(app.clone(), &token, "SySySySy", "Difficulty Appears").await;
    let puzzle_id = submitted["id"].as_i64().expect("id is a number");

    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/puzzles/download/{puzzle_id}"))
                    .header("x-token", &token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    let completion = complete_request(
        app.clone(),
        Some(&token),
        &puzzle_id.to_string(),
        15.0,
        false,
    )
    .await;
    assert_eq!(completion.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/puzzles/download/{puzzle_id}"))
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_to_json(response).await;
    let difficulty = body["meta"]["difficulty"]
        .as_f64()
        .expect("difficulty must be present once downloads > 0");
    assert!(
        (difficulty - 0.5).abs() < 0.01,
        "expected difficulty ~= 0.5 (1 completion / 2 downloads), got {difficulty}"
    );
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

/// Pitfall 1 lock-in, updated for D-15/ADR 0005: Postgres has no unsigned integer type, so every
/// counter is cast to `u32` on the way out, but the source differs per counter now. `downloads`
/// (D-19) is still a stored `i32` column, updated directly here exactly as before. `likes`/
/// `completions` are no longer stored columns to `UPDATE` -- they are `COUNT(...)` aggregates
/// (`i64` at the SQL boundary) computed live from `puzzle_completions`, produced here by three
/// distinct users completing the puzzle, two of them liking it. All three counters must still
/// round-trip through their respective casts without wrapping or truncating.
#[sqlx::test]
async fn counters_round_trip_as_u32(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "test-author").await;
    let token = common::jwt_for(author_id);

    submit_puzzle(app.clone(), &token, "CcCcCcCc", "Roundtrip Puzzle").await;

    let response = app
        .clone()
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
    let puzzle_id = entry["id"].as_i64().expect("puzzle id is a number");

    sqlx::query("UPDATE puzzles SET downloads = $1 WHERE short_key = $2")
        .bind(7_i32)
        .bind("CcCcCcCc")
        .execute(&pool)
        .await
        .expect("downloads update must succeed");

    for (name, liked) in [
        ("roundtrip-liker-1", true),
        ("roundtrip-liker-2", true),
        ("roundtrip-liker-3", false),
    ] {
        let user_id = common::register_test_user(&pool, name).await;
        let user_token = common::jwt_for(user_id);
        let call = complete_request(
            app.clone(),
            Some(&user_token),
            &puzzle_id.to_string(),
            10.0,
            liked,
        )
        .await;
        assert_eq!(call.status(), StatusCode::OK);
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
        .find(|p| p["shortKey"] == "CcCcCcCc")
        .expect("submitted puzzle present in list/new");
    assert_eq!(entry["likes"], 2);
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

/// Issues a search request against `POST /v1/puzzles/search`, always with a full body (relying on
/// the handler's `#[serde(default ...)]` behavior is exercised separately, not here).
async fn search_request(
    app: axum::Router,
    token: Option<&str>,
    search_term: &str,
    difficulty: &str,
    duration: &str,
) -> axum::response::Response {
    let body = json!({
        "searchTerm": search_term,
        "difficulty": difficulty,
        "duration": duration,
    });
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/puzzles/search")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        builder = builder.header("x-token", token);
    }
    app.oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

/// D-05/D-06 + T-06-22: `list/new`, `list/mine`, `download` and `search` all require a valid
/// `x-token` — no header at all answers `unauthorized`, a syntactically-invalid token answers
/// `bad-token`. Every one of the eight responses below must be HTTP 200 (D-17: rejection is a
/// business error, never a bare 401).
#[sqlx::test]
async fn list_search_download_require_auth(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "auth-gate-author").await;
    let token = common::jwt_for(author_id);
    submit_puzzle(app.clone(), &token, "RgRgRgRg", "Auth Gate Puzzle").await;

    // (missing header, expected code)
    for (variant_name, header_token) in [("missing", None), ("garbage", Some("not-a-jwt"))] {
        let expected_code = if header_token.is_none() {
            "unauthorized"
        } else {
            "bad-token"
        };

        let mut list_new_req = Request::builder().uri("/v1/puzzles/list/new");
        if let Some(t) = header_token {
            list_new_req = list_new_req.header("x-token", t);
        }
        let list_new_resp = app
            .clone()
            .oneshot(list_new_req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_error_code(list_new_resp, expected_code).await;

        let mut list_mine_req = Request::builder().uri("/v1/puzzles/list/mine");
        if let Some(t) = header_token {
            list_mine_req = list_mine_req.header("x-token", t);
        }
        let list_mine_resp = app
            .clone()
            .oneshot(list_mine_req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_error_code(list_mine_resp, expected_code).await;

        let mut download_req = Request::builder().uri("/v1/puzzles/download/RgRgRgRg");
        if let Some(t) = header_token {
            download_req = download_req.header("x-token", t);
        }
        let download_resp = app
            .clone()
            .oneshot(download_req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_error_code(download_resp, expected_code).await;

        let search_resp = search_request(app.clone(), header_token, "", "any", "any").await;
        assert_error_code(search_resp, expected_code).await;

        let _ = variant_name; // used only for readability of the loop above
    }
}

/// SC4/T-06-19: `completed` is computed relative to the CALLING user, never a global flag — proven
/// by inserting a `puzzle_completions` row directly for user A only (the `complete` endpoint itself
/// doesn't exist until plan 06-05, so this direct insert is the only way to construct the
/// precondition here) and observing the SAME puzzle read back as `completed: true` for A and
/// `completed: false` for B, on both `list/new` and `download`.
#[sqlx::test]
async fn completed_field_is_per_user(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let user_a = common::register_test_user(&pool, "completer-a").await;
    let user_b = common::register_test_user(&pool, "completer-b").await;
    let token_a = common::jwt_for(user_a);
    let token_b = common::jwt_for(user_b);

    let submitted = submit_puzzle(app.clone(), &token_a, "RbRbRbRb", "Completable Puzzle").await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number") as i32;

    sqlx::query(
        "INSERT INTO puzzle_completions (user_id, puzzle_id, time_taken, liked) VALUES ($1, $2, $3, $4)",
    )
    .bind(user_a)
    .bind(puzzle_id)
    .bind(12.5_f32)
    .bind(false)
    .execute(&pool)
    .await
    .expect("direct completion insert must succeed");

    for (token, expected_completed) in [(&token_a, true), (&token_b, false)] {
        let list_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/puzzles/list/new")
                    .header("x-token", token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let list_body = body_to_json(list_response).await;
        let entry = list_body
            .as_array()
            .expect("list/new returns a JSON array")
            .iter()
            .find(|p| p["shortKey"] == "RbRbRbRb")
            .expect("submitted puzzle present in list/new");
        assert_eq!(
            entry["completed"], expected_completed,
            "list/new completed mismatch for token"
        );

        let download_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/puzzles/download/RbRbRbRb")
                    .header("x-token", token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let download_body = body_to_json(download_response).await;
        assert_eq!(
            download_body["meta"]["completed"], expected_completed,
            "download completed mismatch for token"
        );
    }
}

/// D-10/D-13/T-06-17/T-06-18: `list/mine` returns the caller's own puzzles including ones they
/// hid, `list/new` excludes a hidden puzzle even for its own author, and `download` of a hidden
/// puzzle succeeds for its author but resolves to `not-found` for anyone else. The hide itself is
/// performed via direct SQL (`delete/:id` is plan 06-05 scope, not yet implemented).
#[sqlx::test]
async fn list_mine_returns_only_own_puzzles_including_hidden(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_a = common::register_test_user(&pool, "mine-author-a").await;
    let author_b = common::register_test_user(&pool, "mine-author-b").await;
    let token_a = common::jwt_for(author_a);
    let token_b = common::jwt_for(author_b);

    submit_puzzle(app.clone(), &token_a, "RcRcRcRc", "A First Puzzle").await;
    submit_puzzle(app.clone(), &token_a, "RwRwRwRw", "A Second Puzzle").await;
    submit_puzzle(app.clone(), &token_b, "RuRuRuRu", "B Puzzle").await;

    sqlx::query("UPDATE puzzles SET hidden_at = now(), hidden_by = $1 WHERE short_key = $2")
        .bind(author_a)
        .bind("RwRwRwRw")
        .execute(&pool)
        .await
        .expect("hiding puzzle must succeed");

    let mine_a_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/list/mine")
                .header("x-token", &token_a)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mine_a_body = body_to_json(mine_a_response).await;
    let mine_a_keys: Vec<&str> = mine_a_body
        .as_array()
        .expect("list/mine returns a JSON array")
        .iter()
        .map(|p| p["shortKey"].as_str().expect("shortKey is a string"))
        .collect();
    assert_eq!(mine_a_keys.len(), 2, "author A must see both own puzzles");
    assert!(mine_a_keys.contains(&"RcRcRcRc"));
    assert!(
        mine_a_keys.contains(&"RwRwRwRw"),
        "author A must still see their own hidden puzzle via mine"
    );

    let mine_b_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/list/mine")
                .header("x-token", &token_b)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mine_b_body = body_to_json(mine_b_response).await;
    let mine_b_keys: Vec<&str> = mine_b_body
        .as_array()
        .expect("list/mine returns a JSON array")
        .iter()
        .map(|p| p["shortKey"].as_str().expect("shortKey is a string"))
        .collect();
    assert_eq!(
        mine_b_keys,
        vec!["RuRuRuRu"],
        "author B sees only own puzzle"
    );

    let new_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/list/new")
                .header("x-token", &token_a)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let new_body = body_to_json(new_response).await;
    let new_keys: Vec<&str> = new_body
        .as_array()
        .expect("list/new returns a JSON array")
        .iter()
        .map(|p| p["shortKey"].as_str().expect("shortKey is a string"))
        .collect();
    assert!(
        !new_keys.contains(&"RwRwRwRw"),
        "list/new must not show a puzzle hidden by its own author"
    );

    let download_by_author_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/download/RwRwRwRw")
                .header("x-token", &token_a)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(download_by_author_response.status(), StatusCode::OK);
    let download_by_author_body = body_to_json(download_by_author_response).await;
    assert_eq!(download_by_author_body["meta"]["shortKey"], "RwRwRwRw");

    let download_by_stranger_response = app
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/download/RwRwRwRw")
                .header("x-token", &token_b)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_error_code(download_by_stranger_response, "not-found").await;
}

/// `search` filters on `title ILIKE` only (Phase 6 -> Phase 7 seam for difficulty/duration):
/// case-insensitive substring match, empty term returns everything, a literal `%` is escaped so it
/// matches nothing rather than acting as a wildcard, a hidden puzzle never appears, and `any`/`any`
/// filters exclude nothing.
#[sqlx::test]
async fn search_filters_by_title(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "search-author").await;
    let token = common::jwt_for(author_id);

    submit_puzzle(app.clone(), &token, "SrSrSrSr", "Robot Factory").await;
    submit_puzzle(app.clone(), &token, "SgSgSgSg", "Cutter Palace").await;
    submit_puzzle(app.clone(), &token, "SbSbSbSb", "Mixing Station").await;

    sqlx::query("UPDATE puzzles SET hidden_at = now(), hidden_by = $1 WHERE short_key = $2")
        .bind(author_id)
        .bind("SbSbSbSb")
        .execute(&pool)
        .await
        .expect("hiding puzzle must succeed");

    // Fragment present in exactly one title.
    let response = search_request(app.clone(), Some(&token), "robot", "any", "any").await;
    let body = body_to_json(response).await;
    let list = body.as_array().expect("search returns a JSON array");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["shortKey"], "SrSrSrSr");

    // Case-insensitive.
    let response = search_request(app.clone(), Some(&token), "ROBOT", "any", "any").await;
    let body = body_to_json(response).await;
    let list = body.as_array().expect("search returns a JSON array");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["shortKey"], "SrSrSrSr");

    // Empty search term returns everything visible (hidden puzzle excluded).
    let response = search_request(app.clone(), Some(&token), "", "any", "any").await;
    let body = body_to_json(response).await;
    let list = body.as_array().expect("search returns a JSON array");
    assert_eq!(
        list.len(),
        2,
        "empty search term must return all visible puzzles"
    );
    let keys: Vec<&str> = list
        .iter()
        .map(|p| p["shortKey"].as_str().expect("shortKey is a string"))
        .collect();
    assert!(
        !keys.contains(&"SbSbSbSb"),
        "hidden puzzle must never appear in search results"
    );

    // A literal `%` must be escaped, not interpreted as a wildcard -- proof it matches nothing.
    let response = search_request(app.clone(), Some(&token), "%", "any", "any").await;
    let body = body_to_json(response).await;
    let list = body.as_array().expect("search returns a JSON array");
    assert_eq!(
        list.len(),
        0,
        "a literal '%' search term must be escaped, not act as a wildcard"
    );
}

/// Issues a `POST /v1/puzzles/complete/:id` request.
async fn complete_request(
    app: axum::Router,
    token: Option<&str>,
    id: &str,
    time: f32,
    liked: bool,
) -> axum::response::Response {
    let body = json!({ "time": time, "liked": liked });
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/v1/puzzles/complete/{id}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        builder = builder.header("x-token", token);
    }
    app.oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

/// Issues a `POST /v1/puzzles/report/:id` request.
async fn report_request(
    app: axum::Router,
    token: Option<&str>,
    id: &str,
    reason: &str,
) -> axum::response::Response {
    let body = json!({ "reason": reason });
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/v1/puzzles/report/{id}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        builder = builder.header("x-token", token);
    }
    app.oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

/// Issues a `POST /v1/puzzles/delete/:id` request. No body at all -- the real client sends a
/// literal `{}`, and the handler declares no `Json` extractor, so an entirely empty body must
/// succeed identically.
async fn delete_request(
    app: axum::Router,
    token: Option<&str>,
    id: &str,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/v1/puzzles/delete/{id}"));
    if let Some(token) = token {
        builder = builder.header("x-token", token);
    }
    app.oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// D-02: rejoue LITTÉRALEMENT le scénario de référence -- 60s/`liked=false` puis 90s/`liked=true`
/// sur le même puzzle et le même utilisateur laisse `time_taken=60`, `liked=true`, une seule ligne.
/// Le cas symétrique (90 puis 60) confirme que `time_taken` ne se dégrade jamais dans les deux
/// sens. Vérifie aussi la frontière D-15 (Phase 7, ADR 0005) : les deux complétions se reflètent
/// correctement dans les agrégats calculés à la volée exposés par `download`, chacune sur son
/// propre puzzle, sans contamination croisée.
#[sqlx::test]
async fn completion_upsert_semantics(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "completion-author").await;
    let token = common::jwt_for(author_id);

    // Scenario 1 (reference, D-02): 60 then 90, liked flips false -> true.
    let submitted_1 = submit_puzzle(app.clone(), &token, "CwCwCwCw", "Completion One").await;
    let puzzle_id_1 = submitted_1["id"]
        .as_i64()
        .expect("submitted id is a number");

    let first_call = complete_request(
        app.clone(),
        Some(&token),
        &puzzle_id_1.to_string(),
        60.0,
        false,
    )
    .await;
    assert_eq!(first_call.status(), StatusCode::OK);
    let second_call = complete_request(
        app.clone(),
        Some(&token),
        &puzzle_id_1.to_string(),
        90.0,
        true,
    )
    .await;
    assert_eq!(second_call.status(), StatusCode::OK);

    let row_1: (f32, bool) = sqlx::query_as(
        "SELECT time_taken, liked FROM puzzle_completions WHERE user_id = $1 AND puzzle_id = $2",
    )
    .bind(author_id)
    .bind(puzzle_id_1 as i32)
    .fetch_one(&pool)
    .await
    .expect("completion row must exist");
    assert_eq!(
        row_1.0, 60.0,
        "time_taken must stay at the best (lowest) value"
    );
    assert!(
        row_1.1,
        "liked must be overwritten by the latest value sent"
    );

    let count_1: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM puzzle_completions WHERE user_id = $1 AND puzzle_id = $2",
    )
    .bind(author_id)
    .bind(puzzle_id_1 as i32)
    .fetch_one(&pool)
    .await
    .expect("count query must succeed");
    assert_eq!(
        count_1, 1,
        "re-completion must upsert, not insert a second row"
    );

    // Scenario 2 (symmetric): 90 then 60 -> time_taken stays at the min, 60.
    let submitted_2 = submit_puzzle(app.clone(), &token, "SpSpSpSp", "Completion Two").await;
    let puzzle_id_2 = submitted_2["id"]
        .as_i64()
        .expect("submitted id is a number");

    let third_call = complete_request(
        app.clone(),
        Some(&token),
        &puzzle_id_2.to_string(),
        90.0,
        false,
    )
    .await;
    assert_eq!(third_call.status(), StatusCode::OK);
    let fourth_call = complete_request(
        app.clone(),
        Some(&token),
        &puzzle_id_2.to_string(),
        60.0,
        true,
    )
    .await;
    assert_eq!(fourth_call.status(), StatusCode::OK);

    let row_2: (f32, bool) = sqlx::query_as(
        "SELECT time_taken, liked FROM puzzle_completions WHERE user_id = $1 AND puzzle_id = $2",
    )
    .bind(author_id)
    .bind(puzzle_id_2 as i32)
    .fetch_one(&pool)
    .await
    .expect("completion row must exist");
    assert_eq!(
        row_2.0, 60.0,
        "time_taken must land on the lower of the two replays"
    );

    // D-01 boundary, updated for D-15/ADR 0005: `puzzles.completions`/`likes`/`average_time` no
    // longer exist as stored columns to leave untouched -- the aggregates are derived live from
    // `puzzle_completions` at read time instead. What the boundary now proves is that the two
    // completions recorded above (one per puzzle, `liked=true` each) are correctly reflected by
    // `find_puzzle_by_id` via the HTTP `download` response: exactly 1 completion/1 like per
    // puzzle, nothing more, nothing borrowed from the other puzzle's row.
    let download_response_1 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/puzzles/download/{puzzle_id_1}"))
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(download_response_1.status(), StatusCode::OK);
    let download_body_1 = body_to_json(download_response_1).await;
    assert_eq!(download_body_1["meta"]["completions"], 1);
    assert_eq!(download_body_1["meta"]["likes"], 1);
}

/// D-15/D-16/D-17 (Phase 7, ADR 0005): two distinct users complete the same puzzle, only one of
/// them liking it -- `download` must report `completions = 2`, `likes = 1`, and `averageTime`
/// equal to the simple mean of the two recorded `time_taken` values, all computed live from
/// `puzzle_completions` rather than read from a stored column.
#[sqlx::test]
async fn aggregate_counts_reflect_completions(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "aggregate-author").await;
    let author_token = common::jwt_for(author_id);
    let liker_id = common::register_test_user(&pool, "aggregate-liker").await;
    let liker_token = common::jwt_for(liker_id);

    let submitted = submit_puzzle(app.clone(), &author_token, "SgSgSgSg", "Aggregate Puzzle").await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number");

    let first = complete_request(
        app.clone(),
        Some(&author_token),
        &puzzle_id.to_string(),
        40.0,
        false,
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let second = complete_request(
        app.clone(),
        Some(&liker_token),
        &puzzle_id.to_string(),
        60.0,
        true,
    )
    .await;
    assert_eq!(second.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/puzzles/download/{puzzle_id}"))
                .header("x-token", &author_token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_to_json(response).await;
    assert_eq!(body["meta"]["completions"], 2);
    assert_eq!(body["meta"]["likes"], 1);
    assert_eq!(
        body["meta"]["averageTime"].as_f64(),
        Some(50.0),
        "averageTime must be the simple mean of 40.0 and 60.0"
    );
}

/// D-17: `likes` is NOT a monotonic counter -- a user who re-completes a puzzle with
/// `liked = false` after previously liking it must see `likes` drop back down, while
/// `completions` (one row per user, upserted) stays at 1.
#[sqlx::test]
async fn aggregate_likes_can_decrease(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "decrease-author").await;
    let token = common::jwt_for(author_id);

    let submitted = submit_puzzle(app.clone(), &token, "RcRcRcRc", "Decrease Puzzle").await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number");

    let liked_call = complete_request(
        app.clone(),
        Some(&token),
        &puzzle_id.to_string(),
        30.0,
        true,
    )
    .await;
    assert_eq!(liked_call.status(), StatusCode::OK);
    let unliked_call = complete_request(
        app.clone(),
        Some(&token),
        &puzzle_id.to_string(),
        30.0,
        false,
    )
    .await;
    assert_eq!(unliked_call.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/puzzles/download/{puzzle_id}"))
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_to_json(response).await;
    assert_eq!(
        body["meta"]["likes"], 0,
        "removing a like via re-completion must bring likes back down"
    );
    assert_eq!(
        body["meta"]["completions"], 1,
        "re-completion upserts the same row, it does not add a second completion"
    );
}

/// D-14: `difficulty` stays `null` while `downloads = 0`, even once the puzzle has completions --
/// never a division by zero. Once `downloads` is forced to a nonzero value (simulating real
/// download traffic, which this plan does not implement the increment for -- see plan 07-02),
/// `difficulty` becomes `completions / downloads`.
#[sqlx::test]
async fn aggregate_difficulty_is_null_without_downloads(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "difficulty-author").await;
    let token = common::jwt_for(author_id);

    let submitted = submit_puzzle(app.clone(), &token, "SbSbSbSb", "Difficulty Puzzle").await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number");

    let completion = complete_request(
        app.clone(),
        Some(&token),
        &puzzle_id.to_string(),
        20.0,
        false,
    )
    .await;
    assert_eq!(completion.status(), StatusCode::OK);

    let response_zero_downloads = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/puzzles/download/{puzzle_id}"))
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response_zero_downloads.status(), StatusCode::OK);
    let body_zero_downloads = body_to_json(response_zero_downloads).await;
    assert_eq!(
        body_zero_downloads["meta"]["difficulty"],
        serde_json::Value::Null,
        "difficulty must be null while downloads = 0, never a division by zero"
    );

    // A second user completes the same puzzle so completions = 2, then `downloads` is forced to
    // 4 directly in the database (a dynamic query, never the `query!` macro -- this test-only
    // write has no business growing the versioned `.sqlx` offline cache, same discipline
    // `register_test_user` already applies).
    let second_user_id = common::register_test_user(&pool, "difficulty-second-user").await;
    let second_token = common::jwt_for(second_user_id);
    let second_completion = complete_request(
        app.clone(),
        Some(&second_token),
        &puzzle_id.to_string(),
        25.0,
        false,
    )
    .await;
    assert_eq!(second_completion.status(), StatusCode::OK);

    sqlx::query("UPDATE puzzles SET downloads = 4 WHERE id = $1")
        .bind(puzzle_id as i32)
        .execute(&pool)
        .await
        .expect("forcing downloads must succeed");

    let response_with_downloads = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/puzzles/download/{puzzle_id}"))
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response_with_downloads.status(), StatusCode::OK);
    let body_with_downloads = body_to_json(response_with_downloads).await;
    assert_eq!(body_with_downloads["meta"]["completions"], 2);
    assert_eq!(
        body_with_downloads["meta"]["difficulty"].as_f64(),
        Some(0.5),
        "difficulty = completions / downloads = 2 / 4 = 0.5"
    );
}

/// A puzzle with zero completions must still appear with `completions = 0`, `likes = 0`,
/// `averageTime: null` -- the derived-table `LEFT JOIN` must produce these defaults, never drop
/// the puzzle's row from the listing entirely.
#[sqlx::test]
async fn aggregate_average_time_ignores_no_completion(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "no-completion-author").await;
    let token = common::jwt_for(author_id);

    submit_puzzle(app.clone(), &token, "RyRyRyRy", "No Completion Puzzle").await;

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
    assert_eq!(list.len(), 1, "the puzzle must still appear in the listing");
    assert_eq!(list[0]["completions"], 0);
    assert_eq!(list[0]["likes"], 0);
    assert_eq!(list[0]["averageTime"], serde_json::Value::Null);
}

/// D-01/D-02: `:id` non numérique, puzzle inexistant, `time` nul ou négatif, absence de jeton --
/// chaque cas est refusé avec son code exact et ne laisse jamais de ligne dans `puzzle_completions`.
#[sqlx::test]
async fn complete_rejects_bad_input(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "complete-reject-author").await;
    let token = common::jwt_for(author_id);

    let submitted = submit_puzzle(app.clone(), &token, "RyRyRyRy", "Reject Puzzle").await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number");

    let bad_id_response = complete_request(app.clone(), Some(&token), "abc", 60.0, false).await;
    assert_error_code(bad_id_response, "bad-id").await;

    let not_found_response =
        complete_request(app.clone(), Some(&token), "999999", 60.0, false).await;
    assert_error_code(not_found_response, "not-found").await;

    let zero_time_response = complete_request(
        app.clone(),
        Some(&token),
        &puzzle_id.to_string(),
        0.0,
        false,
    )
    .await;
    assert_error_code(zero_time_response, "bad-payload").await;

    let negative_time_response = complete_request(
        app.clone(),
        Some(&token),
        &puzzle_id.to_string(),
        -10.0,
        false,
    )
    .await;
    assert_error_code(negative_time_response, "bad-payload").await;

    let unauthorized_response =
        complete_request(app.clone(), None, &puzzle_id.to_string(), 60.0, false).await;
    assert_error_code(unauthorized_response, "unauthorized").await;

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM puzzle_completions")
        .fetch_one(&pool)
        .await
        .expect("count query must succeed");
    assert_eq!(count, 0, "no rejected completion attempt must leave a row");
}

/// D-03/D-04: double signalement et auto-signalement refusés, `reason` hors énumération refusé,
/// puzzle inexistant refusé, absence de jeton refusée -- chacun avec son code exact.
#[sqlx::test]
async fn report_rules(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_a = common::register_test_user(&pool, "report-author-a").await;
    let user_b = common::register_test_user(&pool, "report-user-b").await;
    let token_a = common::jwt_for(author_a);
    let token_b = common::jwt_for(user_b);

    let submitted = submit_puzzle(app.clone(), &token_a, "WgWgWgWg", "Report Puzzle").await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number");

    let success_response = report_request(
        app.clone(),
        Some(&token_b),
        &puzzle_id.to_string(),
        "profane",
    )
    .await;
    assert_eq!(success_response.status(), StatusCode::OK);
    let success_body = body_to_json(success_response).await;
    assert_eq!(success_body, json!({ "success": true }));

    let status: String = sqlx::query_scalar(
        "SELECT status FROM puzzle_reports WHERE user_id = $1 AND puzzle_id = $2",
    )
    .bind(user_b)
    .bind(puzzle_id as i32)
    .fetch_one(&pool)
    .await
    .expect("report row must exist");
    assert_eq!(status, "pending");

    // D-03: double report by the same user is refused, still exactly one row.
    let duplicate_response = report_request(
        app.clone(),
        Some(&token_b),
        &puzzle_id.to_string(),
        "profane",
    )
    .await;
    assert_error_code(duplicate_response, "bad-payload").await;

    let count_after_duplicate: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM puzzle_reports WHERE user_id = $1 AND puzzle_id = $2",
    )
    .bind(user_b)
    .bind(puzzle_id as i32)
    .fetch_one(&pool)
    .await
    .expect("count query must succeed");
    assert_eq!(count_after_duplicate, 1);

    // D-03: self-report is refused, still exactly one row total on this puzzle.
    let self_report_response = report_request(
        app.clone(),
        Some(&token_a),
        &puzzle_id.to_string(),
        "profane",
    )
    .await;
    assert_error_code(self_report_response, "can-not-report-your-own-puzzle").await;

    let total_after_self_report: i64 =
        sqlx::query_scalar("SELECT count(*) FROM puzzle_reports WHERE puzzle_id = $1")
            .bind(puzzle_id as i32)
            .fetch_one(&pool)
            .await
            .expect("count query must succeed");
    assert_eq!(total_after_self_report, 1);

    // D-04: an out-of-enum reason is refused before any write is attempted.
    let bad_reason_response =
        report_request(app.clone(), Some(&token_b), &puzzle_id.to_string(), "spam").await;
    assert_error_code(bad_reason_response, "bad-payload").await;

    let not_found_response = report_request(app.clone(), Some(&token_b), "999999", "profane").await;
    assert_error_code(not_found_response, "not-found").await;

    let unauthorized_response =
        report_request(app.clone(), None, &puzzle_id.to_string(), "profane").await;
    assert_error_code(unauthorized_response, "unauthorized").await;
}

/// D-08/D-09/D-11/D-12: `delete/:id` est réservé à l'auteur (`no-permission` pour un tiers,
/// distinct de `not-found`), `:id` non numérique refusé (`bad-id`), la ligne `puzzles` n'est
/// jamais détruite (D-08), `hidden_by` porte toujours l'`id` de l'auteur -- jamais `NULL` (D-09,
/// ADR 0002) -- et une seconde suppression par le même auteur réussit encore (idempotence).
#[sqlx::test]
async fn delete_author_only(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_a = common::register_test_user(&pool, "delete-author-a").await;
    let user_b = common::register_test_user(&pool, "delete-user-b").await;
    let token_a = common::jwt_for(author_a);
    let token_b = common::jwt_for(user_b);

    let submitted = submit_puzzle(app.clone(), &token_a, "WbWbWbWb", "Delete Puzzle").await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number");

    // D-12: a non-author gets no-permission, and the row stays unhidden.
    let forbidden_response =
        delete_request(app.clone(), Some(&token_b), &puzzle_id.to_string()).await;
    assert_error_code(forbidden_response, "no-permission").await;

    let hidden_after_forbidden: bool =
        sqlx::query_scalar("SELECT hidden_at IS NOT NULL FROM puzzles WHERE id = $1")
            .bind(puzzle_id as i32)
            .fetch_one(&pool)
            .await
            .expect("puzzle row must exist");
    assert!(
        !hidden_after_forbidden,
        "a non-author's rejected delete must not hide the puzzle"
    );

    let not_found_response = delete_request(app.clone(), Some(&token_a), "999999").await;
    assert_error_code(not_found_response, "not-found").await;

    let bad_id_response = delete_request(app.clone(), Some(&token_a), "abc").await;
    assert_error_code(bad_id_response, "bad-id").await;

    let success_response =
        delete_request(app.clone(), Some(&token_a), &puzzle_id.to_string()).await;
    assert_eq!(success_response.status(), StatusCode::OK);
    let success_body = body_to_json(success_response).await;
    assert_eq!(success_body, json!({ "success": true }));

    let (is_hidden, hidden_by): (bool, Option<uuid::Uuid>) =
        sqlx::query_as("SELECT hidden_at IS NOT NULL, hidden_by FROM puzzles WHERE id = $1")
            .bind(puzzle_id as i32)
            .fetch_one(&pool)
            .await
            .expect("puzzle row must exist");
    assert!(is_hidden, "successful delete must set hidden_at");
    assert_eq!(
        hidden_by,
        Some(author_a),
        "hidden_by must be the author's own id, never NULL (D-09/ADR 0002)"
    );

    let still_exists: i64 = sqlx::query_scalar("SELECT count(*) FROM puzzles WHERE id = $1")
        .bind(puzzle_id as i32)
        .fetch_one(&pool)
        .await
        .expect("count query must succeed");
    assert_eq!(
        still_exists, 1,
        "D-08: the row must never be physically deleted"
    );

    // Idempotence (Pattern 4): a second delete by the same author still succeeds.
    let second_delete_response =
        delete_request(app.clone(), Some(&token_a), &puzzle_id.to_string()).await;
    assert_eq!(second_delete_response.status(), StatusCode::OK);
    let second_delete_body = body_to_json(second_delete_response).await;
    assert_eq!(second_delete_body, json!({ "success": true }));
}

/// D-10/D-13: après suppression par l'auteur, le puzzle disparaît de `list/new` pour tout le
/// monde (y compris l'auteur), reste visible via `list/mine` de l'auteur, se télécharge encore
/// pour l'auteur mais pas pour un tiers, ne peut plus être complété par un tiers, et n'apparaît
/// plus dans `search`.
#[sqlx::test]
async fn deleted_puzzle_disappears_from_catalog_but_not_for_author(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_a = common::register_test_user(&pool, "vanish-author-a").await;
    let user_b = common::register_test_user(&pool, "vanish-user-b").await;
    let token_a = common::jwt_for(author_a);
    let token_b = common::jwt_for(user_b);

    let submitted = submit_puzzle(app.clone(), &token_a, "WcWcWcWc", "Vanishing Puzzle").await;
    let puzzle_id = submitted["id"].as_i64().expect("submitted id is a number");

    let delete_response = delete_request(app.clone(), Some(&token_a), &puzzle_id.to_string()).await;
    assert_eq!(delete_response.status(), StatusCode::OK);

    for token in [&token_a, &token_b] {
        let list_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/puzzles/list/new")
                    .header("x-token", token)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let list_body = body_to_json(list_response).await;
        let list = list_body.as_array().expect("list/new returns a JSON array");
        assert!(
            !list.iter().any(|p| p["shortKey"] == "WcWcWcWc"),
            "list/new must never show a puzzle hidden by its own author"
        );
    }

    let mine_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/list/mine")
                .header("x-token", &token_a)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mine_body = body_to_json(mine_response).await;
    let mine_list = mine_body
        .as_array()
        .expect("list/mine returns a JSON array");
    assert!(
        mine_list.iter().any(|p| p["shortKey"] == "WcWcWcWc"),
        "list/mine must still show the author's own hidden puzzle"
    );

    let download_by_author = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/download/WcWcWcWc")
                .header("x-token", &token_a)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(download_by_author.status(), StatusCode::OK);

    let download_by_stranger = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/download/WcWcWcWc")
                .header("x-token", &token_b)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_error_code(download_by_stranger, "not-found").await;

    let complete_by_stranger = complete_request(
        app.clone(),
        Some(&token_b),
        &puzzle_id.to_string(),
        30.0,
        false,
    )
    .await;
    assert_error_code(complete_by_stranger, "not-found").await;

    let search_response =
        search_request(app.clone(), Some(&token_a), "Vanishing", "any", "any").await;
    let search_body = body_to_json(search_response).await;
    let search_list = search_body.as_array().expect("search returns a JSON array");
    assert!(
        !search_list.iter().any(|p| p["shortKey"] == "WcWcWcWc"),
        "search must never surface a hidden puzzle"
    );
}

/// D-05/T-06-13: `search` rejects unknown `difficulty`/`duration` values and an oversized
/// `searchTerm` with `bad-payload`; `list` rejects an unrecognized category with `bad-category`;
/// `top-rated` stays a recognized category answering `200 []`, never an error.
#[sqlx::test]
async fn search_and_list_reject_bad_filters_and_category(pool: PgPool) {
    let state = common::test_state(pool.clone());
    let app = savez::app(state);

    let author_id = common::register_test_user(&pool, "reject-author").await;
    let token = common::jwt_for(author_id);

    let bad_difficulty_response =
        search_request(app.clone(), Some(&token), "", "impossible", "any").await;
    assert_error_code(bad_difficulty_response, "bad-payload").await;

    let bad_duration_response =
        search_request(app.clone(), Some(&token), "", "any", "eternal").await;
    assert_error_code(bad_duration_response, "bad-payload").await;

    let too_long_term = "a".repeat(101);
    let bad_term_response =
        search_request(app.clone(), Some(&token), &too_long_term, "any", "any").await;
    assert_error_code(bad_term_response, "bad-payload").await;

    let bad_category_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/list/wat")
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_error_code(bad_category_response, "bad-category").await;

    let top_rated_response = app
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/list/top-rated")
                .header("x-token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(top_rated_response.status(), StatusCode::OK);
    let top_rated_body = body_to_json(top_rated_response).await;
    assert_eq!(top_rated_body, json!([]));
}
