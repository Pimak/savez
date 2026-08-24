# Savez

Self-hosted, open-source Rust backend for shapez 1's community puzzle mode, with a preservation goal.

> [!IMPORTANT]
> **Status: Draft.** The entire project is conditioned on tobspr's agreement, requested by email on August 6, 2026, on three principles: ownership validation via the official API, export by creators of their own puzzles only, and future cooperation on catalog preservation. Development continues while awaiting a response; scope may be revised depending on the outcome of that exchange.

## Goals

**Primary goal:** preserve shapez 1's community puzzle experience through a self-hosted, open-source backend, independent of the official infrastructure (`api.shapez.io`).

**Secondary goals:**

- Give creators a tool to port their own puzzles away from the official service.
- Avoid harming the Puzzle DLC's commercial model while the official service is alive (access restricted to DLC owners).
- Serve as a Rust learning project for the author.
- Support both real shapez 1 clients in active use — the official client ([tobspr-games/shapez.io](https://github.com/tobspr-games/shapez.io)) and the community-maintained [Community Edition](https://github.com/tobspr-games/shapez-community-edition) — whose `ClientAPI` implementations diverge on the puzzle submission payload format (compressed vs raw JSON), the API endpoint (configurable vs hardcoded), and how the oracle token is obtained (automatic Steam ticket vs manual entry). See `docs/cahier-des-charges.md` §4.2/§5/§8.

## Non-goals

- Copying the official community catalog (no scraping of third-party content).
- Replacing the official service while it remains operational.
- A web version of the client (aligned with the Community Edition: standalone only).

## Development status

The project has completed **phase 8 of 12** (deployment artifacts): business logic and moderation (phase 7) plus a complete, locally smoke-tested deployment stack — Dockerfile, production Docker Compose (app + PostgreSQL + Caddy + Uptime Kuma), backup chain, bootstrap script, and recovery runbook (see [Deployment](#deployment)). See `docs/cahier-des-charges.md` for the full specification; the detailed phase roadmap is tracked in a separate private planning repository.

## Usage

The binary has two explicit, mutually exclusive modes (D-04) — there is no longer an implicit
default when no argument is given:

- `savez serve` (or, in development, `cargo run -- serve`) launches the HTTP server.
- `savez mod <action>` (or `cargo run -- mod <action>`) runs a single moderation action directly
  against the database and exits — no running server required.

> [!IMPORTANT]
> This is a breaking change to the launch contract: any deployment script that previously invoked
> the binary with no argument at all must now pass `serve` explicitly. See
> `docs/adr/0004-cli-serve-subcommand.md` for the full rationale. The production `Dockerfile`
> invokes `savez serve` via its `CMD` — there is no systemd unit for the application process
> itself, which runs as a Docker Compose service; systemd only manages the daily backup timer
> (see [Deployment](#deployment)).

### Command-line administration

`savez mod` covers the report queue, report resolution, hide/unhide, permanent deletion, bans,
role promotion, the moderation audit log, rate-limit thresholds, and the profanity word list.
Every action that writes to the database requires `--moderator <pseudo|uuid>` (no implicit
default), and every write is recorded in the append-only moderation log.

| Family | Example |
|---|---|
| Report queue | `savez mod reports --status pending` |
| Report resolution | `savez mod resolve 12 upheld --moderator alice --notes "confirmed"` |
| Hide / unhide | `savez mod hide 42 --reason "profane title" --moderator alice` |
| Permanent deletion | `savez mod delete 42 --moderator alice` |
| Ban / unban | `savez mod ban bob --reason spam --expires-in 7d --moderator alice` |
| Role promotion | `savez mod promote bob moderator --moderator alice` |
| Audit log | `savez mod log --limit 50` |
| Rate-limit thresholds | `savez mod ratelimit set write --window 3600 --limit 5 --moderator alice` |
| Profanity word list | `savez mod profanity add merde --lang fr --moderator alice` |

Run `savez mod --help` (or `savez mod <subcommand> --help`) for the full argument list of every
action.

### Reference moderation walkthrough

The sequence below is the exact command flow a fresh operator can replay end to end against a
running deployment, without reading any planning document. It assumes `DATABASE_URL`, `JWT_KEY`
and `OFFICIAL_API_URL` are already set (see [Usage](#usage)) and that at least one puzzle and one
non-admin account already exist (e.g. seeded via a real `submit`/`login` against the running
server).

1. `docker compose up -d db`, then in one terminal: `savez serve` (or, in development,
   `cargo run -- serve`). Confirm it started with `curl localhost:15001/healthz`.
2. Running the binary with no subcommand at all (`savez` / `cargo run --`) must NOT start the
   server — it prints help and exits non-zero (D-04, `docs/adr/0004-cli-serve-subcommand.md`).
3. In a second terminal: `savez mod reports` — an empty deployment prints an empty queue with no
   error.
4. `savez mod ratelimit list` — prints the three seeded rate-limit thresholds
   (`write` 3600s/5, `write` 86400s/20, `read` 3600s/500).
5. `savez mod profanity list --lang fr` — prints the seeded French word list (at least 25 words).
6. `savez mod promote <existing pseudo> moderator --moderator admin` — promotes that account and
   confirms it on stdout.
7. `savez mod log` — the most recent entry is the `set_role` action step 6 just produced, carrying
   the promoted pseudo.
8. `savez mod hide <puzzle id> --reason test --moderator admin`, then
   `savez mod unhide <puzzle id> --moderator admin` — two confirmations, then two new entries in
   `savez mod log`.
9. `savez mod ban <pseudo> --reason test --expires-in 1h --moderator admin` — prints the created
   ban id; then `savez mod unban <that ban id> --reason erreur --moderator admin` lifts it.
10. Running any write action with a `--moderator` that does not resolve to a real account fails
    cleanly (a readable message, a non-zero exit code), never a raw Rust panic.

Every step above must produce readable output, no panic, and no raw Rust error message — the
moderation audit log (`savez mod log`) should faithfully reflect every action taken.

## Deployment

> [!NOTE]
> No real deployment has happened yet, consistent with this project's Draft status: all artifacts
> below are produced and smoke-tested locally against the real Docker Compose stack, but no VPS or
> domain has been provisioned. Provisioning a VPS, a domain and an S3-compatible storage
> provider, then running the recovery drill for real, is an explicit open checkpoint to be closed
> before Phase 12.

A container image is published to GHCR (`ghcr.io/pimak/savez`) on every `v*` tag by
`.github/workflows/release.yml`, built from the multi-stage `Dockerfile` (musl builder →
distroless, non-root, `CMD ["serve"]`).

The reference deployment is Docker Compose, run from `deploy/`:

```
docker compose pull && docker compose up -d
```

Topology: Caddy terminates TLS (automatic via Let's Encrypt) and is the only service with
published ports; the application container and PostgreSQL stay on the internal Compose network,
never exposed to the host. PostgreSQL is backed up daily via `pg_dump` to S3-compatible object
storage, with 30 days of retention.

For provisioning a fresh VPS, or recovering from total server loss, follow
`deploy/RESTORE.md` — the authoritative, step-by-step recovery runbook.

## License

This backend is distributed under the **AGPL-3.0** license (see `LICENSE`): anyone
hosting a modified version of this server must publish their modified sources.

> Note: if code is reused from the [gatez-backend](https://github.com/armandosneto/gatez-backend)
> project (MIT, copyright Armando Soares e Silva Neto and Rafael Nunes Santana), the corresponding
> `LICENSE-MIT` file will be added to this repository at the time of reuse, in accordance with
> the terms of the original MIT license. To date, no code has been reused from that project.

The future client mod (compatible with the [shapez 1](https://github.com/tobspr-games/shapez.io) game) will be distributed under **GPLv3**, as a derivative work of the game's GPL client, in accordance with section 3.1 of the specification document.

## Author

Maxime Mainguet ([Pimak](https://github.com/Pimak)).

## References

- [tobspr-games/shapez.io](https://github.com/tobspr-games/shapez.io) — official shapez 1 client, API contract and data types targeted by this backend.
- [armandosneto/gatez-backend](https://github.com/armandosneto/gatez-backend) — reference community backend (MIT), data schema and business logic transposed from it.
