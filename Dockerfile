# syntax=docker/dockerfile:1
#
# Build multi-étages : compile un binaire musl statique pour x86_64-unknown-linux-musl dans
# l'étage builder, puis le copie seul dans une image finale distroless non-root. Voir
# docs/adr/0004-cli-serve-subcommand.md (invocation `serve` explicite obligatoire) et
# .planning/phases/08-d-ploiement/08-RESEARCH.md (Pitfalls 1/3/4) pour le rationale complet.

FROM rust:slim-bookworm AS builder

# musl-tools fournit musl-gcc : requis parce que `ring` (via la feature `tls-rustls-ring-webpki`
# de sqlx, et la feature `ring` de rustls) compile de l'assembleur/C et réclame ce compilateur
# spécifique pour la cible musl -- il n'est pas installé par défaut même une fois la cible Rust
# elle-même ajoutée via rustup (08-RESEARCH.md Pitfall 3). Si la compilation échoue faute de
# toolchain C de base, ajouter `build-essential` à cette même ligne apt-get est le repli
# documenté (hypothèse A3 de 08-RESEARCH.md), pas un changement de conception.
RUN apt-get update && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /app
COPY . .

# Aucune base Postgres n'est joignable dans un contexte de build : les macros query!/query_as!
# doivent lire le cache .sqlx/ déjà versionné (même convention que .github/workflows/ci.yml) --
# .dockerignore ne doit JAMAIS exclure .sqlx/ ni migrations/, sous peine de casser cette étape.
ENV SQLX_OFFLINE=true
RUN cargo build --release --target x86_64-unknown-linux-musl

# gcr.io/distroless/static-debian12:nonroot -- ce choix est porteur, pas cosmétique : reqwest
# 0.13 vérifie les certificats via rustls-platform-verifier/rustls-native-certs, qui lit
# /etc/ssl/certs/ca-certificates.crt A L'EXECUTION (08-RESEARCH.md Pitfall 1). Sur une base
# `scratch` l'appel oracle vers OFFICIAL_API_URL échouerait à la première tentative de login
# alors que /healthz répondrait normalement. distroless ships ces certificats CA nativement, et
# tourne en UID 65532 non-root sans shell ni gestionnaire de paquets (surface d'attaque réduite).
FROM gcr.io/distroless/static-debian12:nonroot

LABEL org.opencontainers.image.source="https://github.com/Pimak/savez"

COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/savez /savez

# Ne PAS copier migrations/ dans l'image finale : sqlx::migrate!() les embarque déjà dans le
# binaire à la compilation (Pattern 1, 08-PATTERNS.md).
#
# Aucune instruction HEALTHCHECK : l'image distroless n'a ni shell, ni curl, ni wget (08-RESEARCH.md
# Pitfall 6) ; la sonde de vivacité est externe (UptimeRobot/Uptime Kuma sur /healthz) et la course
# au démarrage de Postgres est déjà couverte par connect_with_retry dans src/main.rs.
#
# ENTRYPOINT/CMD en deux instructions distinctes (jamais fusionnées en un seul
# CMD ["/savez","serve"]), pour que `docker run <image> mod reports` puisse surcharger la seule
# sous-commande sans reconstruire toute la ligne de commande (docs/adr/0004-cli-serve-subcommand.md :
# une invocation sans argument n'a plus le même comportement depuis la Phase 7).
ENTRYPOINT ["/savez"]
CMD ["serve"]
