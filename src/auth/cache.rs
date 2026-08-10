//! Rôle et état de ban d'un utilisateur, lus derrière un cache TTL court (D-09, ADR 0006) plutôt
//! qu'à chaque requête protégée. Le JWT reste réduit à `sub`/`exp` (D-06, Phase 5) : ni le rôle ni
//! l'état de ban ne sont jamais des claims du token, toujours relus en base ici, avec au plus une
//! lecture base par fenêtre de TTL et par utilisateur.
//!
//! D-10 : aucune invalidation ciblée du cache n'existe. Un ban ou une rétrogradation prend jusqu'à
//! `AUTH_CACHE_TTL` avant d'être visible d'une requête déjà en cache — fenêtre de fraîcheur résiduelle
//! explicitement acceptée par ADR 0006, à ne pas « corriger » sans nouvelle décision utilisateur.
//!
//! `moka` est choisi pour sa coalescence de requêtes : la méthode de lecture-ou-calcul du cache
//! utilisée plus bas ne déclenche qu'un seul chargement base pour N appels concurrents sur la même
//! clé expirée (pas de troupeau tonnerre de lectures dupliquées), propriété non triviale à
//! reproduire avec un `Mutex<HashMap>` fait main.

use sqlx::PgPool;
use uuid::Uuid;

/// Ordre cumulatif des rôles : `User < Moderator < Admin`. L'`Ord` dérivé se base sur l'ordre de
/// déclaration des variants — ne JAMAIS réordonner (T-07-13, couvert par le test
/// `role_order_is_cumulative` ci-dessous).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Role {
    User,
    Moderator,
    Admin,
}

/// État de rôle/ban d'un utilisateur tel que renvoyé par le cache. D-11 : `banned` dérive de
/// l'existence d'au moins une ligne `user_bans` active (`lifted_at IS NULL` et `expires_at` nul ou
/// futur) — la coexistence de plusieurs bans est possible et sans incidence sur ce booléen.
#[derive(Clone, Copy, Debug)]
pub struct CachedAuthState {
    pub role: Role,
    pub banned: bool,
}

/// Erreur interne, `Copy`, dédiée à `AuthCache::get`. Le cache `moka` renvoie ses erreurs
/// enveloppées dans un `Arc` (nécessaire à sa coalescence de requêtes : l'erreur doit être
/// partageable entre tous les appelants en attente de la même future en vol) — `AppError`
/// n'implémente pas `Clone` (elle enveloppe `sqlx::Error`, qui ne l'implémente pas non plus), donc
/// un `Arc<AppError>` ne se ramènerait pas à un `AppError` possédé sans perte d'information ni une
/// déballe fragile côté `Arc`. Ce type dédié, `Copy`, se déréférence trivialement (`*arc`) sans
/// aucune des deux limitations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheFetchError {
    UnknownUser,
    Database,
}

/// Cache TTL enveloppant `moka::future::Cache<Uuid, CachedAuthState>`. `#[derive(Clone)]` : moka
/// est interne à `Arc`, donc bon marché à cloner — respecte la convention d'`AppState` (« tous les
/// champs sont bon marché à cloner »).
#[derive(Clone)]
pub struct AuthCache {
    inner: moka::future::Cache<Uuid, CachedAuthState>,
}

impl AuthCache {
    pub fn new(ttl: std::time::Duration) -> Self {
        Self {
            inner: moka::future::Cache::builder().time_to_live(ttl).build(),
        }
    }

    /// Lit le rôle et l'état de ban de `user_id`, au plus une fois par fenêtre de TTL (D-09).
    /// `.map_err(|arc| *arc)` est possible parce que `CacheFetchError` est `Copy` — voir la doc de
    /// ce type.
    pub async fn get(
        &self,
        pool: &PgPool,
        user_id: Uuid,
    ) -> Result<CachedAuthState, CacheFetchError> {
        self.inner
            .try_get_with(user_id, fetch_role_and_ban(pool, user_id))
            .await
            .map_err(|arc| *arc)
    }
}

/// Lecture base du rôle et de l'état de ban courant d'un utilisateur (D-11). Ligne absente (un JWT
/// valide pour un compte supprimé depuis — défense en profondeur) ⇒ `CacheFetchError::UnknownUser`.
/// Erreur sqlx ⇒ journalisée ici (le détail `sqlx::Error` ne survivrait pas à la réduction vers
/// `CacheFetchError::Database`, jamais rien perdre côté serveur) puis réduite à `Database`.
async fn fetch_role_and_ban(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<CachedAuthState, CacheFetchError> {
    let row = sqlx::query!(
        r#"
        SELECT role,
               EXISTS (
                   SELECT 1 FROM user_bans
                   WHERE user_id = $1 AND lifted_at IS NULL
                     AND (expires_at IS NULL OR expires_at > now())
               ) AS "banned!"
        FROM users WHERE id = $1
        "#,
        user_id
    )
    .fetch_optional(pool)
    .await
    .map_err(|err| {
        tracing::error!(error = %err, %user_id, "failed to fetch role/ban state");
        CacheFetchError::Database
    })?
    .ok_or(CacheFetchError::UnknownUser)?;

    let role = match row.role.as_str() {
        "user" => Role::User,
        "moderator" => Role::Moderator,
        "admin" => Role::Admin,
        // La contrainte CHECK de la migration 20260810000002 (T-07-12) rend ce bras inatteignable
        // par une écriture normale -- mais le code doit rester total et sans panique (jamais de
        // déballe optimiste ni d'arrêt fatal explicite, discipline d'AppError::code). Repli au
        // moindre privilège, journalisé pour qu'un tel cas ne passe jamais inaperçu.
        other => {
            tracing::error!(role = other, %user_id, "unexpected role value, defaulting to User");
            Role::User
        }
    };

    Ok(CachedAuthState {
        role,
        banned: row.banned,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T-07-13 : l'ordre de déclaration des variants EST l'ordre de comparaison. Ce test
    /// échouerait immédiatement en cas de réordonnancement accidentel des variants de `Role`.
    #[test]
    fn role_order_is_cumulative() {
        assert!(Role::User < Role::Moderator);
        assert!(Role::Moderator < Role::Admin);
        assert!(Role::User < Role::Admin);
    }

    /// Un admin cumule les droits d'un modérateur : `Role::Admin >= Role::Moderator` est vrai.
    #[test]
    fn admin_role_satisfies_moderator_threshold() {
        assert!(Role::Admin >= Role::Moderator);
        assert!(Role::Admin >= Role::User);
    }
}
