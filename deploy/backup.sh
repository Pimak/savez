#!/usr/bin/env bash
#
# deploy/backup.sh -- sauvegarde quotidienne PostgreSQL vers un stockage S3-compatible.
#
# Rétention verrouillée à 30 jours (DEC-deployment-architecture) : ce n'est pas un réglage
# ajustable ici -- la procédure de restauration correspondante est documentée dans
# deploy/RESTORE.md.
#
# Mode strict volontairement sans concession : une sauvegarde qui échoue silencieusement est pire
# qu'une sauvegarde absente. Toute défaillance doit sortir en non-zéro pour que systemd
# (Type=oneshot) la marque en `failed`.
set -euo pipefail

# Chemin du fichier Compose de production, surchargeable pour permettre d'éprouver ce script
# contre la stack locale de développement (voir deploy/RESTORE.md et le smoke test de ce plan).
SAVEZ_COMPOSE_FILE="${SAVEZ_COMPOSE_FILE:-/opt/savez/deploy/docker-compose.yml}"

# Un `rclone copy` vers une destination vide réussirait silencieusement au mauvais endroit --
# contrôle explicite avant tout dump.
if [ -z "${RCLONE_REMOTE:-}" ] || [ -z "${RCLONE_BUCKET:-}" ]; then
  echo "backup.sh: RCLONE_REMOTE et RCLONE_BUCKET doivent être définis (voir deploy/.env.example) -- abandon avant tout dump" >&2
  exit 1
fi

TS="$(date -u +%Y%m%d-%H%M%S)"
DUMP="${TMPDIR:-/tmp}/savez-${TS}.sql.gz"
OBJECT_NAME="savez-${TS}.sql.gz"

# Nettoyage systématique du dump temporaire, y compris en cas d'échec du téléversement -- ne
# jamais laisser une copie en clair de la base sur le disque du VPS.
trap 'rm -f "$DUMP"' EXIT

echo "backup.sh: démarrage -- production de ${OBJECT_NAME}"

# -T : pas de pseudo-terminal, obligatoire en exécution non interactive (systemd) -- sans ce
# drapeau, `docker compose exec` alloue un tty et corrompt le flux binaire du dump.
docker compose -f "$SAVEZ_COMPOSE_FILE" exec -T db \
  pg_dump -U "${POSTGRES_USER:-savez}" -d "${POSTGRES_DB:-savez}" | gzip > "$DUMP"

# Un dump techniquement produit mais vide serait un faux positif -- contrôle avant tout
# téléversement.
if [ ! -s "$DUMP" ]; then
  echo "backup.sh: le dump produit est vide, abandon avant téléversement" >&2
  exit 1
fi

DUMP_SIZE="$(du -h "$DUMP" | cut -f1)"

# Ni identifiant, ni clé, ni point de terminaison ne sont jamais passés en argument -- rclone les
# lit dans l'environnement (RCLONE_CONFIG_BACKUP_*), donc invisibles dans `ps aux` et absents de
# l'historique du shell. Aucun fournisseur de stockage n'est codé en dur : ce script fonctionne à
# l'identique contre n'importe quel point de terminaison S3-compatible, au choix de l'opérateur.
rclone copy "$DUMP" "${RCLONE_REMOTE}:${RCLONE_BUCKET}/savez/"

# Rétention : purge des objets de plus de 30 jours -- contrainte verrouillée par
# DEC-deployment-architecture, pas un paramètre à arbitrer.
rclone delete "${RCLONE_REMOTE}:${RCLONE_BUCKET}/savez/" --min-age 30d

echo "backup.sh: terminé -- objet ${OBJECT_NAME} (${DUMP_SIZE}) téléversé vers ${RCLONE_REMOTE}:${RCLONE_BUCKET}/savez/"
