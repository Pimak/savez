//! Configurable EN/FR profanity word list (REQ-moderation, ROADMAP SC5), replacing the six-word
//! `PROFANE_WORDS` constant `src/validation.rs` used to hardcode (`TODO(Phase 7)`). The list lives
//! in the `profanity_words` table (migration `20260810000004`), read behind a TTL cache
//! (`db::PROFANITY_CACHE_TTL`) so `validation::validate_title` itself stays pure and synchronous —
//! it receives the list as a parameter, it never fetches it (07-08-PLAN.md `<interfaces>`).
//!
//! `ProfanityCache` mirrors `auth::cache::AuthCache`'s shape: a `moka::future::Cache` fetch
//! reduced through a small `Copy` internal error type, never an unwrap of the shared pointer moka
//! hands back — see that module's doc comment for the full rationale (an `Arc`-wrapped `AppError`
//! cannot be reduced to an owned `AppError` without either losing information or a fragile
//! unwrap). Unlike `AuthCache`, this cache has a single key (`()`): the list is global, never
//! scoped to a user.

use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::repository::{log_moderation_action, moderation_action};

/// Sorted, deduplicated, lowercase word list. `contains_token` relies on the sortedness for a
/// binary search — never a substring `contains`, which would reintroduce the "Scunthorpe problem"
/// (see `validation::validate_title`'s own doc comment for the canonical example).
#[derive(Debug, Default, Clone)]
pub struct ProfanityList(Vec<String>);

impl ProfanityList {
    /// Lowercases every entry, sorts, and deduplicates — the list read from the database is not
    /// guaranteed to arrive in any particular order or case (though the seed migration and
    /// `add_word` both already normalize to lowercase before writing).
    pub fn from_words(words: Vec<String>) -> Self {
        let mut words: Vec<String> = words.into_iter().map(|w| w.to_lowercase()).collect();
        words.sort_unstable();
        words.dedup();
        Self(words)
    }

    /// EXACT token match, never a substring search — a legitimate word that merely CONTAINS a
    /// forbidden word (e.g. a town name containing a slur as a substring) must not match.
    pub fn contains_token(&self, token: &str) -> bool {
        self.0.binary_search_by(|word| word.as_str().cmp(token)).is_ok()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Internal, `Copy` reduction of a database fetch failure — same rationale as
/// `auth::cache::CacheFetchError` (moka wraps the closure's error in an `Arc` for its request
/// coalescence, and `AppError` is neither `Clone` nor `Copy`). Unlike `CacheFetchError`, there is
/// only one failure mode here (a missing profanity list is not a meaningful state — an empty
/// table is not an error), so this type carries no variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FetchError;

/// TTL-cached global profanity list, single-key (`()`) because the list itself is global — unlike
/// `AuthCache`, which is keyed per `Uuid`.
#[derive(Clone)]
pub struct ProfanityCache {
    inner: moka::future::Cache<(), Arc<ProfanityList>>,
}

impl ProfanityCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: moka::future::Cache::builder().time_to_live(ttl).build(),
        }
    }

    /// Reads the current profanity list, at most once per TTL window — same request-coalescence
    /// property `AuthCache::get` relies on (a cache miss under N concurrent callers triggers
    /// exactly one database fetch, not N).
    pub async fn get(&self, pool: &PgPool) -> Result<Arc<ProfanityList>, AppError> {
        self.inner
            .try_get_with((), fetch_all_words(pool))
            .await
            .map_err(|_: Arc<FetchError>| {
                // The real sqlx error was already logged in `fetch_all_words` below — this
                // placeholder variant exists only to carry a `Database`-flavored 5xx through
                // `AppError`'s existing `into_response` path, the same placeholder-variant
                // convention `error.rs`'s own `database_variant_stays_5xx` test uses to construct
                // an `AppError::Database` without a real, specific `sqlx::Error` at hand.
                AppError::Database(sqlx::Error::WorkerCrashed)
            })
    }
}

/// Fetches every word in `profanity_words`, unfiltered by language — `ProfanityList` itself does
/// not distinguish EN from FR at match time (SPEC §4.6 wants both languages blocked by the same
/// filter, not two separate ones). A database error is logged here (the detail would not survive
/// reduction to `FetchError`, never let it disappear silently) before being reduced.
async fn fetch_all_words(pool: &PgPool) -> Result<Arc<ProfanityList>, FetchError> {
    let rows = sqlx::query!("SELECT word FROM profanity_words")
        .fetch_all(pool)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "failed to fetch profanity word list");
            FetchError
        })?;

    Ok(Arc::new(ProfanityList::from_words(
        rows.into_iter().map(|row| row.word).collect(),
    )))
}

/// Adds one word to the profanity list (CLI-only, plan 07-09: `savez mod profanity add`).
/// Normalizes to lowercase before writing — the same casing discipline `ProfanityList::from_words`
/// applies on read, so a word added here always matches consistently once the cache picks it up.
///
/// Refuses (`AppError::BadPayload`) an empty word, a word containing a space (this filter matches
/// whole tokens only — a multi-word entry could never match anything), or a language outside
/// `{en, fr}` — mirrored by, but independent of, the migration's own `CHECK` constraint on `lang`
/// (same defense-in-depth discipline `validation::validate_report_reason`/`repository::
/// resolve_report` already apply elsewhere in this codebase).
///
/// `word` is the table's `PRIMARY KEY` (migration `20260810000004`): `ON CONFLICT (word) DO
/// NOTHING` is therefore the single source of truth for "already present", with no `SELECT`
/// beforehand — same TOCTOU-avoidance discipline `repository::insert_user`/`insert_puzzle` apply
/// to their own unique constraints.
///
/// Returns `true` if a row was actually inserted, `false` if the word was already present (a
/// no-op). Either way, a `moderation_log` row is appended (T-07-45: every list modification is
/// traced, unconditionally — the audit trail records the moderator's INTENT, not merely whether
/// the state visibly changed, the same posture `repository::hide_puzzle`/`unhide_puzzle` already
/// apply to their own idempotent calls).
///
/// Does NOT invalidate `ProfanityCache`: the change becomes visible at the next TTL expiry (D-10's
/// posture for the role/ban cache, reapplied here — see `db::PROFANITY_CACHE_TTL`'s doc comment).
pub async fn add_word(
    pool: &PgPool,
    word: &str,
    lang: &str,
    moderator_id: Uuid,
) -> Result<bool, AppError> {
    let normalized = word.to_lowercase();
    if normalized.is_empty() || normalized.contains(' ') || !matches!(lang, "en" | "fr") {
        return Err(AppError::BadPayload);
    }

    let result = sqlx::query!(
        "INSERT INTO profanity_words (word, lang) VALUES ($1, $2) ON CONFLICT (word) DO NOTHING",
        normalized,
        lang,
    )
    .execute(pool)
    .await?;
    let inserted = result.rows_affected() > 0;

    log_moderation_action(
        pool,
        moderator_id,
        moderation_action::PROFANITY_UPDATE,
        "profanity",
        &normalized,
        Some(serde_json::json!({ "op": "add", "lang": lang, "effective": inserted })),
    )
    .await?;

    Ok(inserted)
}

/// Removes one word from the profanity list (CLI-only, plan 07-09: `savez mod profanity remove`).
/// Normalizes to lowercase before deleting, same discipline as `add_word` — a caller passing mixed
/// case still targets the correct row, since every write path (seed migration, `add_word`) only
/// ever stores lowercase.
///
/// Returns `true` if a row was actually deleted, `false` if no such word existed (a no-op) —
/// mirrors `add_word`'s own true/false contract. Same unconditional `moderation_log` write as
/// `add_word` (T-07-45), and the same no-invalidation posture on `ProfanityCache`.
pub async fn remove_word(pool: &PgPool, word: &str, moderator_id: Uuid) -> Result<bool, AppError> {
    let normalized = word.to_lowercase();

    let result = sqlx::query!("DELETE FROM profanity_words WHERE word = $1", normalized)
        .execute(pool)
        .await?;
    let removed = result.rows_affected() > 0;

    log_moderation_action(
        pool,
        moderator_id,
        moderation_action::PROFANITY_UPDATE,
        "profanity",
        &normalized,
        Some(serde_json::json!({ "op": "remove", "effective": removed })),
    )
    .await?;

    Ok(removed)
}

/// Lists every word in the profanity list, optionally filtered to one language (CLI-only, plan
/// 07-09: `savez mod profanity list`). `lang = None` returns every entry regardless of language —
/// same `$1::text IS NULL OR ...` optional-filter idiom `repository::list_reports` already uses
/// for its own optional `status` filter.
///
/// T-07-46 (accept): this function is deliberately never called from any HTTP route — only the
/// CLI (shell access) exposes the full word list, there is no public endpoint that would let an
/// unauthenticated caller enumerate every filtered word.
pub async fn list_words(pool: &PgPool, lang: Option<&str>) -> Result<Vec<(String, String)>, AppError> {
    let rows = sqlx::query!(
        r#"
        SELECT word, lang FROM profanity_words
        WHERE ($1::text IS NULL OR lang = $1)
        ORDER BY lang, word
        "#,
        lang
    )
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|row| (row.word, row.lang)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_words_normalizes_case_and_dedupes() {
        let list = ProfanityList::from_words(vec![
            "Fuck".to_string(),
            "shit".to_string(),
            "FUCK".to_string(),
            "Merde".to_string(),
        ]);
        assert_eq!(list.len(), 3, "case-insensitive duplicates must collapse to one entry");
        assert!(list.contains_token("fuck"));
        assert!(list.contains_token("shit"));
        assert!(list.contains_token("merde"));
    }

    #[test]
    fn contains_token_matches_exact_word_only() {
        let list = ProfanityList::from_words(vec!["cunt".to_string()]);
        assert!(list.contains_token("cunt"));
        // The canonical Scunthorpe-problem regression: a legitimate word that CONTAINS a
        // forbidden word as a substring must never match a token-exact filter.
        assert!(
            !list.contains_token("scunthorpe"),
            "a substring match would reintroduce the Scunthorpe problem"
        );
    }

    #[test]
    fn empty_list_contains_nothing() {
        let list = ProfanityList::from_words(vec![]);
        assert!(list.is_empty());
        assert!(!list.contains_token("anything"));
    }
}
