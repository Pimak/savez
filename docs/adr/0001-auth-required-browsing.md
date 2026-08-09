# Authentification obligatoire pour lister/parcourir/télécharger les puzzles

Depuis la Phase 3, `GET /v1/puzzles/list/:category` et `GET /v1/puzzles/download/:idOrShortKey`
étaient accessibles sans authentification. La Phase 6 introduit le champ `completed` calculé
relativement à l'utilisateur courant (SC4 du ROADMAP), ce qui exige de connaître l'utilisateur
sur ces routes ainsi que sur la nouvelle route `search`. Plutôt que de traiter l'authentification
comme optionnelle (`completed` retombant à `false` sans token), ces trois routes deviennent
authentification obligatoire — cohérent avec le fait que le vrai client officiel se connecte
avant de parcourir le catalogue. Ceci casse volontairement l'accès anonyme dont bénéficiaient
`list`/`download` depuis les Phases 3/4.

Un accès sans `x-token` valide reçoit HTTP 200 avec `{ "error": "unauthorized" }` (jeton absent)
ou `{ "error": "bad-token" }` (jeton invalide) — exactement le même traitement que `submit`/
`delete`. Cette formulation remplace le statut de refus HTTP littéral que D-06 et le texte
initial de cet ADR écrivaient : D-17, plus spécifique et postérieur, impose désormais le contrat
tout-200 à ces deux chemins de rejet nommément. L'intention de D-06 — l'UNIFORMITÉ de traitement
entre routes protégées — est préservée intégralement ; seul le véhicule change, pas le principe.
Voir `docs/adr/0003-all-200-error-taxonomy.md` pour la règle générale.
