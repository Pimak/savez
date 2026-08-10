#!/usr/bin/env bash
#
# deploy/install.sh -- transforme un VPS Debian générique fraîchement provisionné en machine
# prête à recevoir la stack savez (Docker Compose : db, app, caddy, kuma).
#
# Portabilité (D-04) : aucune commande spécifique à un fournisseur VPS particulier. Ce script
# suppose seulement un Debian récent, rien d'autre.
#
# Réexécutable sans effet de bord (D-09) : chaque étape coûteuse est protégée par une garde
# d'idempotence -- relancer ce script sur une machine déjà provisionnée ne casse rien et n'écrase
# jamais un secret déjà renseigné. C'est la condition qui le rend utilisable dans l'urgence d'une
# reprise (voir deploy/RESTORE.md étape 1).
set -euo pipefail

if [ "$(id -u)" -ne 0 ]; then
  echo "install.sh: doit être exécuté en root (sudo -i, ou une session déjà root)" >&2
  exit 1
fi

# Répertoire du script lui-même, quel que soit le répertoire courant de l'appelant -- rend les
# copies de fichiers ci-dessous indépendantes de l'endroit d'où le script est invoqué.
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"

DEPLOY_TARGET="/opt/savez/deploy"

# ---------------------------------------------------------------------------
# Étape 1 -- paquets de base
# ---------------------------------------------------------------------------
apt-get update
apt-get install -y --no-install-recommends \
  ca-certificates curl gnupg ufw unattended-upgrades

# ---------------------------------------------------------------------------
# Étape 2 -- Docker (dépôt apt officiel, jamais le paquet Debian, plus ancien et patché plus
# lentement)
# ---------------------------------------------------------------------------
if ! command -v docker &>/dev/null; then
  install -m 0755 -d /etc/apt/keyrings
  curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc
  chmod a+r /etc/apt/keyrings/docker.asc

  # shellcheck disable=SC1091
  DEBIAN_CODENAME="$(. /etc/os-release && echo "$VERSION_CODENAME")"
  echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian ${DEBIAN_CODENAME} stable" \
    >/etc/apt/sources.list.d/docker.list

  apt-get update
  apt-get install -y --no-install-recommends \
    docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
fi

# ---------------------------------------------------------------------------
# Étape 3 -- rclone (installeur officiel rclone.org, jamais le paquet Debian, régulièrement
# obsolète)
# ---------------------------------------------------------------------------
if ! command -v rclone &>/dev/null; then
  curl -fsSL https://rclone.org/install.sh | bash
fi

# ---------------------------------------------------------------------------
# Étape 4 -- utilisateur dédié, non-root, membre du groupe docker (pilotage de la stack sans
# sudo)
# ---------------------------------------------------------------------------
if ! id -u savez &>/dev/null; then
  useradd --system --create-home --home-dir /opt/savez --shell /usr/sbin/nologin savez
fi
usermod -aG docker savez

# ---------------------------------------------------------------------------
# Étape 5 -- fichiers : arborescence /opt/savez/deploy, squelette .env jamais écrasé
# ---------------------------------------------------------------------------
mkdir -p "$DEPLOY_TARGET"

# Ne copie le contenu de deploy/ que si le script n'est pas déjà exécuté depuis
# /opt/savez/deploy lui-même (reprise) -- une copie récursive d'un répertoire sur lui-même
# casserait l'exécution.
if [ "$SCRIPT_DIR" != "$DEPLOY_TARGET" ]; then
  cp -r "$SCRIPT_DIR"/. "$DEPLOY_TARGET"/
fi

# cp -n : jamais cp tout court -- une seconde exécution ne doit JAMAIS écraser un .env déjà
# renseigné par l'opérateur. La copie depuis le squelette est la seule source des noms de
# variables : ce script ne réécrit jamais la liste des clés lui-même.
cp -n "$DEPLOY_TARGET/.env.example" "$DEPLOY_TARGET/.env"
chmod 600 "$DEPLOY_TARGET/.env"
chown savez:savez "$DEPLOY_TARGET/.env"
chmod +x "$DEPLOY_TARGET/backup.sh"
chown -R savez:savez "$DEPLOY_TARGET"

# ---------------------------------------------------------------------------
# Étape 6 -- pare-feu (ufw) : refus par défaut en entrée, 22/80/443 seulement
# ---------------------------------------------------------------------------
#
# AVERTISSEMENT : ufw ne filtre PAS les ports publiés par Docker. Docker écrit ses règles de
# publication de port dans les chaînes DOCKER/FORWARD via NAT, évaluées AVANT la chaîne INPUT
# que gère ufw -- un `ports:` ajouté sur le service `db` "juste pour déboguer" serait donc
# directement joignable depuis Internet, quelles que soient les règles ufw ci-dessous. La seule
# protection réelle de PostgreSQL est l'absence de clé `ports:` sur le service `db` dans
# deploy/docker-compose.yml : aucun futur éditeur ne doit l'ajouter.
ufw default deny incoming
ufw default allow outgoing
ufw allow 22/tcp
ufw allow 80/tcp
ufw allow 443/tcp
ufw --force enable

# ---------------------------------------------------------------------------
# Étape 7 -- unités systemd de sauvegarde (seul le minuteur s'active)
# ---------------------------------------------------------------------------
cp "$DEPLOY_TARGET/systemd/savez-backup.service" /etc/systemd/system/savez-backup.service
cp "$DEPLOY_TARGET/systemd/savez-backup.timer" /etc/systemd/system/savez-backup.timer
systemctl daemon-reload
systemctl enable --now savez-backup.timer

# ---------------------------------------------------------------------------
# Étape 8 -- mises à jour de sécurité automatiques (cohérent avec l'objectif de maintenance
# inférieure à une heure par mois, CON-ops-cost)
# ---------------------------------------------------------------------------
cat >/etc/apt/apt.conf.d/20auto-upgrades <<'EOF'
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
EOF

# ---------------------------------------------------------------------------
# Récapitulatif -- gestes manuels restants, dans l'ordre
# ---------------------------------------------------------------------------
cat <<EOF

install.sh : machine prête à recevoir la stack. Gestes manuels restants :

  1. Renseigner ${DEPLOY_TARGET}/.env (édition manuelle, ne JAMAIS commiter ce fichier) :
     - DOMAIN : nom de domaine réel (jamais le placeholder votre-domaine.example en production)
     - JWT_KEY : secret de continuité -- une valeur différente d'une valeur déjà en usage
       déconnecte tous les joueurs
     - POSTGRES_PASSWORD : DOIT être identique au mot de passe encodé dans DATABASE_URL
     - Les clés RCLONE_* (stockage S3-compatible des sauvegardes)
  2. Faire pointer un enregistrement DNS du domaine choisi vers l'adresse IP de cette machine.
  3. Rendre le paquet GHCR de l'image applicative accessible (privé par défaut, voir
     deploy/RESTORE.md étape 4).
  4. Depuis ${DEPLOY_TARGET} : docker compose pull && docker compose up -d

Pour la restauration de données après perte totale, voir deploy/RESTORE.md.
EOF
