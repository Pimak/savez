# Adresse d'écoute configurable via `BIND_ADDR`

`src/main.rs` liait jusqu'ici l'écouteur HTTP en dur sur `127.0.0.1`
(`SocketAddr::from(([127, 0, 0, 1], config.port))`). Ce contrat convenait à toute exécution
bare-metal ou `cargo run` en développement, mais devient bloquant avec la topologie Docker
Compose fixée par `DEC-deployment-architecture` (PROJECT.md) : dans un conteneur applicatif,
un service sibling (`caddy`, via `reverse_proxy app:15001`) ne peut atteindre l'application
qu'à travers le réseau Compose, jamais via `127.0.0.1` du conteneur `app` — un `127.0.0.1` en
dur y rendrait le serveur strictement injoignable, y compris depuis Caddy. 08-RESEARCH.md
affirme « no Rust code changes are required » pour la Phase 8 ; c'est faux précisément sur ce
point, vérifié à la ligne 98 de `src/main.rs` avant ce correctif.

## Décision

Ajouter `BIND_ADDR` comme variable d'environnement **optionnelle avec défaut**, suivant
exactement la convention déjà établie pour `PORT`/`AUTH_MODE` dans `Config::from_lookup` :
absente → défaut `127.0.0.1` (comportement historique strictement préservé) ; présente et
valide → adresse IP correspondante (`0.0.0.0`, `::`, etc.) ; présente et invalide →
`ConfigError::InvalidBindAddr`, jamais un repli silencieux sur le loopback. La stack de
production (`deploy/docker-compose.yml`, plan 08-02) fixe explicitement `BIND_ADDR=0.0.0.0`
dans son `.env`, tandis que le développement local et tout binaire lancé nu conservent le
défaut loopback sans configuration supplémentaire.

## Alternatives écartées

- **Écouter toujours sur `0.0.0.0` inconditionnellement** — rejetée : dégraderait
  silencieusement le contrat d'une exécution bare-metal ou d'un `cargo run` de développeur, qui
  se retrouverait à écouter sur toutes les interfaces réseau de la machine sans que rien ne le
  signale ni ne le justifie hors contexte conteneur.
- **Publier le port `app` sur `127.0.0.1` de l'hôte et laisser Caddy passer par l'hôte plutôt
  que par le réseau Compose interne** — rejetée : cela réintroduit une publication de port que
  le Pitfall `ufw`/Docker documenté par 08-RESEARCH.md rend précisément dangereuse (`ufw` ne
  voit pas les ports publiés par Docker, qui contournent la chaîne `INPUT` via NAT/`FORWARD`) ;
  la non-publication du port sur `app` reste la seule barrière fiable.

## Conséquence assumée

Une variable d'environnement de plus à connaître et à documenter (`.env.example`,
`deploy/.env.example`). Son oubli en production produit une erreur franche côté Caddy
(« connection refused » sur `app:15001`, puisque l'application resterait sur `127.0.0.1` du
conteneur applicatif) et non une faille de sécurité silencieuse — l'échec est bruyant, pas
dangereux. La garantie `SC2` (« conteneur applicatif `:15001` localhost-only ») continue de
reposer sur l'absence de clé `ports:` sur le service `app` dans `deploy/docker-compose.yml`
(plan 08-02), jamais sur la valeur de `BIND_ADDR` : cette dernière ne fait que déterminer si
l'application est joignable *depuis le réseau Compose interne*, pas depuis Internet.
