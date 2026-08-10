//! `savez`'s clap grammar (D-04, ADR 0004, REQ-moderation). Two mutually exclusive top-level
//! modes of the same binary: `savez serve` launches the HTTP server (the entire, unchanged
//! sequence that used to run implicitly with no argument before this phase); `savez mod <action>`
//! runs exactly one moderation action directly against the database and exits — no HTTP round
//! trip, no running server required (see `src/cli/moderation.rs`'s module doc comment for the
//! full rationale, including the deliberate absence of a role check on `--moderator`).
//!
//! Every `ValueEnum` below relies on clap's default kebab-case rendering to produce EXACTLY the
//! lowercase string literals the database's own `CHECK` constraints and this codebase's constant
//! modules expect (`upheld`/`rejected`, `user`/`moderator`/`admin`, `read`/`write`, `en`/`fr`) --
//! never a free-form `String` on these arguments, which is this CLI's ASVS V5 input-validation
//! layer (07-09-PLAN.md `<interfaces>`).

pub mod moderation;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(name = "savez", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Launch the HTTP server (D-04: this was the implicit no-argument behavior before Phase 7).
    Serve,
    /// Moderation admin commands: direct database access, no running server required (ADR 0004).
    Mod {
        #[command(subcommand)]
        action: ModAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum ModAction {
    /// List the report queue, defaulting to pending reports.
    Reports {
        #[arg(long, value_enum, default_value = "pending")]
        status: ReportStatusFilter,
        #[arg(long, default_value_t = 50)]
        limit: i64,
        #[arg(long, default_value_t = 0)]
        offset: i64,
    },
    /// Resolve a report as upheld or rejected, along with every pending sibling report sharing
    /// the same puzzle and reason (D-08).
    Resolve {
        report_id: i32,
        #[arg(value_enum)]
        status: ResolveStatus,
        #[arg(long)]
        notes: Option<String>,
        #[arg(long)]
        moderator: String,
    },
    /// Hide a puzzle from the public catalog.
    Hide {
        puzzle_id: i32,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        moderator: String,
    },
    /// Unhide a previously hidden puzzle.
    Unhide {
        puzzle_id: i32,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        moderator: String,
    },
    /// Permanently delete a puzzle, freeing its short key for immediate reuse.
    Delete {
        puzzle_id: i32,
        #[arg(long)]
        moderator: String,
    },
    /// Ban a user, permanently by default or for a limited duration via --expires-in.
    Ban {
        user: String,
        #[arg(long)]
        reason: String,
        #[arg(long = "expires-in")]
        expires_in: Option<String>,
        #[arg(long)]
        moderator: String,
    },
    /// Lift one existing ban (targeted by its own row id, never "every ban" for a user, D-11).
    Unban {
        ban_id: i32,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        moderator: String,
    },
    /// Promote or demote a user's role.
    Promote {
        user: String,
        #[arg(value_enum)]
        role: RoleArg,
        #[arg(long)]
        moderator: String,
    },
    /// Read the append-only moderation audit journal, most recent first.
    Log {
        #[arg(long, default_value_t = 50)]
        limit: i64,
        #[arg(long, default_value_t = 0)]
        offset: i64,
    },
    /// Configure or list the persisted rate-limit thresholds (D-01/D-02/D-03).
    Ratelimit {
        #[command(subcommand)]
        action: RatelimitAction,
    },
    /// Manage the configurable EN/FR profanity word list.
    Profanity {
        #[command(subcommand)]
        action: ProfanityAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum RatelimitAction {
    /// Set (upsert) the limit for one route class and window.
    Set {
        #[arg(value_enum)]
        class: RouteClassArg,
        #[arg(long)]
        window: i32,
        #[arg(long)]
        limit: i32,
        #[arg(long)]
        moderator: String,
    },
    /// List every configured rate-limit threshold.
    List,
}

#[derive(Subcommand, Debug)]
pub enum ProfanityAction {
    /// Add one word to the profanity list.
    Add {
        word: String,
        #[arg(long, value_enum)]
        lang: LangArg,
        #[arg(long)]
        moderator: String,
    },
    /// Remove one word from the profanity list.
    Remove {
        word: String,
        #[arg(long)]
        moderator: String,
    },
    /// List the profanity words, optionally filtered by language.
    List {
        #[arg(long, value_enum)]
        lang: Option<LangArg>,
    },
}

/// Mirrors `puzzle_reports.status`'s two resolvable values exactly (`resolve_report` also rejects
/// anything else defensively, see `src/repository.rs`).
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolveStatus {
    /// The report is confirmed valid.
    Upheld,
    /// The report is dismissed.
    Rejected,
}

impl ResolveStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ResolveStatus::Upheld => "upheld",
            ResolveStatus::Rejected => "rejected",
        }
    }
}

/// Mirrors the `users_role_check` CHECK constraint's closed set (D-12) exactly.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoleArg {
    /// Regular user, no moderation rights.
    User,
    /// Moderator: report queue, hide/unhide, temporary bans.
    Moderator,
    /// Administrator: every moderator right plus permanent bans, deletion, role changes.
    Admin,
}

impl RoleArg {
    pub fn as_str(self) -> &'static str {
        match self {
            RoleArg::User => "user",
            RoleArg::Moderator => "moderator",
            RoleArg::Admin => "admin",
        }
    }
}

/// Mirrors `rate_limit_config.route_class`'s CHECK constraint exactly (D-02).
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteClassArg {
    /// Read-class routes (list/search/download/login).
    Read,
    /// Write-class routes (submit/complete/report/delete).
    Write,
}

impl From<RouteClassArg> for crate::ratelimit::RouteClass {
    fn from(value: RouteClassArg) -> Self {
        match value {
            RouteClassArg::Read => crate::ratelimit::RouteClass::Read,
            RouteClassArg::Write => crate::ratelimit::RouteClass::Write,
        }
    }
}

/// Mirrors `profanity_words.lang`'s CHECK constraint exactly (07-08-PLAN.md).
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LangArg {
    /// English.
    En,
    /// French.
    Fr,
}

impl LangArg {
    pub fn as_str(self) -> &'static str {
        match self {
            LangArg::En => "en",
            LangArg::Fr => "fr",
        }
    }
}

/// `savez mod reports --status`'s closed set. Unlike `puzzle_reports.status`'s own three values,
/// this adds a fourth, CLI-only `All` variant with no database equivalent -- `as_filter` maps it
/// to `None` (no `WHERE` filter at all), exactly matching `repository::list_reports`' own
/// `status: Option<&str>` contract.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportStatusFilter {
    /// Pending reports only (the default).
    Pending,
    /// Upheld reports only.
    Upheld,
    /// Rejected reports only.
    Rejected,
    /// Every status, no filter.
    All,
}

impl ReportStatusFilter {
    pub fn as_filter(self) -> Option<&'static str> {
        match self {
            ReportStatusFilter::Pending => Some("pending"),
            ReportStatusFilter::Upheld => Some("upheld"),
            ReportStatusFilter::Rejected => Some("rejected"),
            ReportStatusFilter::All => None,
        }
    }
}

/// Parses `--expires-in`'s short duration format: `<n>h`, `<n>d`, or `<n>w`. Absence of the flag
/// itself (an `Option<String>` at the call site) means a permanent ban -- this function is never
/// called in that case. Never panics: every rejection path returns a descriptive `Err(String)`,
/// which the CLI dispatch layer turns into a `CliError` (never an `unwrap`/`expect`).
pub fn parse_expires_in(raw: &str) -> Result<chrono::Duration, String> {
    if raw.len() < 2 {
        return Err(format!(
            "invalid --expires-in value {raw:?}: expected <n>h, <n>d or <n>w"
        ));
    }
    let (number, unit) = raw.split_at(raw.len() - 1);
    let count: i64 = number
        .parse()
        .map_err(|_| format!("invalid --expires-in value {raw:?}: expected <n>h, <n>d or <n>w"))?;

    match unit {
        "h" => Ok(chrono::Duration::hours(count)),
        "d" => Ok(chrono::Duration::days(count)),
        "w" => Ok(chrono::Duration::weeks(count)),
        _ => Err(format!(
            "invalid --expires-in value {raw:?}: expected <n>h, <n>d or <n>w"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_parses() {
        let cli = Cli::try_parse_from(["savez", "serve"]).expect("savez serve must parse");
        assert!(matches!(cli.command, Commands::Serve));
    }

    #[test]
    fn no_subcommand_fails() {
        assert!(
            Cli::try_parse_from(["savez"]).is_err(),
            "savez with no subcommand must fail: D-04 leaves no implicit default mode"
        );
    }

    #[test]
    fn mod_reports_defaults_to_pending() {
        let cli =
            Cli::try_parse_from(["savez", "mod", "reports"]).expect("savez mod reports must parse");
        let Commands::Mod { action } = cli.command else {
            panic!("expected Commands::Mod");
        };
        assert!(matches!(
            action,
            ModAction::Reports {
                status: ReportStatusFilter::Pending,
                limit: 50,
                offset: 0,
            }
        ));
    }

    #[test]
    fn mod_resolve_with_valid_status_parses() {
        let cli = Cli::try_parse_from([
            "savez",
            "mod",
            "resolve",
            "12",
            "upheld",
            "--moderator",
            "alice",
        ])
        .expect("savez mod resolve 12 upheld --moderator alice must parse");
        let Commands::Mod { action } = cli.command else {
            panic!("expected Commands::Mod");
        };
        assert!(matches!(
            action,
            ModAction::Resolve {
                report_id: 12,
                status: ResolveStatus::Upheld,
                moderator,
                ..
            } if moderator == "alice"
        ));
    }

    #[test]
    fn mod_resolve_with_invalid_status_fails() {
        assert!(
            Cli::try_parse_from([
                "savez",
                "mod",
                "resolve",
                "12",
                "maybe",
                "--moderator",
                "alice"
            ])
            .is_err(),
            "a status outside the ValueEnum closed set must be rejected at parse time"
        );
    }

    #[test]
    fn mod_ban_without_expires_in_parses() {
        let cli = Cli::try_parse_from([
            "savez",
            "mod",
            "ban",
            "bob",
            "--reason",
            "spam",
            "--moderator",
            "alice",
        ])
        .expect("savez mod ban bob --reason spam --moderator alice must parse");
        let Commands::Mod { action } = cli.command else {
            panic!("expected Commands::Mod");
        };
        assert!(matches!(
            action,
            ModAction::Ban { user, reason, expires_in: None, moderator }
                if user == "bob" && reason == "spam" && moderator == "alice"
        ));
    }

    #[test]
    fn mod_hide_without_moderator_fails() {
        assert!(
            Cli::try_parse_from(["savez", "mod", "hide", "4"]).is_err(),
            "--moderator is required on every write action (T-07-48)"
        );
    }

    #[test]
    fn parse_expires_in_accepts_hours_days_weeks() {
        assert_eq!(
            parse_expires_in("24h").unwrap(),
            chrono::Duration::hours(24)
        );
        assert_eq!(parse_expires_in("7d").unwrap(), chrono::Duration::days(7));
        assert_eq!(parse_expires_in("2w").unwrap(), chrono::Duration::weeks(2));
    }

    #[test]
    fn parse_expires_in_rejects_unknown_unit() {
        assert!(parse_expires_in("7x").is_err());
    }

    #[test]
    fn parse_expires_in_rejects_empty_string() {
        assert!(parse_expires_in("").is_err());
    }
}
