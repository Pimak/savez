# Supervision /healthz double : UptimeRobot externe + Uptime Kuma auto-hébergé

Le critère de succès SC4 de la Phase 8 exige de détecter et de survivre à une perte totale du VPS.
Un outil de supervision auto-hébergé sur ce même VPS (ex. Uptime Kuma seul) ne peut structurellement
pas remplir ce rôle : s'il tombe en même temps que la machine qu'il surveille, il ne peut jamais
alerter de sa propre panne d'hébergement — c'est le problème classique « qui surveille le
surveillant ». La décision retenue est de combiner deux outils plutôt qu'un seul : **UptimeRobot**
(service externe, plan gratuit, 50 moniteurs, intervalle 5 min) comme détecteur principal de panne
totale — son infrastructure est indépendante du VPS `savez`, donc elle voit et alerte correctement
une coupure complète — et **Uptime Kuma auto-hébergé** (conteneur Docker Compose supplémentaire dans
la stack de production) comme dashboard interne bonus pour le suivi de latence et l'historique,
notifié via **ntfy** en plus des emails UptimeRobot. Alternatives écartées : Uptime Kuma seul
(rejeté — ne couvre pas SC4) ; Uptime Kuma hébergé par un tiers payant (PikaPods ~3 €/mois, Elestio
~11 €/mois — couvrirait aussi SC4, mais ajoute un coût récurrent et un compte/abonnement
supplémentaire par rapport au duo gratuit retenu, sous contrainte `CON-ops-cost`). Conséquence
assumée : un conteneur supplémentaire (Kuma) à maintenir/mettre à jour sur le VPS, jugé négligeable
au regard de son coût nul et de son caractère optionnel — seul UptimeRobot est réellement nécessaire
pour satisfaire SC4.
