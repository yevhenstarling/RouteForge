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

From Phase 3 on there is an image too. After a change to the dependencies or to the `Dockerfile`, build it: `docker compose build solver`.

## Running the API

The API applies the schema when it starts, and a solver started against a database without it waits (D32). The API has no image and no compose service before Phase 6, so it runs on the host, with its settings in the shell:

```powershell
$env:DATABASE_URL = 'postgres://routeforge:routeforge@127.0.0.1:5432/routeforge'
$env:JWT_SECRET = 'any text of at least thirty-two bytes'
$env:ADMIN_EMAIL = 'admin@example.com'; $env:ADMIN_PASSWORD = 'a password of your own'
cargo run -p routeforge-api
```

In bash it is `export NAME=value` for each. The API does not read `.env`.

Swagger UI is then at `http://127.0.0.1:8080/docs/`. The settings are listed at the top of `crates/routeforge-api/src/main.rs` and in `.env.example`. The solvers run beside it with `docker compose up -d solver`.

## Rules that are easy to break

- Exactly five crates. Helpers go into an existing crate, never a new one.
- `routeforge-tsp` has no I/O, no async and no workspace dependencies, and takes every random number from a seeded generator.
- `routeforge-api` never depends on `routeforge-tsp`. `routeforge-bot` never enables the `db` or the `openapi` feature of `routeforge-domain`.
- No `unsafe`. No `unwrap()` outside tests.
- Route costs are integer metres.
- Lease and retry times come from the database clock, not the worker's.
- After changing a query or a migration, run `scripts/sqlx-prepare.ps1` (or `.sh`) and commit `.sqlx/`: builds check queries against that directory, not a database (D28). Never edit a migration that has been merged; add a new one.
- Nothing may need OpenSSL, CMake or NASM to build: TLS stays on rustls with ring, crypto on pure-Rust backends (D12).
- A solver applies no migrations. The API owns the schema (D32).
- In a handler, the argument that says who may call it (`AuthUser`, `Staff` or `Admin`) comes first, so the gate is checked before the body is read (D41).
- A new endpoint needs a row in `MATRIX` in `crates/routeforge-api/src/app/tests.rs`. After changing a handler or a type the API sends or receives, run the tests with `UPDATE_OPENAPI=1`, read the diff of `crates/routeforge-api/openapi.json`, and commit it (D43).
- Every error the API sends is an `ApiError`, which leaves as `application/problem+json`. The cause of a `500` goes to the log and never into the response (D39).
- A job's heartbeat and its search belong to the job's future. Put either into a task of its own and a job that is dropped keeps its lease, or its thread (D34).
- `rust-toolchain.toml` and the base image in the `Dockerfile` name the same compiler. Change them together (D10).

## Dependencies

Versions are pinned in the root `Cargo.toml` and `Cargo.lock`. Several had recent major releases (sqlx 0.9, utoipa 6, rand 0.10, jsonwebtoken 11, argon2 0.6, criterion 0.8), so check docs.rs for the pinned version. `cargo info <crate>` lists a crate's features.

The project is developed on Windows. Keep helper scripts in both PowerShell and bash, and files in LF.
