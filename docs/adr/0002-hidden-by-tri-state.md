# `hidden_by` porte trois significations distinctes

`DEC-moderation-model` verrouille `hidden_by = NULL` comme signifiant un masquage *automatique*
par seuil de signalements (Phase 7). La Phase 6 introduit un second cas d'usage de `hidden_at`/
`hidden_by` : la suppression réversible par l'auteur lui-même (`delete/:id`, D-08/D-09 de
06-CONTEXT.md), plutôt qu'une colonne séparée dédiée à ce cas. Pour ne pas entrer en collision
avec la convention `NULL = automatique` déjà verrouillée, l'auto-suppression par l'auteur pose
`hidden_by = auth.user_id` (l'auteur lui-même) — jamais `NULL`. À terme, `hidden_by` porte donc
trois significations distinctes selon sa valeur : `NULL` (masquage automatique par signalements,
Phase 7), l'`id` de l'auteur (auto-suppression, Phase 6), ou l'`id` d'un modérateur (action
modération humaine, Phase 7). Un futur lecteur ne doit pas supposer que `hidden_by` identifie
toujours un modérateur.
