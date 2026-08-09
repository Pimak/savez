# Toutes les réponses en HTTP 200, sauf les pannes d'infrastructure

`DEC-api-contract-conventions` impose « tout en HTTP 200 », erreurs métier incluses : chaque
rejet renvoie `StatusCode::OK` avec un corps `{ "error": "<code>" }`, où `<code>` appartient à la
taxonomie `T.backendErrors` du client (21 clés, `translations/base-en.yaml`) ou, à défaut
d'équivalent, à une courte liste de codes propres au projet.

Cette règle vise les erreurs MÉTIER — un jeton absent, un payload malformé, un titre profane, une
tentative d'auto-signalement. Elle ne vise pas les pannes d'infrastructure. La taxonomie
`T.backendErrors` ne contient aucun code d'erreur interne (pas de `database-error` ni
`internal-error` côté client), et `AppError::Database` (perte de connexion, épuisement du pool)
reste donc un 500 littéral, corps vide, détail réservé à `tracing::error!` côté serveur
(T-06-05). Le client réel dégrade proprement dans les deux cas : `_request()` traite tout statut
≠ 200 par un rejet brut `bad-status: …`, qu'il s'agisse d'un 500 aujourd'hui ou d'un 503 futur.
Aucune conversion en `{ "error": … }` n'est nécessaire ni souhaitable pour ce chemin.

## Codes propres au projet

Trois codes n'ont aucun équivalent en amont dans `T.backendErrors` — le client affiche alors la
chaîne brute reçue (comportement existant de `_request()`, pas un cas spécial à coder) :

- `name-already-taken` — inscription refusée pour cause de pseudo déjà pris (`AppError::NameTaken`).
- `auth-mode-not-implemented` — `AUTH_MODE` configuré sur une valeur déclarée mais non
  fonctionnelle en v1 (`open`, `steam-openid`).
- `internal-error` — émission de JWT en échec après une vérification oracle pourtant réussie
  (`AppError::TokenIssuanceFailed`) ; c'est un cas de repli interne, distinct de `Database`, qui
  reste sous la convention tout-200 parce que la requête a été traitée jusqu'au bout côté métier.

## Règle de maintenance

Tout nouveau chemin d'erreur réutilise un code `T.backendErrors` existant dès qu'un sens
équivalent existe dans les 21 clés. Un nouveau code n'est inventé que faute de correspondance,
et doit alors être ajouté à la liste ci-dessus.

## Effet de bord accepté

Faire passer les rejets d'authentification (`unauthorized`, `bad-token`) de 401 à 200 masque ces
événements aux outils d'infrastructure qui filtrent par code HTTP (rate limiting périmétrique,
fail2ban, tableaux de bord basés sur le statut). C'est une contrainte imposée par le contrat
client (D-17), acceptée en connaissance de cause : la trace serveur (`tracing::warn!`) reste le
point d'observation de ces rejets, et un rate limiting par utilisateur authentifié est prévu en
Phase 7 (REQ-moderation), pas au niveau HTTP.
