//! Server-side re-implementation of every rule the real shapez client already applies
//! client-side (shape-key grammar, title format, placement bounds). The client is NEVER an
//! authority — its own validation is display convenience only, exactly like `submit()`'s
//! existing author-attribution discipline never trusts a client-supplied author field
//! (`src/routes/puzzles.rs`). Every function here is pure (no database, no I/O) and independently
//! unit-testable.

use crate::error::AppError;
use crate::routes::puzzles::{PuzzleGameBuilding, PuzzleGameData};

/// Title length bounds, on the *trimmed* string (`puzzle_editor_review.js:102-109`).
const MIN_TITLE_LEN: usize = 4;
const MAX_TITLE_LEN: usize = 20;

/// Every shape-key layer is exactly this many characters (`shape_definition.js:192-244`).
const SHAPE_LAYER_LEN: usize = 8;
/// A shape key has 1 to `MAX_SHAPE_LAYERS` colon-delimited layers.
const MAX_SHAPE_LAYERS: usize = 4;

/// Load guard for `search`, not part of the client's own contract: bounds the string interpolated
/// into an `ILIKE '%...%'` scan (consumed by the plan 06-04 `search` handler).
const MAX_SEARCH_TERM_LEN: usize = 100;

/// Valid shape characters (rect/circle/star/windmill), case-sensitive (`enumShortcodeToSubShape`).
const SHAPE_CHARS: [char; 4] = ['R', 'C', 'S', 'W'];
/// Valid color characters (red/green/blue/yellow/purple/cyan/white/uncolored), case-sensitive
/// (`enumShortcodeToColor`).
const COLOR_CHARS: [char; 8] = ['r', 'g', 'b', 'y', 'p', 'c', 'w', 'u'];

/// Plain color literals accepted by `parseItemCode` in place of a shape key, compared
/// case-insensitively (`src/js/game/colors.js:2-13`, `enumColors`).
const COLOR_LITERALS: [&str; 8] = [
    "red",
    "green",
    "blue",
    "yellow",
    "purple",
    "cyan",
    "white",
    "uncolored",
];

/// D-04: the only three accepted `reason` values on `POST /v1/puzzles/report/:id`.
const REPORT_REASONS: [&str; 3] = ["profane", "unsolvable", "trolling"];

/// Accepted `search` filter values (06-04 consumes these; validated here so the phase's single
/// `validation.rs` owner covers every enum, per this plan's objective).
const SEARCH_DIFFICULTIES: [&str; 4] = ["any", "easy", "medium", "hard"];
const SEARCH_DURATIONS: [&str; 4] = ["any", "short", "medium", "long"];

/// Literal port of `ShapeDefinition.isValidShortKeyInternal` (`shape_definition.js:192-244`, see
/// 06-RESEARCH.md "D-14 Resolution"). Iterates over `chars()`, never bytes, so the length checks
/// are correct even if a caller somehow supplies non-ASCII input (which will simply fail the
/// per-quadrant match below, never panic). No allocation beyond the initial `split(':')`.
pub fn is_valid_shape_short_key(key: &str) -> bool {
    let layers: Vec<&str> = key.split(':').collect();
    // Rule 1: 0 layers (unreachable through `split`, which always yields >=1 item, but kept for
    // fidelity to the ported spec) or more than MAX_SHAPE_LAYERS layers is invalid.
    if layers.is_empty() || layers.len() > MAX_SHAPE_LAYERS {
        return false;
    }
    for layer in layers {
        // Rule 2: each layer must be exactly SHAPE_LAYER_LEN characters.
        if layer.chars().count() != SHAPE_LAYER_LEN {
            return false;
        }
        let mut chars = layer.chars();
        let mut any_filled = false;
        // Rule 3: 4 quadrants of 2 characters (shapeChar, colorChar), read in order.
        for _ in 0..4 {
            let (Some(shape_char), Some(color_char)) = (chars.next(), chars.next()) else {
                // Unreachable given the length check above (8 chars == 4 quadrants of 2), kept as
                // a panic-free fallback that never calls a panicking accessor.
                return false;
            };
            if SHAPE_CHARS.contains(&shape_char) {
                // Rule 4: shape in {R,C,S,W} requires a valid color character.
                if !COLOR_CHARS.contains(&color_char) {
                    return false;
                }
                any_filled = true;
            } else if shape_char == '-' {
                // Rule 5: shape '-' requires color '-' exactly.
                if color_char != '-' {
                    return false;
                }
            } else {
                // Rule 6: any other shape character is invalid.
                return false;
            }
        }
        // Rule 7: every layer must have at least one non-"--" quadrant.
        if !any_filled {
            return false;
        }
    }
    true
}

/// Port of `PuzzleSerializer.parseItemCode` (`puzzle_serializer.js:114-132`): trims, then accepts
/// EITHER a case-insensitive color literal OR a shape key valid per
/// [`is_valid_shape_short_key`] — the shape-key branch is checked on the trimmed but
/// NOT lowercased string, since shape-key characters are case-sensitive while color literals are
/// not. This asymmetry is intentional, straight from the client source, not an oversight.
pub fn is_valid_item_code(code: &str) -> bool {
    let trimmed = code.trim();
    if COLOR_LITERALS
        .iter()
        .any(|literal| trimmed.eq_ignore_ascii_case(literal))
    {
        return true;
    }
    is_valid_shape_short_key(trimmed)
}

/// Port of the submit-dialog title gate (`puzzle_editor_review.js:102-109`): length 4-20 on the
/// *trimmed* string, charset `[a-zA-Z0-9_- ]`, plus a profanity filter applied by exact-token
/// match (not substring) to avoid the classic "Scunthorpe problem" false-positive. REQ-moderation/
/// ROADMAP SC5: the list itself is no longer a hardcoded constant here — it is configurable, lives
/// in the `profanity_words` table, and is threaded in by the caller as `profanity` (07-08-PLAN.md
/// `<interfaces>`), so this function stays PURE and SYNCHRONOUS: it receives the list, it never
/// fetches it. Token-exact matching is no longer a compromise accepted for a minimal built-in
/// list — it is now an assumed property of the filter itself (SPEC §4.6: "un filtre naïf suffit au
/// lancement, l'objectif est de bloquer l'évident, la modération humaine gère le reste"). Returns
/// the trimmed title on success: the trimmed form is what gets stored, never the raw wire value
/// (the client itself trims before sending).
pub fn validate_title(raw: &str, profanity: &crate::profanity::ProfanityList) -> Result<String, AppError> {
    let trimmed = raw.trim();
    let char_count = trimmed.chars().count();
    if !(MIN_TITLE_LEN..=MAX_TITLE_LEN).contains(&char_count) {
        return Err(AppError::BadTitle);
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ' ')
    {
        return Err(AppError::BadTitle);
    }

    let lowered = trimmed.to_lowercase();
    for token in lowered.split([' ', '_', '-']) {
        if profanity.contains_token(token) {
            return Err(AppError::ProfaneTitle);
        }
    }

    Ok(trimmed.to_string())
}

/// The puzzle's own `shortKey` is validated with the SAME grammar as an `item` shape key
/// (`puzzle_editor_review.js:133-139` literally calls `ShapeDefinition.isValidShortKey`) — it does
/// not need to correspond to any shape actually used inside the puzzle. Returns the trimmed key on
/// success, mirroring [`validate_title`]'s trim-then-store discipline.
pub fn validate_short_key(raw: &str) -> Result<String, AppError> {
    let trimmed = raw.trim();
    if !is_valid_shape_short_key(trimmed) {
        return Err(AppError::BadShortKey);
    }
    Ok(trimmed.to_string())
}

/// Validates a submitted puzzle's game data end-to-end. The check order below is mandatory and not
/// permutable — it determines which single error code comes back when multiple rules are broken at
/// once:
///
/// (a) `bounds.w`/`bounds.h` zero (no placeable cell at all) ⇒ `BadBuildingPlacement`.
/// (b) zero `Emitter` buildings ⇒ `NoEmitters`.
/// (c) zero `Goal` buildings ⇒ `NoGoals`.
/// (d) every `Emitter`/`Goal`'s `item`, in document order, must pass [`is_valid_item_code`].
/// (e) every building (including `Block`) must sit inside `bounds`.
/// (f) no two buildings may share the same `(x, y)` cell — Assumption A2 (06-RESEARCH.md): the
///     real client editor cannot produce such a puzzle, so this is a server-only invariant, not a
///     ported client rule.
///
/// Bounds arithmetic is carried out in `i64` (`min = -((dimension + 1) / 2)`, valid range
/// `[min, min + dimension)`) so that an extreme `bounds` value can never overflow `i32` (T-06-12).
pub fn validate_game_data(data: &PuzzleGameData) -> Result<(), AppError> {
    if data.bounds.w == 0 || data.bounds.h == 0 {
        return Err(AppError::BadBuildingPlacement);
    }

    let emitter_count = data
        .buildings
        .iter()
        .filter(|building| matches!(building, PuzzleGameBuilding::Emitter { .. }))
        .count();
    if emitter_count == 0 {
        return Err(AppError::NoEmitters);
    }

    let goal_count = data
        .buildings
        .iter()
        .filter(|building| matches!(building, PuzzleGameBuilding::Goal { .. }))
        .count();
    if goal_count == 0 {
        return Err(AppError::NoGoals);
    }

    for building in &data.buildings {
        match building {
            PuzzleGameBuilding::Emitter { item, .. } => {
                if !is_valid_item_code(item) {
                    return Err(AppError::BadShapeKeyInEmitter);
                }
            }
            PuzzleGameBuilding::Goal { item, .. } => {
                if !is_valid_item_code(item) {
                    return Err(AppError::BadShapeKeyInGoal);
                }
            }
            PuzzleGameBuilding::Block { .. } => {}
        }
    }

    let min_x = -((data.bounds.w as i64 + 1) / 2);
    let max_x_exclusive = min_x + data.bounds.w as i64;
    let min_y = -((data.bounds.h as i64 + 1) / 2);
    let max_y_exclusive = min_y + data.bounds.h as i64;

    let mut occupied_cells: Vec<(i32, i32)> = Vec::with_capacity(data.buildings.len());
    for building in &data.buildings {
        let pos = match building {
            PuzzleGameBuilding::Emitter { pos, .. } => pos,
            PuzzleGameBuilding::Goal { pos, .. } => pos,
            PuzzleGameBuilding::Block { pos } => pos,
        };
        let x = pos.x as i64;
        let y = pos.y as i64;
        if x < min_x || x >= max_x_exclusive || y < min_y || y >= max_y_exclusive {
            return Err(AppError::BadBuildingPlacement);
        }
        // Assumption A2: two buildings on the same cell are rejected — not verified against the
        // real client contract (it cannot produce this), the server is the sole judge here.
        if occupied_cells.contains(&(pos.x, pos.y)) {
            return Err(AppError::BadBuildingPlacement);
        }
        occupied_cells.push((pos.x, pos.y));
    }

    Ok(())
}

/// D-04: `reason` on `POST /v1/puzzles/report/:id` is a strict, case-sensitive enum.
pub fn validate_report_reason(reason: &str) -> Result<(), AppError> {
    if REPORT_REASONS.contains(&reason) {
        Ok(())
    } else {
        Err(AppError::BadPayload)
    }
}

/// `search` filter validation (consumed by plan 06-04): `search_term` is bounded to guard against
/// an unbounded `ILIKE '%...%'` scan (T-06-13, not a client-contract rule); `difficulty`/`duration`
/// are strict, case-sensitive enums.
pub fn validate_search_filters(
    search_term: &str,
    difficulty: &str,
    duration: &str,
) -> Result<(), AppError> {
    if search_term.chars().count() > MAX_SEARCH_TERM_LEN {
        return Err(AppError::BadPayload);
    }
    if !SEARCH_DIFFICULTIES.contains(&difficulty) {
        return Err(AppError::BadPayload);
    }
    if !SEARCH_DURATIONS.contains(&duration) {
        return Err(AppError::BadPayload);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::puzzles::{Bounds, Pos};

    // --- is_valid_shape_short_key --------------------------------------------------------

    #[test]
    fn accepts_well_formed_shape_keys() {
        assert!(is_valid_shape_short_key("CuCuCuCu"));
        assert!(is_valid_shape_short_key("RrRrRrRr"));
        assert!(is_valid_shape_short_key("SwSwSwSw"));
        assert!(is_valid_shape_short_key("WuWuWuWu"));
        assert!(is_valid_shape_short_key("Cu------"));
        assert!(is_valid_shape_short_key("CuCuCuCu:RuRuRuRu"));
        assert!(is_valid_shape_short_key(
            "CuCuCuCu:CuCuCuCu:CuCuCuCu:CuCuCuCu"
        ));
    }

    #[test]
    fn rejects_shape_prefix_regression() {
        // Pitfall 1 (06-RESEARCH.md): the real grammar has NO "shape:" prefix. A key with this
        // prefix must be rejected, not silently accepted.
        assert!(!is_valid_shape_short_key("shape:CuCuCuCu"));
    }

    #[test]
    fn rejects_malformed_shape_keys() {
        assert!(!is_valid_shape_short_key(""));
        assert!(!is_valid_shape_short_key("CuCuCuC")); // 7 chars
        assert!(!is_valid_shape_short_key("CuCuCuCuC")); // 9 chars
        assert!(!is_valid_shape_short_key("--------")); // all-empty layer
        assert!(!is_valid_shape_short_key("CxCuCuCu")); // unknown color
        assert!(!is_valid_shape_short_key("XuCuCuCu")); // unknown shape
        assert!(!is_valid_shape_short_key("C-CuCuCu")); // shape with '-' color
        assert!(!is_valid_shape_short_key("-uCuCuCu")); // '-' shape with non-'-' color
        assert!(!is_valid_shape_short_key(
            "CuCuCuCu:CuCuCuCu:CuCuCuCu:CuCuCuCu:CuCuCuCu"
        )); // 5 layers
        assert!(!is_valid_shape_short_key("cucucucu")); // wrong case
    }

    // --- is_valid_item_code ---------------------------------------------------------------

    #[test]
    fn accepts_well_formed_item_codes() {
        assert!(is_valid_item_code("CuCuCuCu"));
        assert!(is_valid_item_code("red"));
        assert!(is_valid_item_code("RED"));
        assert!(is_valid_item_code("Uncolored"));
        assert!(is_valid_item_code("  blue  "));
    }

    #[test]
    fn rejects_malformed_item_codes() {
        assert!(!is_valid_item_code("shape:CuCuCuCu"));
        assert!(!is_valid_item_code("rouge"));
        assert!(!is_valid_item_code(""));
        assert!(!is_valid_item_code("cucucucu"));
    }

    // --- validate_title ----------------------------------------------------------------------

    /// A small fixed word set built inline per test (07-08-PLAN.md `<interfaces>`): `validate_title`
    /// receives the profanity list as a parameter, so these tests stay entirely database-free.
    fn test_profanity() -> crate::profanity::ProfanityList {
        crate::profanity::ProfanityList::from_words(vec![
            "fuck".to_string(),
            "shit".to_string(),
            "bitch".to_string(),
            "merde".to_string(),
            "putain".to_string(),
            "connard".to_string(),
        ])
    }

    #[test]
    fn accepts_well_formed_titles() {
        let profanity = test_profanity();
        assert_eq!(validate_title("Test", &profanity).unwrap(), "Test");
        assert_eq!(
            validate_title("Test Puzzle", &profanity).unwrap(),
            "Test Puzzle"
        );
        assert_eq!(
            validate_title("A_B-C 123", &profanity).unwrap(),
            "A_B-C 123"
        );
        let twenty_chars = "a".repeat(20);
        assert_eq!(
            validate_title(&twenty_chars, &profanity).unwrap(),
            twenty_chars
        );
        assert_eq!(
            validate_title("  Test Puzzle  ", &profanity).unwrap(),
            "Test Puzzle",
            "must return the trimmed form"
        );
    }

    #[test]
    fn rejects_bad_title_format() {
        let profanity = test_profanity();
        assert!(matches!(
            validate_title("abc", &profanity),
            Err(AppError::BadTitle)
        ));
        let twenty_one_chars = "a".repeat(21);
        assert!(matches!(
            validate_title(&twenty_one_chars, &profanity),
            Err(AppError::BadTitle)
        ));
        assert!(matches!(
            validate_title("Tést Puzzle", &profanity),
            Err(AppError::BadTitle)
        ));
        assert!(matches!(
            validate_title("Test!Puzzle", &profanity),
            Err(AppError::BadTitle)
        ));
        assert!(matches!(
            validate_title("    ", &profanity),
            Err(AppError::BadTitle)
        ));
    }

    #[test]
    fn rejects_profane_titles_case_insensitively() {
        let profanity = test_profanity();
        assert!(matches!(
            validate_title("FUCK You Puzzle", &profanity),
            Err(AppError::ProfaneTitle)
        ));
    }

    // --- validate_short_key ------------------------------------------------------------------

    #[test]
    fn accepts_well_formed_short_key() {
        assert_eq!(validate_short_key("CuCuCuCu").unwrap(), "CuCuCuCu");
    }

    #[test]
    fn rejects_malformed_short_key() {
        assert!(matches!(
            validate_short_key("shape:CuCuCuCu"),
            Err(AppError::BadShortKey)
        ));
        assert!(matches!(
            validate_short_key("download-both-1"),
            Err(AppError::BadShortKey)
        ));
        assert!(matches!(
            validate_short_key("123"),
            Err(AppError::BadShortKey)
        ));
        assert!(matches!(validate_short_key(""), Err(AppError::BadShortKey)));
    }

    // --- validate_game_data ------------------------------------------------------------------

    fn base_buildings() -> Vec<PuzzleGameBuilding> {
        vec![
            PuzzleGameBuilding::Emitter {
                item: "CuCuCuCu".to_string(),
                pos: Pos { x: 0, y: 0, r: 0 },
            },
            PuzzleGameBuilding::Goal {
                item: "CuCuCuCu".to_string(),
                pos: Pos { x: 4, y: 3, r: 90 },
            },
        ]
    }

    fn game_data_with_block(bounds: Bounds, block_pos: Pos) -> PuzzleGameData {
        let mut buildings = base_buildings();
        buildings.push(PuzzleGameBuilding::Block { pos: block_pos });
        PuzzleGameData {
            version: 1,
            bounds,
            buildings,
            excluded_buildings: vec![],
        }
    }

    #[test]
    fn accepts_canonical_payload() {
        let data = game_data_with_block(Bounds { w: 10, h: 8 }, Pos { x: 2, y: 2, r: 180 });
        assert!(validate_game_data(&data).is_ok());
    }

    #[test]
    fn rejects_no_emitters() {
        let data = PuzzleGameData {
            version: 1,
            bounds: Bounds { w: 10, h: 8 },
            buildings: vec![PuzzleGameBuilding::Goal {
                item: "CuCuCuCu".to_string(),
                pos: Pos { x: 4, y: 3, r: 90 },
            }],
            excluded_buildings: vec![],
        };
        assert!(matches!(
            validate_game_data(&data),
            Err(AppError::NoEmitters)
        ));
    }

    #[test]
    fn rejects_no_goals() {
        let data = PuzzleGameData {
            version: 1,
            bounds: Bounds { w: 10, h: 8 },
            buildings: vec![PuzzleGameBuilding::Emitter {
                item: "CuCuCuCu".to_string(),
                pos: Pos { x: 0, y: 0, r: 0 },
            }],
            excluded_buildings: vec![],
        };
        assert!(matches!(validate_game_data(&data), Err(AppError::NoGoals)));
    }

    #[test]
    fn rejects_bad_emitter_item() {
        let data = PuzzleGameData {
            version: 1,
            bounds: Bounds { w: 10, h: 8 },
            buildings: vec![
                PuzzleGameBuilding::Emitter {
                    item: "nope".to_string(),
                    pos: Pos { x: 0, y: 0, r: 0 },
                },
                PuzzleGameBuilding::Goal {
                    item: "CuCuCuCu".to_string(),
                    pos: Pos { x: 4, y: 3, r: 90 },
                },
            ],
            excluded_buildings: vec![],
        };
        assert!(matches!(
            validate_game_data(&data),
            Err(AppError::BadShapeKeyInEmitter)
        ));
    }

    #[test]
    fn rejects_bad_goal_item() {
        let data = PuzzleGameData {
            version: 1,
            bounds: Bounds { w: 10, h: 8 },
            buildings: vec![
                PuzzleGameBuilding::Emitter {
                    item: "CuCuCuCu".to_string(),
                    pos: Pos { x: 0, y: 0, r: 0 },
                },
                PuzzleGameBuilding::Goal {
                    item: "nope".to_string(),
                    pos: Pos { x: 4, y: 3, r: 90 },
                },
            ],
            excluded_buildings: vec![],
        };
        assert!(matches!(
            validate_game_data(&data),
            Err(AppError::BadShapeKeyInGoal)
        ));
    }

    #[test]
    fn rejects_building_out_of_bounds() {
        // Exactly the old fixtures' position, per this plan's <behavior> spec.
        let data = game_data_with_block(Bounds { w: 10, h: 8 }, Pos { x: 5, y: 5, r: 0 });
        assert!(matches!(
            validate_game_data(&data),
            Err(AppError::BadBuildingPlacement)
        ));
    }

    #[test]
    fn rejects_two_buildings_on_same_cell() {
        // Assumption A2: the real editor cannot produce this, the server is sole judge.
        let mut buildings = base_buildings();
        buildings.push(PuzzleGameBuilding::Block {
            pos: Pos { x: 0, y: 0, r: 0 }, // same cell as the emitter
        });
        let data = PuzzleGameData {
            version: 1,
            bounds: Bounds { w: 10, h: 8 },
            buildings,
            excluded_buildings: vec![],
        };
        assert!(matches!(
            validate_game_data(&data),
            Err(AppError::BadBuildingPlacement)
        ));
    }

    #[test]
    fn rejects_zero_area_bounds() {
        let data = game_data_with_block(Bounds { w: 0, h: 8 }, Pos { x: 0, y: 0, r: 0 });
        assert!(matches!(
            validate_game_data(&data),
            Err(AppError::BadBuildingPlacement)
        ));
    }

    #[test]
    fn accepts_negative_boundary_corner() {
        let data = game_data_with_block(Bounds { w: 10, h: 8 }, Pos { x: -5, y: -4, r: 0 });
        assert!(validate_game_data(&data).is_ok());
    }

    #[test]
    fn rejects_x_below_min_bound() {
        let data = game_data_with_block(Bounds { w: 10, h: 8 }, Pos { x: -6, y: 0, r: 0 });
        assert!(matches!(
            validate_game_data(&data),
            Err(AppError::BadBuildingPlacement)
        ));
    }

    #[test]
    fn rejects_y_at_exclusive_max_bound() {
        let data = game_data_with_block(Bounds { w: 10, h: 8 }, Pos { x: 0, y: 4, r: 0 });
        assert!(matches!(
            validate_game_data(&data),
            Err(AppError::BadBuildingPlacement)
        ));
    }

    // --- validate_report_reason --------------------------------------------------------------

    #[test]
    fn accepts_known_report_reasons() {
        assert!(validate_report_reason("profane").is_ok());
        assert!(validate_report_reason("unsolvable").is_ok());
        assert!(validate_report_reason("trolling").is_ok());
    }

    #[test]
    fn rejects_unknown_report_reasons() {
        assert!(matches!(
            validate_report_reason("Profane"),
            Err(AppError::BadPayload)
        ));
        assert!(matches!(
            validate_report_reason("spam"),
            Err(AppError::BadPayload)
        ));
        assert!(matches!(
            validate_report_reason(""),
            Err(AppError::BadPayload)
        ));
    }

    // --- validate_search_filters -------------------------------------------------------------

    #[test]
    fn accepts_known_search_filter_combinations() {
        for difficulty in SEARCH_DIFFICULTIES {
            for duration in SEARCH_DURATIONS {
                assert!(validate_search_filters("", difficulty, duration).is_ok());
                assert!(validate_search_filters("robots", difficulty, duration).is_ok());
            }
        }
    }

    #[test]
    fn rejects_unknown_search_filters() {
        assert!(matches!(
            validate_search_filters("", "impossible", "any"),
            Err(AppError::BadPayload)
        ));
        assert!(matches!(
            validate_search_filters("", "any", "eternal"),
            Err(AppError::BadPayload)
        ));
        let too_long = "a".repeat(MAX_SEARCH_TERM_LEN + 1);
        assert!(matches!(
            validate_search_filters(&too_long, "any", "any"),
            Err(AppError::BadPayload)
        ));
    }
}
