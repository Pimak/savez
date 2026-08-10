-- D-12: `users.role` conditionne l'accès à des routes sensibles (modération, admin) -- contrairement
-- à `verified_via` (convention de commentaire depuis la Phase 3, jamais vérifiée en base), une valeur
-- de rôle inattendue aurait un impact sécuritaire direct (escalade de privilège potentielle si un
-- code applicatif futur devait faire confiance à une valeur non prévue). La contrainte CHECK est la
-- source de vérité primaire (T-07-12) ; le `match` applicatif de `src/auth/cache.rs` n'est qu'une
-- défense en profondeur avec repli au moindre privilège. La table est à un chiffre de lignes (et le
-- reste à l'échelle du projet, CON-ops-cost) -- un simple ADD CONSTRAINT suffit, pas besoin de
-- NOT VALID + VALIDATE CONSTRAINT.
ALTER TABLE users
    ADD CONSTRAINT users_role_check CHECK (role IN ('user', 'moderator', 'admin'));

-- Pitfall 2 (07-RESEARCH.md) : PostgreSQL ne crée PAS d'index automatiquement pour une clé étrangère
-- (contrairement à MySQL/InnoDB). `user_bans.user_id` n'a qu'une contrainte FOREIGN KEY, aucun index
-- -- chaque miss du cache d'authentification (AuthCache, TTL ~10s par utilisateur actif) exécute un
-- `EXISTS (SELECT 1 FROM user_bans WHERE user_id = $1 ...)`, un chemin chaud qui serait sinon un
-- Seq Scan. Index partiel sur les bans actifs uniquement (lifted_at IS NULL), la seule forme que ce
-- chemin chaud interroge jamais.
CREATE INDEX idx_user_bans_active ON user_bans (user_id) WHERE lifted_at IS NULL;

-- `puzzle_reports`'s only non-PK index is `UNIQUE(user_id, puzzle_id)`, whose leading column is
-- `user_id`, not `puzzle_id`. Cette phase filtre par `puzzle_id` d'abord à deux endroits sur le
-- chemin chaud : le comptage du masquage automatique (D-05, à chaque signalement) et la résolution
-- groupée par signalement/motif (D-08).
CREATE INDEX idx_puzzle_reports_puzzle_status ON puzzle_reports (puzzle_id, status);
