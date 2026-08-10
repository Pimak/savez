//! Dispatch for `savez mod <action>` (D-04, ADR 0004, REQ-moderation).
//!
//! This is a SECOND, independent entry point into the same binary, calling `repository::*`/
//! `ratelimit::*`/`profanity::*` directly against the database -- NOT an HTTP client of a running
//! `savez serve` process. `docs/cahier-des-charges.md` §4.6 line 198 originally describes the CLI
//! as consuming `/v1/moderation/*` HTTP routes; that text is superseded by D-04/ADR 0004
//! (07-RESEARCH.md, Pattern 7's "Architectural clarification"). Every match arm below is a thin
//! delegate: resolve any user identifier(s) needed, call exactly ONE `repository`/`ratelimit`/
//! `profanity` function (never a bare SQL statement issued directly from this file -- this
//! module's own acceptance criteria enforce that), then print a human-readable summary to stdout.
//! No moderation business rule is duplicated here; it already lives in the functions this module
//! calls (07-05 through 07-08).
//!
//! `--moderator` is required on every write action and resolved via `repository::find_user_id`
//! (UUID first, pseudo name as fallback): `moderation_log.moderator_id` is `NOT NULL REFERENCES
//! users(id)`, and defaulting silently to the seeded admin account would make a multi-moderator
//! audit trail meaningless the moment a second moderator exists (T-07-48). An unresolvable
//! `--moderator` interrupts the action BEFORE any write -- proven by `cli_mod_unknown_moderator_is_
//! an_error` in `tests/cli.rs`.
//!
//! This CLI performs NO role check of its own on the caller (T-07-49, accepted per ADR 0004):
//! whoever has a shell capable of running `savez mod` already has direct access to `DATABASE_URL`,
//! so an application-level role gate here would be purely decorative. The real role gate is the
//! HTTP layer's `ModeratorUser`/`AdminUser` extractors (plan 07-10) -- this is a deliberate design
//! decision, not an oversight.

use sqlx::PgPool;
use uuid::Uuid;

use crate::cli::{ModAction, ProfanityAction, RatelimitAction};
use crate::error::AppError;
use crate::{profanity, ratelimit, repository};

#[derive(thiserror::Error, Debug)]
pub enum CliError {
    #[error("{0}")]
    App(#[from] AppError),
    #[error("no user found matching {0:?}")]
    UnknownUser(String),
    #[error("{0}")]
    InvalidDuration(String),
}

/// Resolves a `--moderator`/target-user CLI argument (UUID or pseudo, `repository::find_user_id`)
/// to a `users.id`, or `CliError::UnknownUser` -- never a panic, never a silent default. Shared by
/// every write arm below that needs to attribute an action to a real account (the moderator
/// itself, and, for `Ban`/`Promote`, the target user).
async fn resolve_user(pool: &PgPool, name_or_id: &str) -> Result<Uuid, CliError> {
    repository::find_user_id(pool, name_or_id)
        .await?
        .ok_or_else(|| CliError::UnknownUser(name_or_id.to_string()))
}

/// Dispatches one parsed `ModAction` directly against the database, printing a human-readable
/// summary to stdout on success. `Err` is displayed on stderr by `main.rs`, mapped to a non-zero
/// exit code -- this module never aborts the process on a rejected action, only returns `Err`.
pub async fn dispatch(pool: &PgPool, action: ModAction) -> Result<(), CliError> {
    match action {
        ModAction::Reports {
            status,
            limit,
            offset,
        } => {
            let reports = repository::list_reports(pool, status.as_filter(), limit, offset).await?;
            if reports.is_empty() {
                println!("No reports.");
            } else {
                println!(
                    "{:<6} {:<8} {:<20} {:<10} {:<16} {:<12} {:<16} {:<8}",
                    "id", "puzzle", "short_key", "hidden", "reporter", "reason", "status", "author"
                );
                for report in reports {
                    println!(
                        "{:<6} {:<8} {:<20} {:<10} {:<16} {:<12} {:<16} {} ({} upheld)",
                        report.id,
                        report.puzzle_id,
                        report.puzzle_short_key,
                        report.puzzle_hidden,
                        report.reporter_name,
                        report.reason,
                        report.status,
                        report.author_name,
                        report.author_upheld_reports,
                    );
                }
            }
        }

        ModAction::Resolve {
            report_id,
            status,
            notes,
            moderator,
        } => {
            let moderator_id = resolve_user(pool, &moderator).await?;
            let resolved_ids =
                repository::resolve_report(pool, report_id, status.as_str(), moderator_id, notes.as_deref())
                    .await?;
            let also_resolved: Vec<i32> = resolved_ids
                .iter()
                .copied()
                .filter(|&id| id != report_id)
                .collect();
            println!(
                "Resolved report {report_id} as {}. {} report(s) total resolved: {resolved_ids:?}.",
                status.as_str(),
                resolved_ids.len()
            );
            if !also_resolved.is_empty() {
                println!(
                    "  (D-08: {} sibling report(s) with the same puzzle and reason also resolved: {also_resolved:?})",
                    also_resolved.len()
                );
            }
        }

        ModAction::Hide {
            puzzle_id,
            reason,
            moderator,
        } => {
            let moderator_id = resolve_user(pool, &moderator).await?;
            repository::hide_puzzle(pool, puzzle_id, moderator_id, reason.as_deref()).await?;
            println!("Puzzle {puzzle_id} hidden.");
        }

        ModAction::Unhide {
            puzzle_id,
            reason,
            moderator,
        } => {
            let moderator_id = resolve_user(pool, &moderator).await?;
            repository::unhide_puzzle(pool, puzzle_id, moderator_id, reason.as_deref()).await?;
            println!("Puzzle {puzzle_id} unhidden.");
        }

        ModAction::Delete {
            puzzle_id,
            moderator,
        } => {
            let moderator_id = resolve_user(pool, &moderator).await?;
            let short_key = repository::purge_puzzle(pool, puzzle_id, moderator_id).await?;
            println!("Puzzle {puzzle_id} permanently deleted. Short key freed: {short_key}");
        }

        ModAction::Ban {
            user,
            reason,
            expires_in,
            moderator,
        } => {
            let target_id = resolve_user(pool, &user).await?;
            let moderator_id = resolve_user(pool, &moderator).await?;
            let expires_at = match expires_in {
                Some(raw) => {
                    let duration =
                        crate::cli::parse_expires_in(&raw).map_err(CliError::InvalidDuration)?;
                    Some(chrono::Utc::now() + duration)
                }
                None => None,
            };
            let ban_id = repository::ban_user(pool, target_id, &reason, moderator_id, expires_at).await?;
            match expires_at {
                Some(at) => println!("User {user} banned (ban id {ban_id}), expires at {at}."),
                None => println!("User {user} banned permanently (ban id {ban_id})."),
            }
        }

        ModAction::Unban {
            ban_id,
            reason,
            moderator,
        } => {
            let moderator_id = resolve_user(pool, &moderator).await?;
            repository::lift_ban(pool, ban_id, &reason, moderator_id).await?;
            println!("Ban {ban_id} lifted.");
        }

        ModAction::Promote {
            user,
            role,
            moderator,
        } => {
            let target_id = resolve_user(pool, &user).await?;
            let moderator_id = resolve_user(pool, &moderator).await?;
            repository::set_user_role(pool, target_id, role.as_str(), moderator_id).await?;
            println!("User {user} role set to {}.", role.as_str());
        }

        ModAction::Log { limit, offset } => {
            let entries = repository::list_moderation_log(pool, limit, offset).await?;
            if entries.is_empty() {
                println!("No moderation log entries.");
            } else {
                println!(
                    "{:<6} {:<20} {:<16} {:<10} {:<10} {:<28} details",
                    "id", "moderator", "action", "target_type", "target_id", "created_at"
                );
                for entry in entries {
                    println!(
                        "{:<6} {:<20} {:<16} {:<10} {:<10} {:<28} {}",
                        entry.id,
                        entry.moderator_name,
                        entry.action,
                        entry.target_type,
                        entry.target_id,
                        entry.created_at,
                        entry
                            .details
                            .map(|d| d.to_string())
                            .unwrap_or_default(),
                    );
                }
            }
        }

        ModAction::Ratelimit { action } => match action {
            RatelimitAction::Set {
                class,
                window,
                limit,
                moderator,
            } => {
                let moderator_id = resolve_user(pool, &moderator).await?;
                ratelimit::set_config(pool, class.into(), window, limit, moderator_id).await?;
                println!("Rate limit set: class={class:?} window={window}s limit={limit}");
            }
            RatelimitAction::List => {
                let rows = ratelimit::list_config(pool).await?;
                if rows.is_empty() {
                    println!("No rate limit thresholds configured.");
                } else {
                    println!("{:<8} {:<12} limit", "class", "window(s)");
                    for (class, window_seconds, limit_count) in rows {
                        println!("{class:<8} {window_seconds:<12} {limit_count}");
                    }
                }
            }
        },

        ModAction::Profanity { action } => match action {
            ProfanityAction::Add {
                word,
                lang,
                moderator,
            } => {
                let moderator_id = resolve_user(pool, &moderator).await?;
                let inserted = profanity::add_word(pool, &word, lang.as_str(), moderator_id).await?;
                if inserted {
                    println!("Word {word:?} ({}) added.", lang.as_str());
                } else {
                    println!("Word {word:?} was already present (no-op, still logged).");
                }
            }
            ProfanityAction::Remove { word, moderator } => {
                let moderator_id = resolve_user(pool, &moderator).await?;
                let removed = profanity::remove_word(pool, &word, moderator_id).await?;
                if removed {
                    println!("Word {word:?} removed.");
                } else {
                    println!("Word {word:?} was not present (no-op, still logged).");
                }
            }
            ProfanityAction::List { lang } => {
                let words = profanity::list_words(pool, lang.map(|l| l.as_str())).await?;
                if words.is_empty() {
                    println!("No profanity words.");
                } else {
                    for (word, word_lang) in words {
                        println!("{word_lang:<4} {word}");
                    }
                }
            }
        },
    }

    Ok(())
}
