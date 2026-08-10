# Contrat de lancement CLI : `savez serve` explicite

Avant la Phase 7, `main.rs` n'avait aucun parsing d'arguments — le binaire ne savait faire
qu'une chose (lancer le serveur HTTP), invoqué sans argument. La Phase 7 introduit une CLI
admin `clap` (`DEC-moderation-model`) pour la modération (file de signalements, hide/unhide,
ban/unban, configuration du rate limiting). Plutôt que de garder "aucun argument = serveur"
et d'ajouter les commandes de modération à côté, le binaire distingue désormais explicitement
`savez serve` (lance le serveur HTTP) de `savez mod <...>` (commandes de modération) via
`clap::Subcommand`. Ceci casse volontairement le contrat de lancement actuel : tout script de
déploiement qui invoquait le binaire sans argument devra passer `serve` explicitement. Comme
la Phase 8 (déploiement — Dockerfile, unité systemd) n'a pas encore été construite au moment
de cette décision, l'impact est absorbé en amont plutôt que de forcer une migration ultérieure
du script de déploiement une fois en production.
