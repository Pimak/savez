use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use savez::db::AppState;
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
                item: "shape:CuCuCuCu".to_string(),
                pos: Pos { x: 0, y: 0, r: 0 },
            },
            PuzzleGameBuilding::Goal {
                item: "shape:CuCuCuCu".to_string(),
                pos: Pos { x: 5, y: 5, r: 90 },
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
    assert_eq!(emitter["item"], "shape:CuCuCuCu");
    assert!(emitter.get("pos").is_some());
    assert_eq!(emitter["pos"]["x"], 0);
    assert_eq!(emitter["pos"]["y"], 0);
    assert_eq!(emitter["pos"]["r"], 0);

    let goal = &buildings[1];
    assert_eq!(goal["type"], "goal");
    assert_eq!(goal["item"], "shape:CuCuCuCu");

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
/// 6-table SPEC §4.3 schema plus the D-02 dev-seed-author row, without any manual setup here.
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

    let seed_row: (String, String) = sqlx::query_as(
        "SELECT name, verified_via FROM users WHERE id = '00000000-0000-0000-0000-000000000001'",
    )
    .fetch_one(&pool)
    .await
    .expect("D-02 seed row must exist after migrations run");
    assert_eq!(
        seed_row,
        ("dev-seed-author".to_string(), "dev-seed".to_string())
    );
}

fn sample_game_data() -> Value {
    json!({
        "version": 1,
        "bounds": { "w": 10, "h": 8 },
        "buildings": [
            { "type": "emitter", "item": "shape:CuCuCuCu", "pos": { "x": 0, "y": 0, "r": 0 } },
            { "type": "goal", "item": "shape:CuCuCuCu", "pos": { "x": 5, "y": 5, "r": 90 } },
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

/// T-03-21 mitigation proof: a submission body carrying client-supplied `author`/`authorId`
/// fields is accepted, but the row that lands in the database is always attributed to the
/// D-02 seed author — never to the values the client sent.
#[sqlx::test]
async fn submit_persists_puzzle(pool: PgPool) {
    let state = AppState { pool: pool.clone() };
    let app = savez::app(state);

    let body = json!({
        "title": "Test Puzzle",
        "shortKey": "submit-persists-1",
        "data": sample_game_data(),
        "author": "attaquant",
        "authorId": "11111111-1111-1111-1111-111111111111",
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/puzzles/submit")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let row: (String, String) =
        sqlx::query_as("SELECT title, author_id::text FROM puzzles WHERE short_key = $1")
            .bind("submit-persists-1")
            .fetch_one(&pool)
            .await
            .expect("submitted puzzle row must exist");
    assert_eq!(row.0, "Test Puzzle");
    assert_eq!(row.1, "00000000-0000-0000-0000-000000000001");
}

/// A puzzle downloads identically by numeric `id` and by `shortKey`, in the `{ meta, game }`
/// shape, and an unknown id resolves to 404.
#[sqlx::test]
async fn download_by_id_and_by_short_key(pool: PgPool) {
    let state = AppState { pool: pool.clone() };
    let app = savez::app(state);

    let submit_body = json!({
        "title": "Download Test Puzzle",
        "shortKey": "download-both-1",
        "data": sample_game_data(),
    });

    let submit_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/puzzles/submit")
                .header(header::CONTENT_TYPE, "application/json")
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
                .uri("/v1/puzzles/download/download-both-1")
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
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(not_found_response.status(), StatusCode::NOT_FOUND);
}

async fn submit_puzzle(app: axum::Router, short_key: &str, title: &str) -> Value {
    let body = json!({
        "title": title,
        "shortKey": short_key,
        "data": sample_game_data(),
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/puzzles/submit")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_to_json(response).await
}

/// D-04: `new` is the only real category. Two puzzles submitted successively must both come back,
/// most-recently-submitted first (`created_at DESC, id DESC`), each attributed to the seed author.
#[sqlx::test]
async fn list_new_returns_submitted_puzzles_newest_first(pool: PgPool) {
    let state = AppState { pool: pool.clone() };
    let app = savez::app(state);

    submit_puzzle(app.clone(), "list-new-first", "First Puzzle").await;
    submit_puzzle(app.clone(), "list-new-second", "Second Puzzle").await;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/list/new")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_to_json(response).await;
    let list = body.as_array().expect("list/new returns a JSON array");
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["shortKey"], "list-new-second");
    assert_eq!(list[1]["shortKey"], "list-new-first");
    assert_eq!(list[0]["author"], "dev-seed-author");
    assert_eq!(list[1]["author"], "dev-seed-author");
}

/// D-05: `top-rated` and `mine` always answer 200 with an empty array, never an error and never a
/// copy of `new`'s content, even once puzzles exist.
#[sqlx::test]
async fn list_top_rated_and_mine_return_empty(pool: PgPool) {
    let state = AppState { pool: pool.clone() };
    let app = savez::app(state);

    submit_puzzle(app.clone(), "list-empty-1", "Some Puzzle").await;

    for category in ["top-rated", "mine"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/puzzles/list/{category}"))
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
    let state = AppState { pool: pool.clone() };
    let app = savez::app(state);

    submit_puzzle(app.clone(), "download-counter-1", "Counter Puzzle").await;

    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/puzzles/download/download-counter-1")
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
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_to_json(response).await;
    let list = body.as_array().expect("list/new returns a JSON array");
    let entry = list
        .iter()
        .find(|p| p["shortKey"] == "download-counter-1")
        .expect("submitted puzzle present in list/new");
    assert_eq!(entry["downloads"], 0);
    assert_eq!(entry["completed"], false);

    let downloads: i32 = sqlx::query_scalar("SELECT downloads FROM puzzles WHERE short_key = $1")
        .bind("download-counter-1")
        .fetch_one(&pool)
        .await
        .expect("puzzle row must exist");
    assert_eq!(downloads, 0);
}

/// Pitfall 1 lock-in: Postgres has no unsigned integer type, so `likes`/`downloads`/`completions`
/// are stored `i32` and cast to `u32` on the way out. This proves non-zero values round-trip
/// intact through that cast rather than silently wrapping or truncating.
#[sqlx::test]
async fn counters_round_trip_as_u32(pool: PgPool) {
    let state = AppState { pool: pool.clone() };
    let app = savez::app(state);

    submit_puzzle(app.clone(), "counters-roundtrip-1", "Roundtrip Puzzle").await;

    sqlx::query(
        "UPDATE puzzles SET likes = $1, downloads = $2, completions = $3 WHERE short_key = $4",
    )
    .bind(42_i32)
    .bind(7_i32)
    .bind(3_i32)
    .bind("counters-roundtrip-1")
    .execute(&pool)
    .await
    .expect("counter update must succeed");

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/puzzles/list/new")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_to_json(response).await;
    let list = body.as_array().expect("list/new returns a JSON array");
    let entry = list
        .iter()
        .find(|p| p["shortKey"] == "counters-roundtrip-1")
        .expect("submitted puzzle present in list/new");
    assert_eq!(entry["likes"], 42);
    assert_eq!(entry["downloads"], 7);
    assert_eq!(entry["completions"], 3);
}
