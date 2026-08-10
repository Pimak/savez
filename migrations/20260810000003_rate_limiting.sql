-- D-01: le rate limiting vit en base, sur deux tables séparées -- une table de configuration des
-- seuils (peu de lignes, modifiable par la future CLI admin, plan 07-09) et une table
-- d'événements horodatés (une ligne écrite par requête acceptée), et non un compteur en mémoire
-- (`governor`/`tower-governor` explicitement écartés, T-07-SC) : les seuils et les compteurs
-- doivent survivre à un redémarrage du serveur.
--
-- D-02: `route_class` distingue `read`/`write`, avec des seuils bien plus larges côté lecture.
-- La contrainte d'unicité ci-dessous porte sur la paire (classe, fenêtre) et non sur la seule
-- classe : plusieurs fenêtres (horaire ET journalière) coexistent pour une même classe, et c'est
-- la plus contraignante des lignes qui l'emporte -- voir `src/ratelimit.rs::check_and_record`.
--
-- D-03: seule cette table de seuils numériques est modifiable par la CLI admin et persistée en
-- base -- la correspondance route→classe reste du code figé dans `src/ratelimit.rs`, jamais une
-- variable d'environnement ni une ligne de configuration.
--
-- Semer les valeurs par défaut par migration suit le précédent `20260808000001_seed_first_admin.sql`
-- (une ligne INSERTée dès la migration, pas au premier lancement d'une commande CLI) : le serveur
-- doit être utilisable dès son premier démarrage, avant qu'un admin n'ait jamais lancé
-- `savez mod ratelimit set`.
CREATE TABLE rate_limit_config (
    id             INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    route_class    TEXT NOT NULL CHECK (route_class IN ('read', 'write')),
    window_seconds INTEGER NOT NULL CHECK (window_seconds > 0),
    limit_count    INTEGER NOT NULL CHECK (limit_count >= 0),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (route_class, window_seconds)
);

CREATE TABLE rate_limit_events (
    id          INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id     UUID NOT NULL REFERENCES users(id),
    route_class TEXT NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_rate_limit_events_lookup
    ON rate_limit_events (user_id, route_class, occurred_at);

INSERT INTO rate_limit_config (route_class, window_seconds, limit_count) VALUES
    ('write', 3600, 5),
    ('write', 86400, 20),
    ('read',  3600, 500)
ON CONFLICT (route_class, window_seconds) DO NOTHING;
