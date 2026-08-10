# Cache TTL court pour rôle/ban, sans invalidation ciblée

Le JWT émis par le serveur est volontairement minimal (`sub` + `exp` seuls, D-06 Phase 5) et longue
durée (30 jours, sans revalidation) — il ne peut donc jamais porter le rôle ou l'état de ban d'un
utilisateur sans rouvrir cette décision. La Phase 7 doit néanmoins faire respecter les bans et les
routes réservées aux modérateurs/admins sur chaque requête protégée. Plutôt qu'une lecture DB fraîche
à chaque requête (coût nul à cette échelle mais retenu comme option la plus simple/prévisible), le
choix retenu est un cache TTL court côté serveur (5 à 15 secondes) pour le rôle et l'état de ban.
Conséquence assumée : un utilisateur banni pendant qu'il agit activement (ex. spam de signalements)
peut continuer d'agir jusqu'à quelques secondes après la décision du modérateur, le temps que le cache
expire — aucune invalidation ciblée n'est déclenchée au moment du `ban` CLI (ça demanderait une
coordination entre le process CLI et le serveur HTTP, jugée disproportionnée à cette échelle). Ce
délai de propagation est un compromis sécurité/performance délibéré, pas un oubli — à reconsidérer
seulement si cette fenêtre s'avère un problème réel en usage.
