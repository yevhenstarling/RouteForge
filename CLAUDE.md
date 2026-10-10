# RouteForge

Capstone project for a Rust bootcamp: a backend that turns a day's parcel orders into optimized courier routes, with the route optimization spread over horizontally scalable workers.

## Checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check -p routeforge-bot --locked    # the bot must build without the `db` feature
cargo test --workspace --locked
```

From Phase 2 on, tests need Postgres: `docker compose up -d db`, with `.env` copied from `.env.example`.

## Rules that are easy to break

- Exactly five crates. Helpers go into an existing crate, never a new one.
- `routeforge-tsp` has no I/O, no async and no workspace dependencies, and takes every random number from a seeded generator.
- `routeforge-api` never depends on `routeforge-tsp`. `routeforge-bot` never enables the `db` feature of `routeforge-domain`.
- No `unsafe`. No `unwrap()` outside tests.
- Route costs are integer metres.
- Lease and retry times come from the database clock, not the worker's.
- Nothing may need OpenSSL, CMake or NASM to build: TLS stays on rustls with ring, crypto on pure-Rust backends (D12).

## Dependencies

Versions are pinned in the root `Cargo.toml` and `Cargo.lock`. Several had recent major releases (sqlx 0.9, utoipa 6, rand 0.10, jsonwebtoken 11, argon2 0.6, criterion 0.8), so check docs.rs for the pinned version. `cargo info <crate>` lists a crate's features.

The project is developed on Windows. Keep helper scripts in both PowerShell and bash, and files in LF.
