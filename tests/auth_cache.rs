//! Proves D-09 (role/ban state is stale by at most `TTL`) and D-10 (no targeted invalidation
//! exists — staleness only ever clears by TTL expiry) empirically, against a real `AuthCache`
//! built at a real TTL — deliberately separate from the rest of the suite, which uses
//! `common::TEST_AUTH_CACHE_TTL` (1ms) to stay stable when a test bans a user and immediately
//! calls a route through the same `AppState`.

mod common;

use savez::auth::cache::{AuthCache, Role};
use std::time::Duration;

/// D-09: a role change committed directly to the database is not observed by a cache entry that
/// has not yet expired.
#[sqlx::test]
async fn role_change_is_not_visible_before_ttl_expiry(pool: sqlx::PgPool) {
    let user_id = common::register_test_user_with_role(&pool, "cache-fresh", "user").await;
    let cache = AuthCache::new(Duration::from_secs(10));

    let first = cache.get(&pool, user_id).await.expect("first read");
    assert_eq!(first.role, Role::User);

    sqlx::query("UPDATE users SET role = 'admin' WHERE id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("promote to admin directly in the database");

    let second = cache
        .get(&pool, user_id)
        .await
        .expect("second read, still within the 10s TTL");
    assert_eq!(
        second.role,
        Role::User,
        "D-09: a cached value must not observe a DB change before its TTL expires"
    );
}

/// D-10: no targeted invalidation exists — the only way a role/ban change becomes visible is TTL
/// expiry.
#[sqlx::test]
async fn role_change_is_visible_after_ttl_expiry(pool: sqlx::PgPool) {
    let user_id = common::register_test_user_with_role(&pool, "cache-stale", "user").await;
    let cache = AuthCache::new(Duration::from_millis(1));

    let first = cache.get(&pool, user_id).await.expect("first read");
    assert_eq!(first.role, Role::User);

    sqlx::query("UPDATE users SET role = 'admin' WHERE id = $1")
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("promote to admin directly in the database");

    tokio::time::sleep(Duration::from_millis(20)).await;

    let second = cache
        .get(&pool, user_id)
        .await
        .expect("second read, after the 1ms TTL has elapsed");
    assert_eq!(
        second.role,
        Role::Admin,
        "D-10: no targeted invalidation exists -- staleness clears only once the TTL elapses"
    );
}
