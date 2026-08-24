# Reprise après perte totale du VPS

Runbook destiné à un membre de la communauté qui reprend l'exploitation de `savez` sans avoir
lu aucun document de planification. Chaque étape numérotée énonce sa commande exacte et le
résultat attendu.

**Architecture cible en trois lignes :** Caddy est le seul conteneur qui publie des ports (80 et
443) et gère la TLS automatique ; l'applicatif et PostgreSQL ne sont joignables que sur le réseau
interne Docker Compose, jamais directement depuis Internet ; la sauvegarde est un `pg_dump`
quotidien compressé, téléversé vers un stockage S3-compatible.

## Prérequis à réunir AVANT de commencer

| Prérequis | Détail |
|---|---|
| Accès à un VPS Debian | Root ou sudo, IP publique connue |
| Accès au registrar du domaine | Pour créer/modifier un enregistrement DNS |
| Identifiants du stockage objet | `RCLONE_CONFIG_BACKUP_ENDPOINT`, `_ACCESS_KEY_ID`, `_SECRET_ACCESS_KEY`, nom du bucket |
| Valeur d'origine de `JWT_KEY` | Récupérée depuis le gestionnaire de mots de passe où elle doit avoir été conservée -- voir étape 2 |

## Étape 1 -- Provisionnement

1. Créer un VPS Debian générique chez le fournisseur de son choix (aucun fournisseur particulier
   n'est imposé par ce projet).
2. S'y connecter en root : `ssh root@<ip-du-vps>`.
3. Récupérer le dépôt : `git clone <url-du-depot> savez && cd savez`.
4. Exécuter le script de bootstrap : `bash deploy/install.sh`.

**Résultat attendu :** Docker, `docker compose`, `rclone`, un utilisateur système `savez`, un
pare-feu `ufw` restreint à 22/80/443, et le minuteur `savez-backup.timer` sont en place.
`/opt/savez/deploy/.env` existe (squelette vide, jamais écrasé si déjà renseigné).

## Étape 2 -- Configuration du `.env`

Renseigner `/opt/savez/deploy/.env` à partir du squelette copié par `install.sh`
(`deploy/.env.example` fait autorité sur la liste complète des clés) : `DOMAIN` y vaut par défaut
le placeholder générique `votre-domaine.example`, à remplacer impérativement par le nom de
domaine réel choisi à l'étape 3 -- ne jamais démarrer Caddy en production avec ce placeholder.
Trois pièges concrets à ne pas manquer :

- **`POSTGRES_PASSWORD` et le mot de passe encodé dans `DATABASE_URL` doivent être identiques.**
  Une valeur différente fait échouer l'authentification de `app` contre `db` dès le démarrage.
- **`BIND_ADDR` doit rester `0.0.0.0`.** Le défaut loopback (`127.0.0.1`) rend l'applicatif
  injoignable depuis le conteneur `caddy` sibling -- `reverse_proxy app:15001` échouerait en
  connexion refusée.
- **`RUST_LOG=info` conditionne l'existence même du journal de production.** Sans cette variable,
  le filtre par défaut de `tracing_subscriber` est `error` : les lignes `migrations applied` et
  `listening` (vérifiées à l'étape 6) ne s'affichent tout simplement pas.

> [!IMPORTANT]
> **`JWT_KEY`** est un secret de continuité, pas seulement de sécurité. Réutiliser la valeur
> d'origine préserve les sessions de tous les joueurs déjà connectés ; une valeur différente les
> déconnecte tous d'un coup, sans faille de sécurité ouverte pour autant. Cette valeur doit vivre
> dans un gestionnaire de mots de passe, jamais uniquement sur le serveur qui vient d'être
> détruit -- c'est précisément le scénario que ce runbook couvre.

## Étape 3 -- DNS (MANUEL)

Faire pointer un enregistrement `A` (et `AAAA` le cas échéant) du domaine choisi vers l'adresse IP
du VPS, puis **attendre la propagation avant de démarrer Caddy** : sans résolution DNS correcte,
le challenge ACME de Let's Encrypt échoue et aucun certificat n'est délivré.

Rappel permanent : le nom de domaine retenu ne doit jamais contenir « shapez » (contrainte de
marque du projet) -- ni dans son intitulé ni d'une façon laissant croire à un service officiel.

Cette étape n'est pas automatisable : aucun script de ce dépôt n'a accès au compte registrar de
l'opérateur.

## Étape 4 -- Accès à l'image (MANUEL ET NON AUTOMATISABLE)

Un paquet publié sur GHCR (GitHub Container Registry) est **privé par défaut**, indépendamment de
la visibilité du dépôt source. Tant que ce réglage n'est pas corrigé, `docker compose pull`
échouera avec `unauthorized`.

Corriger cela dans les réglages du paquet GHCR (lié au dépôt), en le liant au dépôt public ou en
le passant explicitement public.

Le contournement consistant à stocker un jeton d'accès personnel de longue durée dans le `.env`
du serveur est **proscrit** : il déplace le problème vers un secret supplémentaire à faire
tourner, pour ne rien résoudre -- corriger le réglage de visibilité du paquet est la seule
solution qui ne crée pas ce fardeau.

## Étape 5 -- Restauration des données

L'ordre ci-dessous n'est pas négociable : l'applicatif applique ses migrations SQLx au démarrage.
Le démarrer avant la restauration créerait un schéma déjà migré, et restaurer un `pg_dump`
par-dessus provoquerait des conflits d'objets déjà existants (tables, contraintes). `db` doit donc
être seul et prêt avant toute écriture du dump.

1. Démarrer uniquement PostgreSQL et attendre son healthcheck :
   ```
   cd /opt/savez/deploy
   docker compose up -d db
   docker compose ps db   # attendre l'état "healthy"
   ```
2. Récupérer le dump le plus récent depuis le stockage objet :
   ```
   rclone lsf backup:${RCLONE_BUCKET}/savez/ | sort | tail -n1
   rclone copy backup:${RCLONE_BUCKET}/savez/<nom-du-fichier>.sql.gz .
   ```
3. Injecter le dump décompressé dans la base :
   ```
   gunzip -c <nom-du-fichier>.sql.gz | docker compose exec -T db psql -U savez -d savez
   ```
4. Vérifier que la restauration a bien peuplé la base :
   ```
   docker compose exec -T db psql -U savez -d savez -c "SELECT count(*) FROM users;"
   docker compose exec -T db psql -U savez -d savez -c "SELECT count(*) FROM puzzles;"
   ```
   **Résultat attendu :** deux comptes non nuls, cohérents avec le service perdu.

## Étape 6 -- Démarrage complet

```
cd /opt/savez/deploy
docker compose pull && docker compose up -d
```

Vérifications :

- `curl https://<domaine-reel>/healthz` répond `ok` en HTTPS (preuve que Caddy a obtenu son
  certificat et route correctement vers `app`).
- `docker compose logs app` contient, dans l'ordre, les lignes `migrations applied` puis
  `listening` (absentes sans `RUST_LOG=info`, voir étape 2).
- `docker compose ps --format '{{.Service}} {{.State}}'` affiche `running` pour les quatre
  services (`db`, `app`, `caddy`, `kuma`).

## Étape 7 -- Supervision (MANUEL)

**UptimeRobot** (moniteur externe, MANUEL) est le seul détecteur capable de signaler une perte
totale du VPS : un outil hébergé sur la machine surveillée ne peut structurellement pas signaler
sa propre disparition. Créer un moniteur HTTP(S) sur `https://<domaine-reel>/healthz`, intervalle
5 minutes.

Le dashboard **Uptime Kuma** auto-hébergé est interne par défaut (aucun port publié). Deux accès
possibles :

- **Par défaut :** tunnel SSH vers le port du conteneur, par exemple
  `ssh -L 3001:localhost:3001 root@<ip-du-vps>` puis `docker compose port kuma 3001` sur le VPS
  pour confirmer le mappage interne, ou plus simplement un tunnel direct vers le conteneur via
  `docker compose exec kuma` selon les besoins de débogage.
- **Optionnel :** décommenter le bloc de site `status.{$DOMAIN}` dans `deploy/Caddyfile` pour
  exposer le dashboard publiquement -- Kuma gère sa propre authentification (login intégré), donc
  décommenter ce bloc n'expose pas un service sans contrôle d'accès. Cette option reste
  explicitement facultative.

Configurer ensuite une notification **ntfy** (MANUEL) dans l'interface de Kuma (Settings ->
Notifications), le choix du topic revenant à l'opérateur.

## Étape 8 -- Vérification de la sauvegarde

```
systemctl list-timers | grep savez-backup   # confirme que le minuteur est armé
systemctl start savez-backup.service        # déclenche une exécution immédiate
rclone lsf backup:${RCLONE_BUCKET}/savez/ | sort | tail -n1   # nouvel objet .sql.gz
```

**Résultat attendu :** un nouvel objet `savez-<horodatage>.sql.gz` apparaît côté stockage.

## Budget temps

| Étape | Budget |
|---|---|
| 1. Provisionnement + `install.sh` | 15 min |
| 2. Configuration `.env` | 5 min |
| 3. DNS (propagation incluse) | 10 min |
| 4. Accès GHCR | 3 min |
| 5. Restauration des données | 10 min |
| 6. Démarrage complet + vérifications | 5 min |
| 7. Supervision (UptimeRobot + Kuma + ntfy) | 8 min |
| 8. Vérification de la sauvegarde | 2 min |
| **Total** | **58 min** |

## Pièges connus

- **Paquet GHCR privé par défaut** (étape 4) : `docker compose pull` échoue silencieusement avec
  `unauthorized` tant que la visibilité du paquet n'a pas été corrigée dans les réglages GitHub.
- **`BIND_ADDR` resté en loopback** (étape 2) : Caddy ne peut pas joindre `app`, `/healthz`
  répond en `502` malgré un conteneur `app` visiblement `running`.
- **`RUST_LOG` absent** (étape 2) : le conteneur `app` fonctionne, mais `docker compose logs app`
  reste muet -- aucune ligne `migrations applied`/`listening` pour confirmer le démarrage.
- **`ufw` qui semble protéger un port publié par Docker sans le faire** : `ufw` gère la chaîne
  `INPUT`, alors que Docker écrit ses règles de publication de port dans les chaînes
  `DOCKER`/`FORWARD` via NAT, évaluées avant `INPUT`. La seule protection réelle de PostgreSQL
  reste l'absence de clé `ports:` sur le service `db` dans `deploy/docker-compose.yml`.

## Note finale

Ce runbook n'a pas encore été exécuté contre un VPS réel : le déploiement réel (accès HTTPS
réel, test de reprise réel en conditions de production) reste un **checkpoint explicitement
ouvert**, à lever avant la Phase 12 (publication du mod, qui a besoin d'un serveur public réel).
