# Le rejet pour dépassement de quota utilise le code `ratelimit`, pas `bad-payload`

## Contexte

`docs/cahier-des-charges.md` §4.6 et `REQUIREMENTS.md` décrivent tous deux, en toutes lettres, un
rejet de rate limiting renvoyant le code générique `bad-payload`. Mais `src/error.rs::T_BACKEND_ERRORS`
-- la liste des 21 codes réellement présents dans la taxonomie `T.backendErrors` du client
(`translations/base-en.yaml`, rétro-ingénierie faite en Phase 6 contre le code source des deux
vrais clients) -- contient déjà une entrée spécifique `ratelimit`.

Ce conflit n'a pas été résolu par 07-RESEARCH.md (question ouverte n°1, Pitfall 4) : le texte du
SPEC est explicite, mais `ratelimit` est objectivement le code le plus proche sémantiquement.

## Décision

`AppError::RateLimited` renvoie le code `"ratelimit"`, pas `"bad-payload"`.

`docs/adr/0003-all-200-error-taxonomy.md` fixe la règle de maintenance de ce projet en toutes
lettres : « Tout nouveau chemin d'erreur réutilise un code `T.backendErrors` existant dès qu'un
sens équivalent existe dans les 21 clés. Un nouveau code n'est inventé que faute de
correspondance. » `ratelimit` est déjà l'une de ces 21 clés, et son sens est un équivalent exact
d'un dépassement de quota -- il n'y a ici ni ambiguïté ni besoin d'inventer quoi que ce soit.

Le texte `bad-payload` du SPEC et de REQUIREMENTS.md est antérieur à la rétro-ingénierie complète
de la taxonomie du client faite en Phase 6 : au moment où ce texte a été écrit, l'existence même
du code `ratelimit` dans `T.backendErrors` n'était pas encore confirmée dans ce projet. Il ne
s'agit donc pas d'un choix délibéré du SPEC en connaissance de cause, mais d'un texte devenu
obsolète par un enrichissement de connaissance postérieur.

## Conséquences

Un joueur dont le compte dépasse un quota d'écriture ou de lecture voit un message client
spécifique de limitation de débit (la traduction associée à `ratelimit` dans
`translations/base-en.yaml`), au lieu du message générique de charge invalide associé à
`bad-payload` -- un message plus juste pour l'utilisateur final, sans coût d'implémentation
supplémentaire côté serveur (`ratelimit` était déjà listé dans `T_BACKEND_ERRORS`, donc déjà admis
par `every_business_variant_has_a_known_code`, `src/error.rs`).

Le texte de `docs/cahier-des-charges.md` §4.6 et de `REQUIREMENTS.md` reste, pour l'instant,
en désaccord littéral avec le code réellement produit par le serveur. L'alignement de ce texte
est confié explicitement au plan 07-11 (dernier plan de la phase, chargé de la relecture
documentaire globale) -- ce n'est pas un oubli de ce plan-ci.

## Statut

Accepté (2026-08-10, plan 07-07).
