use crate::error::AppError;

// Bornes applicatives : `users.name` est un `TEXT` sans contrainte de longueur en base, la borne
// doit donc exister ici (ASVS V5).
const MIN_NAME_LEN: usize = 3;
const MAX_NAME_LEN: usize = 32;

/// RED phase stub (05-04 Task 2): deliberately accepts everything, so the reject_* tests below
/// fail for a real, callable reason before the GREEN commit closes the gap.
fn validate_name(_name: &str) -> Result<(), AppError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_typical_names() {
        assert!(validate_name("Zorg").is_ok());
        assert!(validate_name("joueur_42").is_ok());
        assert!(validate_name("a-b-c").is_ok());
    }

    #[test]
    fn rejects_too_short() {
        assert!(matches!(validate_name("ab"), Err(AppError::InvalidName)));
    }

    #[test]
    fn rejects_too_long() {
        let name = "a".repeat(33);
        assert!(matches!(validate_name(&name), Err(AppError::InvalidName)));
    }

    #[test]
    fn rejects_disallowed_charset() {
        assert!(matches!(
            validate_name("jean dupont"),
            Err(AppError::InvalidName)
        ));
        assert!(matches!(
            validate_name("élodie"),
            Err(AppError::InvalidName)
        ));
        assert!(matches!(
            validate_name("drop;table"),
            Err(AppError::InvalidName)
        ));
    }

    #[test]
    fn rejects_surrounding_whitespace() {
        assert!(matches!(
            validate_name(" zorg "),
            Err(AppError::InvalidName)
        ));
    }
}
