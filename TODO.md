# TODO — Savez (serveur puzzle communautaire shapez 1)

Basé sur le cahier des charges du 6 août 2026. Deux chantiers : **Backend Rust (AGPL-3.0)** puis **Mod client JS (GPLv3)**. Le chantier 2 démarre après l'étape 5 du backend.

Légende : `[ ]` à faire · `[x]` fait

---

## Étape 0 — Fondations du projet

- [ ] Renommer/configurer le crate (`savez`, conventions Rust : nom en minuscules)
- [ ] Ajouter `LICENSE` (AGPL-3.0) ; si code repris de gatez-backend : `LICENSE-MIT` (copyright Armando Neto) + section « Licence » dans le README expliquant l'empilement
- [ ] README initial : objectifs, non-objectifs (pas de scraping, pas de concurrence au service officiel), statut du projet
- [ ] Configurer `cargo clippy` + CI GitHub Actions (build + tests + clippy)
- [ ] Premier commit git (le dépôt est initialisé mais vide)

## Étape 1 — Squelette HTTP

- [ ] Dépendances : `axum`, `tokio`, `serde`, `tracing`
- [ ] Serveur Axum écoutant sur `:15001` (port attendu par le client officiel en mode dev)
- [ ] `GET /v1/puzzles/list/new` renvoyant un JSON en dur
- [ ] Middleware CORS (client Electron/navigateur dev)
- [ ] Endpoint `/healthz`
- [ ] Configuration par variables d'environnement (`DATABASE_URL`, `JWT_KEY`, `OFFICIAL_API_URL`, port)
- [ ] **Critère de fin : réponse JSON servie sur :15001**

## Étape 2 — Persistance (SQLx + PostgreSQL)

- [ ] Dépendance `sqlx` (PostgreSQL), migrations appliquées automatiquement au démarrage
- [ ] Migration initiale — tables :
  - [ ] `users` (id uuid, name unique, email optionnel, password_hash nullable, verified_via, steam_id, role, created_at)
  - [ ] `puzzles` (id autoincrement, short_key unique, title, author_id, data JSON **décompressé**, likes, downloads, completions, difficulty, average_time, locale, hidden_at/hidden_by, created_at)
  - [ ] `puzzle_completions` (user_id + puzzle_id uniques ensemble, time_taken, liked, completed_at)
  - [ ] `puzzle_reports` (user_id, puzzle_id, reason, status, reviewed_at/reviewer_id, review_notes, created_at ; unicité (user_id, puzzle_id))
  - [ ] `user_bans` (user_id, reason, moderator_id, expires_at nullable, lifted_at/lift_reason/lift_moderator_id, created_at)
  - [ ] `moderation_log` (append-only : moderator_id, action, target_type/target_id, details JSON, created_at)
- [ ] `GET /v1/puzzles/list/:category` persistant (`new` | `top-rated` | `mine`)
- [ ] `GET /v1/puzzles/download/:idOrShortKey` (id numérique ou shortKey, incrémente `downloads`)
- [ ] `POST /v1/puzzles/submit`
- [ ] Types `PuzzleMetadata` et `PuzzleGameData` conformes à `savegame_typedefs.js`
- [ ] **Critère de fin : CRUD vérifié via tests d'intégration**

## Étape 3 — Interop compression lz-string (double format officiel/CE)

- [ ] Dépendance `lz-str` — décompression du champ `data` à la soumission (variante EncodedURIComponent, compatible `compressX64` du client officiel)
- [ ] Détection automatique de format à la soumission : le client officiel compresse `data` (`compressX64`), la Community Edition envoie du JSON brut non compressé (bug assumé côté CE, cf. commentaire `FIXME` dans son `api.js`) — tenter `JSON.parse` direct, puis décompression lz-string en repli. Les deux formats sont des cibles permanentes, pas un mode de secours
- [ ] Stockage décompressé, servi tel quel au download (aucun client ne décompresse à la lecture — pas de divergence de ce côté)
- [ ] **Critère de fin : deux tests unitaires d'interop passent — un contre une chaîne compressée produite par le client officiel réel, un contre une chaîne JSON brute produite par la Community Edition réelle**

## Étape 4 — Authentification

- [ ] Dépendances : `jsonwebtoken`, `argon2`, `reqwest`
- [ ] `POST /v1/public/login` : réception du token officiel, vérification oracle (1 appel `GET /v1/puzzles/list/mine` vers `api.shapez.io` ; HTTP 200 ⇒ possession du DLC)
- [ ] Création/rattachement de compte `verified_via = "official-api"`, émission d'un JWT propre
- [ ] Middleware d'auth par header `x-token` (ignorer `x-api-key`)
- [ ] Préparer la bascule phase 2 par configuration : inscription libre (`open`) et/ou Steam OpenID (`steam-openid`)
- [ ] Seed/CLI de création du premier `admin` (jamais via l'API publique)
- [ ] **Critère de fin : compte créé via token officiel valide ; refus sinon**

## Étape 5 — Contrat shapez complet

- [ ] Toutes les réponses en **HTTP 200**, y compris les erreurs métier ; format `{ "error": "<code>" }` avec les codes de `T.backendErrors` (`not-found`, `bad-payload`, `short-key-already-taken`, `profane-title`, `no-emitters`, `no-goals`, `bad-building-placement`, …)
- [ ] `POST /v1/puzzles/search` (`{ searchTerm, difficulty, duration }`)
- [ ] `POST /v1/puzzles/complete/:id` (`{ time, liked }`)
- [ ] `POST /v1/puzzles/report/:id` (`{ reason }` ∈ `profane` | `unsolvable` | `trolling`)
- [ ] `POST /v1/puzzles/delete/:id` (POST, pas DELETE ; auteur uniquement)
- [ ] Validation à la soumission : ≥ 1 émetteur et ≥ 1 objectif, clés de forme valides, shortKey unique et bien formé, titre de longueur correcte, placements dans les bounds
- [ ] Champ `completed` de `PuzzleMetadata` relatif à l'utilisateur courant
- [ ] **Critère de fin : le client officiel en mode dev affiche et joue les puzzles du serveur**

## Étape 6 — Logique métier + modération

### Logique métier
- [ ] Compteurs : `downloads` au download ; `completions` et `likes` à la complétion
- [ ] `average_time` = moyenne des `time_taken`
- [ ] `difficulty` = ratio complétions/téléchargements (référence : gatez-backend ; formule propre documentée)
- [ ] Tri `top-rated` par likes, départagé par complétions

### Modération automatique
- [ ] Filtre de vocabulaire sur le titre (`profane-title`), liste configurable, EN/FR minimum
- [ ] Rate limiting par utilisateur (configurable, ex. 5/h, 20/j ; erreur `bad-payload`)

### Flux de signalement
- [ ] Interdictions : signaler son propre puzzle (`can-not-report-your-own-puzzle`), signaler deux fois
- [ ] File de signalements `status = pending`
- [ ] Masquage automatique préventif à N signalements pending d'utilisateurs distincts (défaut 3, `hidden_by = NULL`)
- [ ] Masquage : retiré des listes/recherches, visible par l'auteur et les modérateurs

### Routes de modération (middleware de rôle user/moderator/admin)
- [ ] `GET /v1/moderation/reports?status=pending` (moderator, paginé)
- [ ] `POST /v1/moderation/reports/:id/resolve` (`{ status: "upheld"|"rejected", notes }`)
- [ ] `POST /v1/moderation/puzzles/:id/hide` / `unhide` (moderator)
- [ ] `DELETE /v1/moderation/puzzles/:id` (admin ; libère le shortKey)
- [ ] `POST /v1/moderation/users/:id/ban` (temp : moderator ; perm : admin)
- [ ] `POST /v1/moderation/users/:id/lift-ban` (admin, motivé)
- [ ] `GET /v1/moderation/log` (admin, paginé)
- [ ] Bans : bloquent login, soumission, complétion, signalement (erreur dédiée)
- [ ] Compteur de signalements `upheld` par auteur affiché au modérateur (pas de ban automatique)

### CLI admin + audit
- [ ] CLI d'administration `clap` (sous-commandes du binaire, ex. `savez mod reports`) consommant les routes de modération
- [ ] Toute action de modération journalisée dans `moderation_log` (append-only)
- [ ] Erreurs typées avec `thiserror`
- [ ] **Critère de fin : parité fonctionnelle avec le service officiel ; scénario signalement → revue → sanction testé**

## Étape 7 — Déploiement (chantier 1, exploitation)

- [x] Build CI : cible `x86_64-unknown-linux-musl` (binaire musl statique vers image distroless), publication GHCR (`ghcr.io/pimak/savez`) à chaque tag `v*` (`.github/workflows/release.yml`)
- [x] Dossier `deploy/` versionné : fichier Docker Compose de production (app + PostgreSQL + Caddy + Uptime Kuma), Caddyfile, script d'installation (`install.sh`, VPS Debian nu → machine opérationnelle), script de sauvegarde (`backup.sh`), unités systemd du minuteur de sauvegarde (`savez-backup.service`/`.timer` — aucune unité systemd pour le binaire applicatif lui-même, qui tourne comme service Compose), doc de restauration (`RESTORE.md`)
- [x] Sauvegarde `pg_dump` quotidien vers stockage objet S3-compatible (rétention 30 jours), via `rclone` générique compatible tout fournisseur (aucun fournisseur en dur dans le script)
- [x] `Dockerfile` multi-étages (musl builder → distroless non-root) — mécanisme de build principal de l'image publiée, plus une simple commodité optionnelle
- [x] Supervision : UptimeRobot (externe, détecteur principal de panne totale) + Uptime Kuma auto-hébergé (dashboard interne) sur `/healthz` ; logs `tracing` → stdout, lus via `docker compose logs app`
- [ ] Provisionner un VPS réel (~4-6 €/mois) + acheter un nom de domaine (sans « shapez » dans le nom) + choisir un fournisseur de stockage S3-compatible + exécuter le test de reprise <1h en conditions réelles — checkpoint explicite ouvert, à lever avant la Phase 12

## Chantier 2 — Mod client (après l'étape 5)

### M1 — Socle
- [ ] Mod chargeable par le système `MODS` natif (shapez standalone ≥ 1.5, pas de fork)
- [ ] Rendre l'endpoint de `ClientAPI` injectable par constructeur (sans changer le comportement par défaut)
- [ ] Second `ClientAPI` pointé sur le serveur communautaire, sessions/tokens indépendants
- [ ] Inscription/login communautaire depuis le jeu (transmission du token officiel pour l'oracle)
- [ ] Pré-filtre : fonctionnalités visibles seulement si `app.platformWrapper.dlcs.puzzle`

### M2 — Export
- [ ] Bouton dans l'onglet « My puzzles » (`puzzle_menu.js`) : `apiListPuzzles("mine")` → `apiDownloadPuzzle(id)` → `communityApi.apiSubmitPuzzle(...)`
- [ ] Débit : séquentiel, pause ≥ 1 s entre requêtes, reprise sur erreur
- [ ] Idempotence via `shortKey` (déjà présent ⇒ ignoré)
- [ ] Rapport de fin (exportés / ignorés / échoués)

### M3 — Navigation
- [ ] Consulter/jouer les puzzles du serveur communautaire (réutilisation de l'UI existante sur `communityApi`)

### M4 — Publication
- [ ] Publication mod.io / GitHub Releases
- [ ] Documentation utilisateur (installation, inscription, export)

---

## Points ouverts / risques à suivre

- [ ] Réponse de tobspr (fallback : contenu neuf uniquement, inscription libre dès la phase 1)
- [ ] Formule exacte difficulté/top-rated inconnue → formule propre documentée
- [ ] Extinction possible de `api.shapez.io` avant lancement → bascule directe en phase 2
- [ ] Démarrage à froid du catalogue → export créateurs (M2) + communication (Discord shapez, CE)
