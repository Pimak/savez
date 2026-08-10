# Cahier des charges — Serveur puzzle communautaire pour shapez 1

**Backend Rust auto-hébergé & mod client — objectif conservation**

| | |
|---|---|
| **Auteur** | Maxime Mainguet ([Pimak](https://github.com/Pimak)) |
| **Date** | 6 août 2026 |
| **Statut** | Draft — suppose l'accord de tobspr |

> Ce document suppose l'accord de tobspr sur les trois principes du mail du 6 août 2026 : validation de possession via l'API officielle, export par les créateurs de leurs propres puzzles uniquement, coopération future sur la préservation du catalogue.

## 1. Contexte et objectifs

shapez 1 est un jeu open source (GPLv3) dont le mode puzzle repose sur un backend fermé (`api.shapez.io`), opéré par tobspr Games et réservé aux possesseurs du DLC Puzzle (vérification Steam). Le jeu n'est plus activement maintenu (développement passé à shapez 2) ; le service officiel pourrait fermer à terme.

**Objectif principal :** pérenniser l'expérience puzzle communautaire de shapez 1 via un backend auto-hébergé, open source, indépendant de l'infrastructure officielle.

**Objectifs secondaires :**

- Offrir aux créateurs un outil de **portabilité de leurs propres puzzles** depuis le service officiel.
- Ne **pas nuire au modèle commercial** du DLC tant que le service officiel est en vie (accès réservé aux possesseurs du DLC).
- Servir de **projet d'apprentissage Rust** pour l'auteur.

**Non-objectifs :**

- Copier le catalogue communautaire officiel (aucun scraping de contenu tiers).
- Se substituer au service officiel tant qu'il fonctionne.
- Version web du client (alignement sur la Community Edition : standalone uniquement).

## 2. Périmètre — deux chantiers

| # | Chantier | Priorité | Langage | Licence |
|---|----------|----------|---------|---------|
| 1 | **Backend** serveur puzzle | 1 (débloque le chantier 2) | Rust | AGPL-3.0 |
| 2 | **Mod** client (export + connexion double backend) | 2 | JavaScript | GPLv3 |

Le backend est développé et testé en premier contre le client officiel en mode dev (qui pointe déjà sur `http://localhost:15001`, cf. `src/js/platform/api.js:27`).

## 3. Contraintes légales et éthiques

### 3.1 Licences

- **Backend : AGPL-3.0.** Programme séparé communiquant par API réseau → non soumis à la GPLv3 du jeu. L'AGPL garantit que tout hébergeur d'une version modifiée publie ses sources (cohérent avec l'objectif de pérennité).
  - Si du code est repris de **gatez-backend** (MIT) : conserver le fichier de licence MIT original (`LICENSE-MIT`) avec le copyright d'Armando Neto, `LICENSE` = AGPL-3.0, section « Licence » dans le README expliquant l'empilement.
- **Mod : GPLv3.** Tourne dans le processus du jeu, importe ses classes → œuvre dérivée du client GPL. Sources publiées.

### 3.2 Contenu et service officiel

- Les puzzles sont des contenus de leurs auteurs : **seul l'auteur authentifié exporte ses propres créations** (catégorie `mine` de l'API officielle).
- Les statistiques officielles (likes, complétions, difficulté calculée) ne sont **pas importées** — remises à zéro sur le nouveau serveur.
- Usage du service officiel limité à : 1 appel de vérification par inscription + export au fil de l'eau par les créateurs (1 requête à la fois, pause entre chaque).
- La marque « shapez » n'est pas utilisée dans le nom du projet/serveur d'une façon laissant croire à un service officiel.

## 4. Spécifications du backend

### 4.1 Stack technique

| Composant | Choix | Rôle |
|-----------|-------|------|
| Langage | Rust (édition stable courante) | Binaire statique, faible empreinte, pérennité |
| Framework HTTP | Axum + Tokio | Handlers, extracteurs, middlewares |
| Base de données | SQLite via SQLx | Fichier unique (sauvegarde triviale), requêtes vérifiées à la compilation |
| Auth | `jsonwebtoken` (JWT) + `argon2` | Sessions et mots de passe |
| Interop compression | `lz-str` | Compatibilité avec le `compressX64` du client (lz-string, variante EncodedURIComponent) |
| Client HTTP sortant | `reqwest` | Appel de vérification vers `api.shapez.io` |
| Qualité | `cargo clippy`, `thiserror` | Lint, erreurs typées à terme |

**Cible de déploiement :** un binaire unique sur petit VPS Linux ; configuration par variables d'environnement (`DATABASE_URL`, `JWT_KEY`, `OFFICIAL_API_URL`, port).

### 4.2 Contrat d'API — compatibilité client shapez

Le backend implémente le contrat exact attendu par `ClientAPI` (`src/js/platform/api.js`) :

#### Conventions transversales

- Toutes les réponses en **HTTP 200** pour les erreurs **MÉTIER** (le client rejette tout statut ≠ 200 par un rejet brut `bad-status: …`). Exception explicite : une panne d'**infrastructure** (`AppError::Database` — perte de connexion, épuisement du pool) reste un 5xx littéral, corps vide, jamais repliée dans la taxonomie métier — voir `docs/adr/0003-all-200-error-taxonomy.md`.
- Erreurs au format `{ "error": "<code>" }`. Les **21 clés réelles** de `T.backendErrors` (`translations/base-en.yaml`, identiques dans les deux clients réels) : `ratelimit`, `invalid-api-key`, `unauthorized`, `bad-token`, `bad-id`, `not-found`, `bad-category`, `bad-short-key`, `profane-title`, `bad-title-too-many-spaces`, `bad-shape-key-in-emitter`, `bad-shape-key-in-goal`, `no-emitters`, `no-goals`, `short-key-already-taken`, `can-not-report-your-own-puzzle`, `bad-payload`, `bad-building-placement`, `timeout`, `too-many-likes-already`, `no-permission`. Trois codes propres au projet, sans équivalent en amont (le client affiche alors la chaîne brute reçue, comportement natif de `_request()`, pas un cas spécial à coder) : `name-already-taken`, `auth-mode-not-implemented`, `internal-error` — voir `docs/adr/0003-all-200-error-taxonomy.md`.
- Auth par header **`x-token`** (pas de `Authorization: Bearer`). Le header `x-api-key` envoyé par le client est ignoré. **Toutes** les routes `/v1/puzzles/*` exigent un `x-token` valide ; seules `/v1/public/login` et `/healthz` sont publiques — voir `docs/adr/0001-auth-required-browsing.md`.
- CORS activé (le client tourne en Electron/navigateur dev).

#### Endpoints

| Méthode | Route | Corps / paramètres | Réponse | Auth (`x-token`) |
|---------|-------|--------------------|---------|-------------------|
| POST | `/v1/public/login` | `{ token }` (phase 1 : token officiel — cf. 4.4) ou identifiants propres | `{ token }` (JWT du serveur) | publique |
| GET | `/v1/puzzles/list/:category` | `category` ∈ `new` \| `top-rated` \| `mine` | `PuzzleMetadata[]` | requise |
| POST | `/v1/puzzles/search` | `{ searchTerm, difficulty, duration }` | `PuzzleMetadata[]` | requise |
| GET | `/v1/puzzles/download/:idOrShortKey` | id numérique ou shortKey | `{ meta, game }` (`game` **décompressé**) | requise |
| POST | `/v1/puzzles/submit` | `{ title, shortKey, data }` (`data` compressé lz-string ou JSON brut — cf. « Double format de `data` » ci-dessous) | le `PuzzleMetadata` **créé** (le client n'inspecte que l'absence d'une clé `error`, jamais `{ success: true }` — comportement inchangé depuis les Phases 3-5) | requise |
| POST | `/v1/puzzles/complete/:id` | `{ time, liked }` | `{ success: true }` | requise |
| POST | `/v1/puzzles/report/:id` | `{ reason }` ∈ `profane` \| `unsolvable` \| `trolling` | `{ success: true }` | requise |
| POST | `/v1/puzzles/delete/:id` | — (POST, pas DELETE) | `{ success: true }` | requise |

`/healthz` (hors contrat `ClientAPI`, supervision uniquement — cf. 6.2) est également publique. Voir `docs/adr/0001-auth-required-browsing.md` pour la justification de l'authentification désormais obligatoire sur `list`/`download`/`search`, qui casse volontairement l'accès anonyme dont ces routes bénéficiaient depuis les Phases 3/4.

#### Types de données

Cf. `src/js/savegame/savegame_typedefs.js` :

- `PuzzleMetadata` : `id`, `shortKey`, `likes`, `downloads`, `completions`, `difficulty` (nullable), `averageTime` (nullable), `title`, `author`, `completed` (bool, relatif à l'utilisateur courant).
- `PuzzleGameData` : `version`, `bounds {w, h}`, `buildings[]` (types `emitter` / `goal` / `block`, avec `item` et `pos {x, y, r}`).

**Validation à la soumission**, chaque règle assortie de son code d'erreur exact :

- au moins un émetteur (`no-emitters`) ;
- au moins un objectif (`no-goals`) ;
- l'`item` de chaque émetteur/objectif est soit un littéral de couleur (`red`, `green`, `blue`, `yellow`, `purple`, `cyan`, `white`, `uncolored`, comparaison insensible à la casse), soit une clé de forme valide (`bad-shape-key-in-emitter` / `bad-shape-key-in-goal`) — **une clé de forme ne porte aucun préfixe de type** : l'exemple `shape:CuCuCuCu` qui a circulé dans le projet était **erroné**, la forme correcte est `CuCuCuCu` ;
- `shortKey` grammaticalement valide selon la même grammaire de forme (`bad-short-key`) et non déjà pris (`short-key-already-taken`) ;
- titre de 4 à 20 caractères une fois trimé, alphabet `[a-zA-Z0-9_- ]` (`bad-title-too-many-spaces`), hors liste de grossièretés par correspondance de token exact (`profane-title`) ;
- tout bâtiment (émetteur, objectif, bloc) dans les bornes de `bounds`, et jamais deux bâtiments sur la même case (`bad-building-placement`).

**Double format de `data` à la soumission.** Comparaison directe des deux `ClientAPI` réels (2026-08-08, `src/js/platform/api.js`) : le client officiel compresse `data` en lz-string avant envoi (`compressX64(JSON.stringify(payload.data))`), tandis que la Community Edition envoie du JSON brut non compressé — un bug assumé côté CE, signalé par son propre commentaire `// FIXME: Server expects lzstring compressed payload`. Ce ne sont pas deux modes exclusifs à choisir au déploiement : les deux clients coexistent en usage réel, donc `/v1/puzzles/submit` doit **détecter le format automatiquement** (tentative de `JSON.parse` direct, puis décompression `lz-str`/`compressX64` en repli) plutôt que d'en supposer un seul. Aucune divergence en sens inverse : ni l'un ni l'autre client ne décompresse quoi que ce soit à la lecture, donc `GET /v1/puzzles/download/*` continue de servir `game` décompressé sans changement (cf. 4.3, « Décision »).

### 4.3 Modèle de données

Transposition du schéma Prisma de gatez-backend, épuré des champs logic-gates (`minimumComponents`, `minimumNands`…), enrichi pour la double authentification :

- **`users`** : `id` (uuid), `name` (unique), `email` (unique, optionnel), `password_hash` (nullable en phase 1 si compte purement oracle), **`verified_via`** (`official-api` | `open` | `steam-openid`), `steam_id` (nullable), `role` (user/moderator/admin), `created_at`.
- **`puzzles`** : `id` (autoincrement), `short_key` (unique), `title`, `author_id`, `data` (JSON du `PuzzleGameData`, stocké **décompressé**), `likes`, `downloads`, `completions`, `difficulty` (float nullable), `average_time` (float nullable), `locale`, `hidden_at`/`hidden_by` (modération), `created_at`.
- **`puzzle_completions`** : `user_id` + `puzzle_id` (unique ensemble), `time_taken`, `liked`, `completed_at`.
- **`puzzle_reports`** : `user_id`, `puzzle_id`, `reason`, `status` (`pending` | `upheld` | `rejected`), `reviewed_at`/`reviewer_id`, `review_notes`, `created_at`. Unicité `(user_id, puzzle_id)` — un seul signalement actif par utilisateur et par puzzle.
- **`user_bans`** : reprise du modèle gatez — `user_id`, `reason`, `moderator_id`, `expires_at` (nullable = permanent), `lifted_at`/`lift_reason`/`lift_moderator_id`, `created_at`.
- **`moderation_log`** : journal d'audit — `id`, `moderator_id`, `action` (`hide_puzzle`, `unhide_puzzle`, `delete_puzzle`, `ban_user`, `lift_ban`, `resolve_report`, `promote_user`…), `target_type`/`target_id`, `details` (JSON), `created_at`. Table en append-only.

Décision : `data` est décompressé **à la soumission** (validation immédiate) et servi tel quel au download.

### 4.4 Authentification — deux phases

#### Phase 1 — API officielle en vie (« oracle »)

1. Le mod envoie au backend le token de session officiel de l'utilisateur (`app.clientApi.token`).
2. Le backend effectue **un appel** vers `api.shapez.io` avec ce token (ex. `GET /v1/puzzles/list/mine`). HTTP 200 ⇒ l'utilisateur possède le DLC.
3. Création/rattachement du compte avec `verified_via = "official-api"`, émission d'un JWT propre au serveur. Aucune revalidation par requête (revalidation optionnelle à intervalle long).

#### Phase 2 — après extinction du service officiel

- Les comptes existants sont conservés tels quels (le champ `verified_via` porte l'historique).
- Les nouvelles inscriptions basculent, par simple configuration, vers l'inscription libre (`open`) et/ou Steam OpenID (`steam-openid` — preuve d'identité Steam, la possession du DLC n'étant plus vérifiable ni pertinente).

> Contrainte explicite : la validation Steamworks directe (`CheckAppOwnership`, `AuthenticateUserTicket`) est **impossible** sans la clé publisher de tobspr — l'oracle est la seule voie de vérification de possession.

### 4.5 Logique métier

- **Compteurs** : `downloads` reste le seul agrégat stocké, incrémenté au download (D-19). `completions`, `likes` et `average_time` ne sont **plus** des colonnes stockées — les colonnes correspondantes ont été supprimées par migration et sont désormais calculées à la volée par agrégation sur `puzzle_completions` à chaque lecture (`list`/`search`/`download`), voir `docs/adr/0005-live-computed-aggregates.md` (D-15/D-18).
- **`average_time`** : moyenne des `time_taken` des complétions, calculée à la volée (D-16).
- **`difficulty`** : `completions / downloads`, calculée à la volée (D-14) — une valeur **basse** signifie un puzzle **difficile** (peu de joueurs qui l'ont téléchargé sont allés au bout). `NULL` tant que `downloads = 0` (jamais de division par zéro).
- **`top-rated`** : tri par likes (calculé à la volée), départage par complétions.

### 4.6 Modération

#### Rôles

Trois niveaux portés par `users.role`, chacun incluant les droits du précédent :

| Rôle | Droits |
|------|--------|
| `user` | Signaler un puzzle ; supprimer **ses propres** puzzles |
| `moderator` | Consulter la file de signalements ; masquer/démasquer un puzzle ; résoudre un signalement ; bannir temporairement |
| `admin` | Suppression définitive ; bans permanents et levées de ban ; promotion/rétrogradation des modérateurs (`savez mod promote`/`demote` — CLI uniquement, aucune route HTTP dédiée, cf. tableau des routes ci-dessous) ; accès au journal d'audit |

L'auteur du projet est le premier `admin` (créé par seed/CLI, jamais par l'API publique).

#### Modération automatique (à la soumission)

- **Filtre de vocabulaire** sur le titre (erreur `profane-title`, déjà prévue par le client). Liste de mots configurable, multilingue au minimum EN/FR. Un filtre naïf suffit au lancement — l'objectif est de bloquer l'évident, la modération humaine gère le reste.
- **Validation structurelle** stricte (cf. 4.2) : émetteurs/objectifs présents, formes valides, placements dans les bounds — élimine les puzzles cassés ou trollesques par construction.
- **Rate limiting par utilisateur** : plafond de soumissions par heure et par jour (configurable, seedé à 5/h + 20/j en écriture, 500/h en lecture — `savez mod ratelimit set`) pour bloquer le spam ; code d'erreur `ratelimit` côté client (pas `bad-payload`, générique et sans rapport — voir `docs/adr/0007-ratelimit-wire-code.md`).

#### Flux de signalement

1. Un joueur signale un puzzle en jeu (motifs du client : `profane`, `unsolvable`, `trolling`). Un utilisateur ne peut pas signaler son propre puzzle (`can-not-report-your-own-puzzle`) ni signaler deux fois le même.
2. Le signalement entre en file avec `status = pending`.
3. **Masquage automatique préventif** : si un puzzle accumule **N signalements `pending` d'utilisateurs distincts** (seuil **fixé à 3 en v1**, constante `AUTO_HIDE_REPORT_THRESHOLD` non exposée à la CLI — à rendre configurable plus tard sur le même modèle que le rate limiting, table de configuration + `savez mod`, si le besoin se confirme), il est masqué automatiquement (`hidden_at` renseigné, `hidden_by = NULL` pour distinguer l'automatique de l'humain) en attendant revue. Le masquage retire le puzzle des listes/recherches mais le laisse accessible à son auteur et aux modérateurs.
4. Un modérateur passe en revue : il joue/inspecte le puzzle, puis tranche chaque signalement — `upheld` (fondé) ou `rejected` (infondé) — avec le masquage/démasquage correspondant.
5. Les signalements `upheld` répétés contre un même auteur alimentent la décision de ban (pas d'automatisme : décision humaine, mais le compteur est affiché au modérateur).

#### Sanctions

- **Masquage** : réversible, ne détruit rien — l'outil par défaut.
- **Suppression définitive** : réservée aux admins, pour les contenus illégaux ou après ban de l'auteur. Le `shortKey` redevient disponible.
- **Ban temporaire** (`expires_at`) ou **permanent** : bloque login, soumission, complétion et signalement — l'API renvoie une erreur dédiée. Les puzzles existants de l'utilisateur restent visibles sauf décision contraire. Toute levée de ban est motivée (`lift_reason`).

#### API de modération

Le client du jeu n'expose que le signalement — le reste passe par huit routes dédiées, **hors du contrat shapez**, protégées par un middleware de rôle (`ModeratorUser`/`AdminUser`, `src/routes/moderation.rs`), telles qu'elles sont réellement livrées :

| Méthode | Route | Rôle minimum | Corps |
|---------|-------|---------------|-------|
| GET | `/v1/moderation/reports` | moderator | — (`?status=pending\|all\|...`, `limit`, `offset`) |
| POST | `/v1/moderation/reports/:id/resolve` | moderator | `{ status: "upheld"\|"rejected", notes? }` |
| POST | `/v1/moderation/puzzles/:id/hide` | moderator | `{ reason? }` (facultatif) |
| POST | `/v1/moderation/puzzles/:id/unhide` | moderator | `{ reason? }` (facultatif) |
| DELETE | `/v1/moderation/puzzles/:id` | admin | — |
| POST | `/v1/moderation/users/:id/ban` | moderator, **admin si permanent** | `{ reason, expiresAt? }` |
| POST | `/v1/moderation/users/:id/lift-ban` | admin | `{ reason }` |
| GET | `/v1/moderation/log` | admin | — (`limit`, `offset`) |

La route de ban applique deux règles de sécurité vérifiées avant tout appel au dépôt de données : un ban **permanent** (`expiresAt` absent) exige le rôle `admin` — un `moderator` ne peut poser qu'un ban **temporaire** ; et, dans tous les cas, un `moderator` ne peut jamais bannir une cible dont le rôle est **supérieur ou égal** au sien (ni un autre `moderator`, ni un `admin`), pour empêcher un modérateur de neutraliser la supervision d'un admin.

**Interface :** pas de panneau web au lancement, et **pas de CLI qui consomme ces routes HTTP par-dessus le réseau** — le texte initial de cette section, qui décrivait un CLI d'administration consommateur des routes `/v1/moderation/*`, est **superseded** par `docs/adr/0004-cli-serve-subcommand.md` (D-04). Le binaire livré expose deux sous-commandes distinctes du même exécutable : `savez serve` (lance le serveur HTTP, dont ces huit routes) et `savez mod <action>` (accède directement à la base de données, sans passer par HTTP). Les routes `/v1/moderation/*` visent une intégration future (client de modération dédié, éventuel panneau web), pas la CLI elle-même. Un mini panneau web ou une intégration au mod pourront venir plus tard si des modérateurs tiers rejoignent le projet.

#### Traçabilité

Toute action de modération écrit une entrée dans `moderation_log` (qui, quoi, quand, pourquoi). Ce journal est append-only et réservé aux admins : c'est la garantie de confiance interne d'un projet dont l'argument est la pérennité communautaire — y compris le jour où la modération n'est plus assurée par le seul auteur.

## 5. Spécifications du mod

- **Nature :** mod JavaScript pour shapez standalone ≥ 1.5 (système `MODS` natif) — pas de fork du client, installation par dépôt d'un fichier dans le dossier mods.
- **Double connexion :** le mod instancie un second `ClientAPI` (endpoint paramétrable vers le serveur communautaire) à côté de `app.clientApi` (officiel, inchangé). Deux sessions/token indépendants. Prérequis : rendre l'endpoint injectable (constructeur) sans toucher au comportement par défaut. **Cette injection n'est pas une simple commodité : côté Community Edition, `getEndpoint()` est figé en dur sur `https://api.shapez.io` (le switch dev/beta du client officiel a été retiré, remplacé par un commentaire `// TODO: Custom Puzzle DLC server / extract API into a mod?`) — c'est la seule voie pour faire dialoguer une build CE avec le serveur communautaire.** Le mod doit se charger et fonctionner identiquement sur les deux clients (officiel et CE ≥ 1.5).
- **Auth côté Community Edition :** la CE n'effectue plus l'échange de ticket Steam automatique du client officiel (`ipcRenderer.invoke("steam:get-ticket")`) — le joueur saisit son token manuellement via une boîte de dialogue propre à la CE. Le flux oracle (4.4) reste inchangé côté backend : `app.clientApi.token` est exploité de façon identique quelle que soit son origine, une fois peuplé. Ce point est documenté dans la doc utilisateur (M4) pour éviter la confusion des joueurs CE.
- **Fonction 1 — inscription/login** au serveur communautaire depuis le jeu, avec transmission du token officiel pour la vérification oracle (phase 1).
- **Fonction 2 — export :** bouton dans l'onglet « My puzzles » du menu puzzle (`src/js/states/puzzle_menu.js`) qui itère : `clientApi.apiListPuzzles("mine")` → pour chaque puzzle `apiDownloadPuzzle(id)` → `communityApi.apiSubmitPuzzle(...)`. **Débit :** séquentiel, pause ≥ 1 s entre requêtes, reprise sur erreur, rapport de fin (exportés / ignorés / échoués). Idempotence via `shortKey` (déjà présent sur le serveur ⇒ ignoré).
- **Fonction 3 — navigation :** consulter/jouer les puzzles du serveur communautaire depuis le menu (réutilisation de l'UI existante pointée sur `communityApi`).
- **Pré-filtre client :** fonctionnalités visibles uniquement si `app.platformWrapper.dlcs.puzzle` (confort ; la barrière réelle est côté serveur).

## 6. Déploiement et exploitation

Principe directeur : **le coût et la complexité d'exploitation sont des specs**, pas des détails — un serveur communautaire bénévole meurt de ses coûts récurrents et de sa charge de maintenance avant de mourir de ses bugs. L'architecture (binaire statique + SQLite) est choisie pour qu'une seule petite machine suffise pendant des années.

### 6.1 Cible : un VPS unique

| Poste | Choix recommandé | Coût estimé |
|-------|------------------|-------------|
| Serveur | VPS entrée de gamme (Hetzner CX22, OVH/Scaleway équivalent — 2 vCPU, 2-4 Go RAM, largement surdimensionné) | ~4-5 €/mois |
| Nom de domaine | un `.fr`/`.io`/`.dev` au choix (éviter d'inclure « shapez » — cf. 3.2) | ~10-15 €/an |
| TLS | Let's Encrypt via Caddy (automatique) | 0 € |
| Sauvegardes | Object storage S3-compatible (Backblaze B2, Scaleway) — quelques Go | < 1 €/mois |

**Total : ~6 €/mois.** Pas de base de données managée, pas de Kubernetes, pas de CDN — rien dans ce projet ne le justifie : l'API sert de petits JSON à une communauté de niche, un VPS à 4 € encaisse ça sans effort.

### 6.2 Architecture de déploiement

```
Internet ──► Caddy (:443, TLS auto) ──► shapez-puzzle-server (:15001, localhost only)
                                              │
                                              └── puzzles.sqlite ──► Litestream ──► S3
```

- **Caddy** en reverse proxy : deux lignes de Caddyfile, TLS Let's Encrypt automatique, HTTP/2. (Nginx + certbot possible, mais Caddy minimise la maintenance.)
- **Le binaire** tourne en service **systemd** (`Restart=always`, utilisateur dédié non-root, `ProtectSystem=strict`) et n'écoute que sur localhost.
- **SQLite + [Litestream](https://litestream.io/)** : réplication continue du fichier vers l'object storage. En cas de perte totale du VPS, restauration à quelques secondes près avec `litestream restore`. Complément : un dump quotidien (`sqlite3 .backup` + cron) versionné sur 30 jours, comme ceinture et bretelles.
- **Endpoint `/healthz`** (à ajouter au backend, trivial) pour la supervision.

### 6.3 Mise en production et mises à jour

- **Build :** GitHub Actions compile le binaire Linux (`x86_64-unknown-linux-musl` — statique, zéro dépendance système) à chaque tag et publie une GitHub Release. Le binaire compilé aujourd'hui tournera tel quel dans dix ans, cohérent avec l'objectif de conservation.
- **Déploiement :** `scp` du binaire + `systemctl restart` — un script de 10 lignes. Les migrations SQLx s'appliquent automatiquement au démarrage. Pas d'orchestrateur : la coupure d'une seconde au restart est acceptable pour ce service.
- **Docker optionnel :** un `Dockerfile` (image `scratch` + binaire musl, ~10 Mo) est fourni pour qui préfère, mais n'est **pas** le mode de déploiement de référence — c'est une commodité pour les auto-hébergeurs tiers, dans l'esprit AGPL.

### 6.4 Supervision et exploitation courante

Volontairement minimal :

- **Uptime :** un service gratuit type UptimeRobot (ou une instance Uptime Kuma) pingue `/healthz` et alerte par mail.
- **Logs :** `tracing` vers stdout, capté par journald (`journalctl -u shapez-puzzle-server`). Pas de stack ELK.
- **Métriques :** au lancement, aucune. Si le besoin émerge : un endpoint Prometheus viendra plus tard.
- **Charge de maintenance visée :** < 1 h/mois (mises à jour de sécurité OS via `unattended-upgrades`, vérification des sauvegardes).

### 6.5 Reprise et transmission

Deux scénarios couverts par conception :

- **Perte du serveur :** nouveau VPS + script d'installation (Caddy, systemd, Litestream) + `litestream restore` ⇒ service restauré en < 1 h. Le script d'installation est versionné dans le repo (`deploy/`).
- **Transmission du projet :** tout ce qui est nécessaire pour reprendre l'exploitation (scripts, Caddyfile, unités systemd, doc de restauration) vit dans le repo sous AGPL — n'importe quel membre de la communauté peut relancer le service à l'identique. C'est le pendant opérationnel de l'objectif de pérennité.

## 7. Roadmap

### Backend (chantier 1)

| Étape | Livrable | Critère de fin |
|-------|----------|----------------|
| 1 | Squelette Axum, `GET /v1/puzzles/list/new` en dur | Réponse JSON servie sur :15001 |
| 2 | SQLx + migrations + list/download/submit persistants | CRUD vérifié via tests d'intégration |
| 3 | Module lz-string (`lz-str`) | Test unitaire d'interop avec une chaîne produite par le client réel |
| 4 | Auth : inscription, login, JWT, middleware `x-token`, oracle `reqwest` | Compte créé via token officiel valide ; refus sinon |
| 5 | Contrat shapez complet (statuts, erreurs, formats) | **Le client officiel en mode dev affiche et joue les puzzles du serveur** |
| 6 | Logique métier (stats, top-rated) + modération (auto-modération, flux de signalement, masquage auto, routes `/v1/moderation/*`, CLI admin, journal d'audit) | Parité fonctionnelle avec le service officiel ; scénario complet signalement → revue → sanction testé |

### Mod (chantier 2 — démarre après l'étape 5)

| Étape | Livrable |
|-------|----------|
| M1 | Mod chargeable, second `ClientAPI`, login communautaire |
| M2 | Export « mes puzzles » avec rapport |
| M3 | Navigation/jeu sur le serveur communautaire |
| M4 | Publication (mod.io / GitHub releases), doc utilisateur |

## 8. Risques et points ouverts

| Risque | Impact | Mitigation |
|--------|--------|------------|
| Refus ou silence de tobspr | Perte des fonctions oracle + export | Fallback assumé : contenu neuf uniquement, inscription libre dès la phase 1 |
| Double format de soumission (officiel compressé lz-string / Community Edition JSON brut, confirmé par comparaison directe des deux `ClientAPI` le 2026-08-08) | Puzzles CE rejetés ou corrompus si un seul format est supposé | Détection de format native à l'étape 3, pas un repli conditionnel — test d'interop contre les deux formats réels |
| Endpoint figé côté Community Edition (`https://api.shapez.io` en dur, pas de switch dev) | Impossible de tester le contrat contre une vraie build CE avant que le mod existe | Vérification end-to-end contre la CE reportée à l'étape M1 (injection d'endpoint par le mod) ; patch local non distribué possible pour QA anticipée à l'étape 5 |
| Extinction de `api.shapez.io` avant le lancement | Oracle inopérant | Basculer directement en phase 2 ; relancer tobspr sur la préservation du catalogue |
| Démarrage à froid (catalogue vide) | Faible adoption | Export créateurs (M2), communication communautaire (Discord shapez, CE) |
| Formule exacte difficulté/top-rated inconnue (backend officiel fermé) | Classements divergents | S'inspirer de gatez-backend ; assumer une formule propre documentée |
| Champs/comportements non documentés du contrat client | Bugs d'intégration | L'étape 5 utilise le client réel comme test d'acceptation |

## 9. Références

- Client officiel : [tobspr-games/shapez.io](https://github.com/tobspr-games/shapez.io) — `src/js/platform/api.js` (contrat), `src/js/savegame/savegame_typedefs.js` (types), `src/js/core/restriction_manager.js` (restrictions), `src/js/states/puzzle_menu.js` (UI puzzle)
- Référence backend : [armandosneto/gatez-backend](https://github.com/armandosneto/gatez-backend) (MIT — schéma et logique métier)
- Community Edition : [tobspr-games/shapez-community-edition](https://github.com/tobspr-games/shapez-community-edition)
- Interop compression : [lz-str (crates.io)](https://crates.io/crates/lz-str)
