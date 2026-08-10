//! Dispatch for `savez mod <action>` (D-04, ADR 0004, REQ-moderation). Placeholder shape wired by
//! this plan's Task 1 so `main.rs`'s restructured startup sequence has something to call from the
//! start; the real per-action delegation to `repository`/`ratelimit`/`profanity` lands in Task 2.

use sqlx::PgPool;

use crate::cli::ModAction;

#[derive(thiserror::Error, Debug)]
pub enum CliError {
    #[error("moderation CLI dispatch not yet implemented")]
    NotYetImplemented,
}

pub async fn dispatch(_pool: &PgPool, _action: ModAction) -> Result<(), CliError> {
    Err(CliError::NotYetImplemented)
}
