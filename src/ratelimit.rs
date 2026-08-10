//! Per-user rate limiting (D-01, D-02, D-03), persisted in PostgreSQL rather than in an
//! in-process/Redis token bucket -- `governor`/`tower-governor` are explicitly rejected (T-07-SC,
//! 07-RESEARCH.md "Don't Hand-Roll") because neither survives a server restart, and D-01 locks the
//! two-table split precisely so both the configured thresholds AND the per-user event log are
//! durable across restarts.
//!
//! `rate_limit_config` (few rows, modifiable by the future `savez mod ratelimit set` CLI, plan
//! 07-09) holds the numeric thresholds. `rate_limit_events` (one row per accepted request) is the
//! timestamped event log `check_and_record` counts against. D-03: only the numeric thresholds in
//! `rate_limit_config` are configurable at runtime -- the route→class mapping below is code, fixed
//! at compile time, never a database row or an environment variable.
//!
//! D-02: two classes (`read`/`write`), read carrying a far larger default ceiling than write, and
//! several windows can coexist for the same class (`UNIQUE (route_class, window_seconds)`, not
//! `UNIQUE (route_class)`) -- e.g. write's default 5/hour AND 20/day both apply simultaneously,
//! whichever is hit first wins. A class with zero configured rows imposes no limit at all: this is
//! not an edge case to special-case away, it is the explicit, intentional behavior of an
//! unconfigured class (used deliberately by `tests/common::relax_rate_limits` to disable limiting
//! in tests that need to exceed the seeded defaults).
//!
//! The counting window is SLIDING (a `COUNT(*)` over timestamped events younger than the window),
//! never a calendar bucket reset on a fixed schedule -- a fixed-bucket scheme under-counts near its
//! own reset boundary (07-RESEARCH.md "Don't Hand-Roll"), which a sliding window avoids by
//! construction. The interval arithmetic below always multiplies a bound parameter by a one-second
//! Postgres interval literal, never a string-concatenated `::interval` cast -- no dynamically built
//! SQL text anywhere in this project.
//!
//! Every accepted request also opportunistically purges this same user+class's events older than
//! the largest configured window, bounded to that one user and class -- without this,
//! `rate_limit_events` grows without bound since a row is written on every single accepted request.
//!
//! Route→class mapping (D-03, fixed in code, never in a database row):
//!
//! | Route | Classe |
//! |---|---|
//! | `POST /v1/public/login` | `read` |
//! | `GET /v1/puzzles/list/{category}` | `read` |
//! | `POST /v1/puzzles/search` | `read` |
//! | `GET /v1/puzzles/download/{id_or_key}` | `read` |
//! | `POST /v1/puzzles/submit` | `write` |
//! | `POST /v1/puzzles/complete/{id}` | `write` |
//! | `POST /v1/puzzles/report/{id}` | `write` |
//! | `POST /v1/puzzles/delete/{id}` | `write` |
//!
//! `/v1/moderation/*` (plan 07-10) is deliberately NOT in this table and never calls
//! `check_and_record`: those routes are already gated behind a privileged role, and imposing a
//! quota there would risk a moderator locking themselves out of moderation during exactly the kind
//! of high-volume abuse event that role exists to handle (T-07-37).

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteClass {
    Read,
    Write,
}

impl RouteClass {
    /// Literal values admitted by `rate_limit_config`'s/`rate_limit_events`' `CHECK (route_class IN
    /// ('read', 'write'))` constraint (rate_limit_config only; `rate_limit_events.route_class` has
    /// no CHECK of its own but is always written from this same function) -- these two strings must
    /// stay byte-identical to that constraint.
    pub fn as_str(self) -> &'static str {
        match self {
            RouteClass::Read => "read",
            RouteClass::Write => "write",
        }
    }
}

/// Checks every configured window for `(user_id, class)` against its limit, in ascending
/// `window_seconds` order, then records this request as a new event. Order of operations below is
/// mandatory:
///
/// 1. Read every configured window for `class`. Zero rows -> `Ok(())` immediately, no further
///    query runs -- an unconfigured class imposes no limit (also the lever `relax_rate_limits`
///    uses in tests).
/// 2. For each configured window, count events younger than that window. Any window at or past its
///    limit rejects the whole call with `AppError::RateLimited` -- no purge, no insert, this
///    request is not recorded.
/// 3. Purge this user+class's events older than the LARGEST configured window (bounding table
///    growth without discarding an event a still-active window needs to count).
/// 4. Record this request as a new event.
pub async fn check_and_record(
    pool: &PgPool,
    user_id: Uuid,
    class: RouteClass,
) -> Result<(), AppError> {
    let class_str = class.as_str();

    let windows = sqlx::query!(
        r#"
        SELECT window_seconds, limit_count
        FROM rate_limit_config
        WHERE route_class = $1
        ORDER BY window_seconds
        "#,
        class_str
    )
    .fetch_all(pool)
    .await?;

    if windows.is_empty() {
        return Ok(());
    }

    for window in &windows {
        let row = sqlx::query!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM rate_limit_events
            WHERE user_id = $1
              AND route_class = $2
              AND occurred_at > now() - ($3::int * interval '1 second')
            "#,
            user_id,
            class_str,
            window.window_seconds
        )
        .fetch_one(pool)
        .await?;

        if row.count >= i64::from(window.limit_count) {
            return Err(AppError::RateLimited);
        }
    }

    // `windows` is ordered ascending by `window_seconds` (the query above), so the last element is
    // the largest configured window -- the bound the purge below must not evict events still owed
    // to a smaller, still-active window.
    let largest_window_seconds = windows
        .last()
        .expect("checked non-empty above")
        .window_seconds;

    sqlx::query!(
        r#"
        DELETE FROM rate_limit_events
        WHERE user_id = $1
          AND route_class = $2
          AND occurred_at < now() - ($3::int * interval '1 second')
        "#,
        user_id,
        class_str,
        largest_window_seconds
    )
    .execute(pool)
    .await?;

    sqlx::query!(
        "INSERT INTO rate_limit_events (user_id, route_class) VALUES ($1, $2)",
        user_id,
        class_str
    )
    .execute(pool)
    .await?;

    Ok(())
}

/// Upserts one `(route_class, window_seconds)` threshold (CLI-only: `savez mod ratelimit set`,
/// plan 07-09). Never a `SELECT`-then-branch: the `UNIQUE (route_class, window_seconds)`
/// constraint is the single source of truth on whether a row already exists, exactly the same
/// `ON CONFLICT ... DO UPDATE` discipline `repository::insert_user`/`insert_puzzle` already apply
/// to their own unique constraints -- never a TOCTOU-prone read-then-write.
pub async fn set_config(
    pool: &PgPool,
    class: RouteClass,
    window_seconds: i32,
    limit_count: i32,
    moderator_id: Uuid,
) -> Result<(), AppError> {
    let class_str = class.as_str();

    sqlx::query!(
        r#"
        INSERT INTO rate_limit_config (route_class, window_seconds, limit_count)
        VALUES ($1, $2, $3)
        ON CONFLICT (route_class, window_seconds) DO UPDATE SET limit_count = EXCLUDED.limit_count, updated_at = now()
        "#,
        class_str,
        window_seconds,
        limit_count,
    )
    .execute(pool)
    .await?;

    crate::repository::log_moderation_action(
        pool,
        moderator_id,
        crate::repository::moderation_action::RATELIMIT_SET,
        "ratelimit",
        class_str,
        Some(serde_json::json!({
            "windowSeconds": window_seconds,
            "limitCount": limit_count,
        })),
    )
    .await?;

    Ok(())
}

/// Lists every configured rate-limit threshold, ordered by class then window (CLI-only: `savez mod
/// ratelimit list`, plan 07-09). Returns `(route_class, window_seconds, limit_count)` tuples --
/// this function has no HTTP-facing caller, so a dedicated response struct would add ceremony
/// with no reader.
pub async fn list_config(pool: &PgPool) -> Result<Vec<(String, i32, i32)>, AppError> {
    let rows = sqlx::query!(
        r#"
        SELECT route_class, window_seconds, limit_count
        FROM rate_limit_config
        ORDER BY route_class, window_seconds
        "#
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| (row.route_class, row.window_seconds, row.limit_count))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_as_str_matches_check_constraint() {
        assert_eq!(RouteClass::Read.as_str(), "read");
    }

    #[test]
    fn write_as_str_matches_check_constraint() {
        assert_eq!(RouteClass::Write.as_str(), "write");
    }
}
