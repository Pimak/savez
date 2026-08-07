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

## Non-goals

- Copying the official community catalog (no scraping of third-party content).
- Replacing the official service while it remains operational.
- A web version of the client (aligned with the Community Edition: standalone only).

## Development status

The project is currently in **phase 1 of 12** (project foundations). See `docs/cahier-des-charges.md` for the full specification and `.planning/ROADMAP.md` for the detailed roadmap.

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
