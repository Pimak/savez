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

The project has completed **phase 6 of 12** (complete shapez `ClientAPI` contract): all 21 real `T.backendErrors` wire codes, mandatory `x-token` authentication on every `/v1/puzzles/*` route, the all-HTTP-200 business-error convention (with its documented 5xx infrastructure-failure exception), full submission validation (emitters, goals, shape-key grammar, `shortKey`, title, building placement), per-user `completed`/`mine`, and author-reversible puzzle deletion. See `docs/cahier-des-charges.md` for the full specification and `.planning/ROADMAP.md` for the detailed roadmap.

## Usage

The binary has two explicit, mutually exclusive modes (D-04) — there is no longer an implicit
default when no argument is given:

- `savez serve` (or, in development, `cargo run -- serve`) launches the HTTP server.
- `savez mod <action>` (or `cargo run -- mod <action>`) runs a single moderation action directly
  against the database and exits — no running server required.

> [!IMPORTANT]
> This is a breaking change to the launch contract: any deployment script that previously invoked
> the binary with no argument at all must now pass `serve` explicitly. See
> `docs/adr/0004-cli-serve-subcommand.md` for the full rationale. The phase 8 Dockerfile and
> systemd unit must invoke `savez serve`, not a bare `savez`.

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
