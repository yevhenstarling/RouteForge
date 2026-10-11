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

From Phase 3 on there are images too. After a change to the dependencies or to the `Dockerfile`, build them: `docker compose build`. Run that from PowerShell. From Git Bash or `cmd` it can end with `additional privileges requested: pass "--allow=fs.read=Dockerfile"`, which means that the shell's working directory is spelled with a lower-case drive letter; `BUILDX_BAKE_ENTITLEMENTS_FS=0` in the environment gets round it (see the traps in `docs/plan.md`).

## Running it

The whole stack, as somebody who has just cloned the repository gets it (D54):

```sh
docker compose up --build --scale solver=4
```

The page with the map is then at `http://127.0.0.1:8080/` and Swagger UI at `http://127.0.0.1:8080/docs/`. The admin is `admin@example.com` with the password `routeforge`, unless `.env` says otherwise. On a database with no depot and no courier, an admin gets a button on the page that sets up a demo city.

`scripts/demo.ps1` (or `.sh`, which needs `curl` and `jq`) makes the scaling run and the failure run on that stack (D57). It plans tomorrow, and on that day it cancels and discards the plans that are in its way.

An image holds what was in the working tree when it was built. After a change, `--build` again.

The compose file names its project `routeforge`, so `docker compose` and the scripts act on the development stack wherever they are run from, and `docker compose down -v` there deletes the development database. To try something on a database of its own, set `COMPOSE_PROJECT_NAME`, `POSTGRES_PORT` and `API_PORT` in the shell first, for example to `routeforge-try`, `5433` and `8081`.

### The API on the host

For work on the API it is quicker to run it outside Docker, beside the database and the solvers of the stack. Both want port 8080, so stop the one in the stack first, and the bot that waits for it: `docker compose stop api bot`. The API applies the schema when it starts, and a solver started against a database without it waits (D32). Its settings come from the shell:

```powershell
$env:DATABASE_URL = 'postgres://routeforge:routeforge@127.0.0.1:5432/routeforge'
$env:JWT_SECRET = 'any text of at least thirty-two bytes'
$env:ADMIN_EMAIL = 'admin@example.com'; $env:ADMIN_PASSWORD = 'a password of your own'
cargo run -p routeforge-api
```

In bash it is `export NAME=value` for each. The API does not read `.env`. The settings are listed at the top of `crates/routeforge-api/src/main.rs` and in `.env.example`.

The page is compiled into the API (D52), so a change to `web/index.html` shows after the API has been built and started again.

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
- A job's heartbeat, its search and the announcer of its progress belong to the job's future. Put one of them into a task of its own and a job that is dropped keeps its lease, or its thread (D34). An event stream of the API is likewise a value its response owns, with no task behind it (D50).
- A change to a plan or to one of its jobs goes through `db::queue`, which tells whoever watches the plan, in the transaction of the change. A new way to change either has to tell them too, with the plan's id and nothing else as the payload (D48).
- A file added under `web/` needs a row in `FILES` in `crates/routeforge-api/src/page.rs`. Files under `web/vendor/` are kept as they were released: do not edit them (D52).
- The page asks no other host for anything but map tiles, and puts text from the API into the document as text, never as HTML (D52). After changing `web/index.html`, try it in a browser: the tests check that the page is served and what it loads, not what it does.
- `rust-toolchain.toml` and the base image in the `Dockerfile` name the same compiler. Change them together (D10).
- `docker compose up` has to work on a clone with no `.env`. A variable the compose file hands to a service needs a fallback there when the binary has no default for it. A test in `crates/routeforge-api/src/config.rs` holds the fallbacks of the `api` service to what the API starts with (D54).
- A service is the first process in its container, and the first process is not ended by a signal it does not listen for. A binary has to hear SIGTERM itself, or stopping the stack waits ten seconds for it (D35, D51, D56).
- The runtime image gets no tools. What a health check needs to ask is asked by the binary itself, as `routeforge-api healthcheck` does (D55).

## Dependencies

Versions are pinned in the root `Cargo.toml` and `Cargo.lock`. Several had recent major releases (sqlx 0.9, utoipa 6, rand 0.10, jsonwebtoken 11, argon2 0.6, criterion 0.8), so check docs.rs for the pinned version. `cargo info <crate>` lists a crate's features.

The project is developed on Windows. Keep helper scripts in both PowerShell and bash, and files in LF.
