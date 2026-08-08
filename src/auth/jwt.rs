use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 30 jours, en secondes. D-06 : échelle semaines/mois, aucun mécanisme de refresh — à
/// expiration le mod refait un login oracle complet. Constante nommée, jamais un littéral
/// inline, pour que la Phase 7 puisse l'ajuster sans re-dériver le raisonnement.
pub const DEFAULT_LIFETIME_SECS: u64 = 2_592_000;

/// Le JWT vit 30 jours sans refresh, donc toute donnée d'autorisation mutable (`role`,
/// `banned`, `verified_via`) embarquée resterait valide pendant 30 jours après un bannissement
/// ou une rétrogradation. La Phase 7 relira l'état courant en base à partir de `sub` — elle
/// n'ajoutera PAS de champ ici sans rouvrir D-06.
#[derive(Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    pub exp: usize,
}

/// Émet un JWT HS256 signé avec `jwt_key`, contenant uniquement `sub` et `exp`.
pub fn issue(
    jwt_key: &str,
    user_id: Uuid,
    lifetime_secs: u64,
) -> Result<String, jsonwebtoken::errors::Error> {
    let claims = Claims {
        sub: user_id,
        exp: (jsonwebtoken::get_current_timestamp() + lifetime_secs) as usize,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(jwt_key.as_bytes()),
    )
}

// STUB (RED phase): validate() intentionally leaves two safety-critical gaps open so the
// negative-path tests below fail against this version — validate_exp is disabled, and the
// accepted algorithm set is broadened to include HS512. Both are fixed in the GREEN commit.
/// Valide un JWT HS256 signé avec `jwt_key`.
pub fn validate(jwt_key: &str, token: &str) -> Result<Claims, jsonwebtoken::errors::Error> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = false;
    validation.algorithms = vec![Algorithm::HS256, Algorithm::HS512];
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(jwt_key.as_bytes()),
        &validation,
    )
    .map(|data| data.claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_then_validate_round_trip() {
        let token = issue("test-key", Uuid::nil(), DEFAULT_LIFETIME_SECS).unwrap();
        let claims = validate("test-key", &token).unwrap();
        assert_eq!(claims.sub, Uuid::nil());
    }

    #[test]
    fn rejects_token_signed_with_another_key() {
        let token = issue("key-a", Uuid::nil(), DEFAULT_LIFETIME_SECS).unwrap();
        assert!(validate("key-b", &token).is_err());
    }

    #[test]
    fn rejects_expired_token() {
        let claims = Claims {
            sub: Uuid::nil(),
            exp: (jsonwebtoken::get_current_timestamp() - 10_000) as usize,
        };
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(b"test-key"),
        )
        .unwrap();
        assert!(validate("test-key", &token).is_err());
    }

    #[test]
    fn rejects_algorithm_confusion() {
        let claims = Claims {
            sub: Uuid::nil(),
            exp: (jsonwebtoken::get_current_timestamp() + DEFAULT_LIFETIME_SECS) as usize,
        };
        let token = encode(
            &Header::new(Algorithm::HS512),
            &claims,
            &EncodingKey::from_secret(b"test-key"),
        )
        .unwrap();
        assert!(validate("test-key", &token).is_err());
    }

    #[test]
    fn claims_carry_only_sub_and_exp() {
        let claims = Claims {
            sub: Uuid::nil(),
            exp: 0,
        };
        let value = serde_json::to_value(&claims).unwrap();
        let obj = value.as_object().unwrap();
        assert_eq!(obj.len(), 2);
        assert!(obj.contains_key("sub"));
        assert!(obj.contains_key("exp"));
    }
}
