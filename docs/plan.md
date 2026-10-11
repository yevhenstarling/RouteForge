# RouteForge: implementation plan

Phase status is in the table under "Phases at a glance". The reasons behind the decisions are in [architecture.md](architecture.md); the approved description is in [description.md](description.md).

## Context

RouteForge is the approved capstone for the Rust bootcamp: a backend that turns a day's parcel orders into optimized courier routes and spreads the NP-hard part over horizontally scalable workers. The approved description is the contract: one Cargo workspace of five crates, PostgreSQL as both data store and job queue, actix-web, sqlx, a single Leaflet page, and one `docker compose up`.

The repository started empty, so everything is new code. There is no fixed due date, so the phases are in priority order with a cut line that can move. PostgreSQL is assumed throughout, because the queue design names `SKIP LOCKED` and `LISTEN/NOTIFY`.

## Decisions the plan rests on

Confirmed with the project owner:

- **Order model.** One order is one stop: a delivery from the depot or a pickup back to it. Couriers start and end at the depot. This is capacitated vehicle routing, which is what sweep + 2-opt/Or-opt/SA solves.
- **Job unit.** A plan request enqueues one `plan` job. A worker runs sweep clustering and, in the same transaction, enqueues one `route` job per cluster. Each worker then optimizes one courier's route with its own time budget, so more workers finish a plan sooner.
- **Docker.** Docker Desktop with the WSL2 backend.

Derived from the description:

- **Crate boundaries.**
  - `routeforge-tsp`: no I/O, no async, no workspace dependencies. It works on a distance matrix and planar points and knows nothing about lat/lon, orders or couriers.
  - `routeforge-domain`: domain types and request/response DTOs, geo helpers, the `DistanceProvider` trait with its Haversine implementation, and, behind a `db` feature, the sqlx repositories, migrations and job queue. Persistence lives here because both `api` and `solver` need it and the workspace is fixed at five crates.
  - `routeforge-api`: depends on `domain` only. It never links solver code.
  - `routeforge-solver`: depends on `domain` and `tsp`; owns the OSRM client.
  - `routeforge-bot`: talks to `api` over HTTP only; uses `domain` without `db` for the DTOs.
- **Costs are integer metres** (`u32` matrix). Move deltas are exact, local search cannot loop on float noise, and property tests can assert that incremental cost equals recomputed cost.
- **Anytime and deterministic.** The RNG is a named seeded generator (ChaCha8). Annealing cools by iteration count, in fixed-length epochs with reheats; the deadline and the cancel flag only cut that trajectory short. Same seed plus same iteration budget gives an identical route. Tests use iteration budgets, not wall-clock time.
- **Queue guarantee.** Delivery is at-least-once: a lease can expire and the job is retried. Completion is fenced: results are written in the same transaction that marks the job done, and only if the worker still holds the lease. Lease times use the database clock, never the worker's.
- **Capacity, version 1.** A route is feasible when the sum of all its demands, pickups and deliveries together, fits the courier's capacity. This is stricter than mixed routes need and always safe. Tracking the real load along the route is in Phase 8.
- **Offline.** Haversine by default, Leaflet vendored into the repo, map tiles optional (routes still draw without them), and the bot idles when its tokens are absent.
- **Bot authority.** The bot calls the API with the Telegram user's own short-lived JWT, so the model can never do more than that user's role allows. Anything with a side effect needs a button confirmation handled in code.

## Target layout

```
RouteForge/
├─ Cargo.toml, rust-toolchain.toml, .env.example
├─ .cargo/config.toml       builds read query metadata from .sqlx/
├─ Dockerfile, docker-compose.yml
├─ crates/
│  ├─ routeforge-tsp/       pure algorithms; benches/, examples/
│  ├─ routeforge-domain/    types, geo, db + queue (feature `db`); migrations/
│  ├─ routeforge-api/       actix-web binary
│  ├─ routeforge-solver/    worker binary
│  └─ routeforge-bot/       teloxide binary
├─ web/                     index.html, vendor/leaflet
├─ scripts/                 sqlx-prepare.ps1, sqlx-prepare.sh; later demo.ps1, demo.sh
├─ docs/                    description.md, plan.md, architecture.md
└─ .sqlx/                   offline query metadata (committed)
```

## Phases at a glance

| # | Phase | What it lets you show | Size | Status |
|---|---|---|---|---|
| 0 | Skeleton and tooling | Empty workspace builds, lints and tests | S | done locally; CI not run yet |
| 1 | Algorithms (`routeforge-tsp`) | Cost falling stage by stage on a random instance; benchmarks | L | done |
| 2 | Schema and job queue (`routeforge-domain`) | Queue tests: exclusive claim, lease expiry, fenced completion | L | done |
| 3 | Solver worker (`routeforge-solver`) | Workers draining a plan and surviving a killed worker | M | todo |
| 4 | API (`routeforge-api`) | The whole flow through Swagger UI at `/docs` | L | todo |
| 5 | Live updates and map | Routes appearing and improving live in the browser | M | todo |
| 6 | One-command stack, scaling, failure demo | `docker compose up --scale solver=4` from a fresh clone | M | todo |
| 7 | Telegram bot (`routeforge-bot`) | Placing an order by chat | M | todo |
| 8 | OSRM, hardening, presentation | Road distances, benchmark report | M | todo |

**Cut line.** Phases 0 to 6 are the core claim of the project. Phase 7 is in the approved description, so it stays, but it comes last because nothing depends on it; its minimum is two tools (create an order, list my orders). Phase 8 is where things get dropped first: OSRM is optional by the description.

## Phase 0: skeleton and tooling

Delivered:

- Root `Cargo.toml`: members `crates/*`, edition 2024, `[workspace.lints]` (no `unsafe`; no `unwrap`, `dbg!` or `todo!` outside tests) and `[workspace.dependencies]` pinned to the releases current on 2026-10-10: actix-web 4.15, sqlx 0.9, tokio 1.53, utoipa 6, utoipa-swagger-ui 10, jsonwebtoken 11, argon2 0.6, teloxide 0.17, rand 0.10, criterion 0.8, proptest 1.12. reqwest is on 0.12, not 0.13, to match teloxide; see D12 in the decision log.
- `rust-toolchain.toml` pinned to 1.99.0. sqlx 0.9 needs 1.94 or newer.
- Five crates with stubs, wired to the dependencies they will use, so the feature choices are already proven to build on Windows.
- `.gitattributes` (LF endings), `.dockerignore`, `.env.example`, and `docker-compose.yml` with only `db`.
- GitHub Actions: `fmt --check`, `clippy -D warnings`, the bot built without the `db` feature, tests with a Postgres service.
- `docs/description.md`, this plan, `docs/architecture.md`, and `CLAUDE.md`.
- `.cargo/audit.toml` with one justified ignore (D12).

Not verified yet, because it needs something outside the repository:

- The CI workflow has not run. It runs on the first push of `dev` or the first pull request. Phase 2 added two steps to it, which are as untested as the rest.

## Phase 1: algorithms (`routeforge-tsp`)

Delivered:

- Types: `DistanceMatrix` (flat `Vec<u32>`, symmetric by construction), `Problem` (depot, stops with planar point and demand, courier capacities), `Tour`, `Clustering`.
- Construction and local search: nearest neighbour, 2-opt, Or-opt (segments of 1 to 3), alternated until neither improves.
- Simulated annealing over 2-opt, relocate and swap moves, keeping the best tour seen.
- Run control shared by all stages (`Control`): iteration budget and/or deadline, an `AtomicBool` cancel flag, a progress callback.
- Sweep clustering by capacity, from up to 36 start positions, keeping the best. Stops that fit no courier are returned as unassigned, never silently dropped.
- Two public entry points, matching the two job kinds: `cluster(&Problem) -> Clustering` and `optimize_route(&DistanceMatrix, seed, &mut Control) -> Tour`.
- Tests: proptest properties (every stop exactly once; capacity never exceeded; local search never worsens and ends 2-opt- and Or-opt-optimal; incremental delta equals full recomputation; same seed and budget give the same tour; a larger budget never gives a longer one; cancel and deadline are honoured), a brute-force oracle for up to 9 stops, and three TSPLIB instances with their published optimal tours as a quality regression (eil51, berlin52, kroA100).
- Criterion benchmarks per stage at 50, 200 and 1000 stops: `cargo bench -p routeforge-tsp`.
- An example that plans a seeded random day and prints the total length after each stage: `cargo run -p routeforge-tsp --release --example stages`.

What later phases need from it, with the reasons in the decision log:

- Phase 3: a route with four or more stops is annealed until its budget, its deadline or the cancel flag stops it (D18), so the solver's time budget per route sets how long a plan takes.
- Phase 3: an iteration budget counts local search moves as well (D15). Routes of 250 stops need more than a million before annealing starts.
- Phase 3: a `Control` borrows the cancel flag and owns the progress callback, so it is built inside the `spawn_blocking` closure, not moved into it.
- Phase 8: an OSRM table has to be symmetrized before it becomes a `DistanceMatrix` (D14).

## Phase 2: schema and job queue (`routeforge-domain`)

The first phase that needs Postgres.

Delivered:

- Migrations inside the crate, `0001_domain.sql` and `0002_queue.sql`: `users`, `depots`, `couriers`, `plans`, `orders`, `routes`, `route_stops`, `jobs`, `workers`. A partial index over the unfinished jobs. A unique index that allows one plan in progress per service date. Unique constraints that keep an order on one route and give a courier one route per plan.
- Types: an id type per table, `Role`, the order, plan and job status enums, `JobPayload` (a tagged enum stored as JSONB), `LatLon`, and the entities with their `New…` inputs, which are the DTOs.
- Geo: `haversine_m`, `Projection` for the sweep, `DistanceProvider` with `Haversine`, and `DistanceTable`.
- Repositories with compile-time checked queries, one module per table under `db`: `users`, `depots`, `couriers`, `orders`, `plans`, `routes`.
- The queue, `db::queue`: `enqueue` with `NOTIFY`, `claim`, `heartbeat`, `begin_complete` with its `Completion`, `fail`, `release`, `cancel_plan`, and two readers. `db::workers` is the registry.
- Query metadata in `.sqlx/`, `SQLX_OFFLINE` set in `.cargo/config.toml`, `scripts/sqlx-prepare` for both shells, and a CI step that fails when the metadata is stale.
- Tests: 90 in the crate, 62 of them against PostgreSQL through `#[sqlx::test]`. The six this plan asked for are among them: eight claimers drain 60 jobs and each job is claimed once; an expired lease is claimed again; a worker that lost its job can do nothing to it; cancelling stops the jobs and frees the orders; three failed attempts fail the job and its plan; route jobs completing at the same moment mark the plan done once.
- The queue tests were tried against six faults put into the queue on purpose: the plan lock taken out of a completion, and out of a final failure; the comparison of the attempt number weakened; `SKIP LOCKED` removed; an exhausted job claimable again; and nobody burying. Each made the test written for it fail, the first two with PostgreSQL's "deadlock detected".

Two names differ from what this plan first said, for reasons in the decision log: `complete` became `begin_complete` and a `Completion` (D25), and `request_cancel` became `cancel_plan` (D23).

What later phases need from it:

- Phase 3: the `plan` job does all its work inside its completion: `queue::begin_complete`, `orders::lock_pending`, cluster, `orders::mark_planned`, one `queue::enqueue` per cluster with the plan job's request id, `plans::start_routing`, `commit`. A route job completes with `routes::insert` and `plans::route_finished`, both through `completion.conn()`.
- Phase 3: a worker passes spans of time and never a timestamp (D6). It should send its heartbeat well inside the lease, and stop the solve on `Heartbeat::Cancelled` and on `Heartbeat::Lost` alike.
- Phase 3: `queue::claim` returns `DbError::Unreadable` for a job this build cannot read, after counting the attempt as failed. Log it and claim again.
- Phase 3: a plan's seed is a `u32`, so it survives JSON in a browser. The seed of a route job is a `u64`. The plan's seed in the high half and the number of the cluster in the low half is one way to derive it.
- Phase 3: how long a route may be optimized is not in the schema. Add it with a migration when the solver's settings are designed.
- Phases 3 and 4: database tests in other crates use `#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]`. A test pool has five connections, so a test must not hold more than that at once.
- Phase 4: `plans::submit` is the whole "create a plan" step. `DbError::ActivePlanExists` and `DbError::EmailTaken` are the two conflicts to turn into `409`. `orders::cancel` returns `false` for an order that is no longer pending.
- Phase 4: the DTOs have no OpenAPI derives yet. Add `utoipa` to the domain crate behind a feature. `orders::list` has no paging, and the queue has no depth statistics.
- Phase 4: decide whether a plan that is done may be replaced (D20).

## Phase 3: solver worker (`routeforge-solver`)

- Worker loop: connect with retry, register, `LISTEN` plus a jittered polling fallback, claim, run, complete or fail. One job at a time per instance by default, so capacity scales with the instance count.
- `plan` job: load the date's orders, active couriers and depot; project; `cluster`; insert the `route` jobs and update the plan inside the completing transaction.
- `route` job: build the matrix through `DistanceProvider`; run `optimize_route` in `spawn_blocking`; a heartbeat task extends the lease, publishes progress from a `watch` channel, and sets the cancel flag on a cancel request or a lost lease; fenced completion writes `routes` and `route_stops` and increments the plan's done counter.
- Graceful shutdown on Ctrl-C or SIGTERM: stop claiming, cancel the solve, release the job back to the queue.
- `tracing` spans carrying `worker_id`, `job_id`, `plan_id`, `request_id`.
- `Dockerfile` (multi-stage with cargo-chef, `SQLX_OFFLINE=true`) and a `solver` service in compose, so image builds are exercised from here on.
- Tests: headless end to end (seed orders, enqueue a plan, run three in-process workers, every order routed exactly once within capacity); abort a worker mid-solve and the plan still completes after lease expiry; cancel stops jobs within one heartbeat.
- **Done when:** those tests pass and `docker compose up --scale solver=3` starts three idle workers.

## Phase 4: API (`routeforge-api`)

- Skeleton: config from env, pool, migrations at startup, request-id middleware (accept or generate `X-Request-Id`, echo it, put it in the span), problem+json errors, `/healthz`, `/readyz`.
- Auth: register and login, argon2id hashing off the async threads, JWT with a role claim, an `AuthUser` extractor, role and ownership checks. Admin seeded from env.
- Resources: orders (clients see their own; operators and admins see all), couriers, depot, users.
- Plans: create (plan row, `plan` job and `NOTIFY` in one transaction; `409` when the date already has an active plan; the request id is stored on the job), get, list routes with ordered stops, cancel.
- System stats: queue depth by status, workers alive and busy.
- OpenAPI through utoipa on every DTO and handler; Swagger UI at `/docs` with vendored assets.
- Demo data: an admin endpoint that generates N random orders around the depot from a seed. This lives in the API so no sixth crate is needed.
- Tests: role matrix (each role against each endpoint), ownership, validation, enqueue atomicity, OpenAPI snapshot.
- **Done when:** the whole flow works from Swagger UI: log in, create orders, create a plan, read back the routes.

## Phase 5: live updates and map

- API: one `PgListener` feeding a `tokio::sync::broadcast` channel; SSE endpoints for one plan's events and for system stats.
- Workers publish status and throttled progress with `NOTIFY`.
- `web/index.html`, served by the API: login, date picker, generate demo orders, plan, cancel; a Leaflet map with the depot, stops and one coloured line per courier; per-route progress; queue depth and worker list.
- Leaflet vendored under `web/vendor/`; the tile layer is optional.
- **Done when:** pressing Plan shows routes appear and improve live, with the network unplugged as well.

## Phase 6: one-command stack, scaling and failure demo

- Compose: `db`, `api`, `solver` (no fixed name or ports, so it scales), `bot`; healthchecks and `depends_on: service_healthy`.
- Scaling run: the same seeded plan with 1 and with 4 solvers, time to completion recorded.
- Failure run: `docker kill` a solver mid-solve; the lease expires and another worker finishes. Scripted in `scripts/demo.ps1` and `scripts/demo.sh`.
- **Done when:** a fresh clone needs only `docker compose up --build --scale solver=4`.

## Phase 7: Telegram bot (`routeforge-bot`)

- API side: `users.telegram_id`, and a service-key endpoint that maps a Telegram user to a RouteForge user and returns a short-lived JWT for them.
- Bot: teloxide long polling, so no public URL is needed; bounded per-chat history in memory.
- Claude client over `reqwest` against the Messages API (there is no official Rust SDK), with a hand-written tool loop: capped iterations, all tool results of a turn returned in one message, assistant content echoed back unchanged, `refusal` and `max_tokens` stop reasons handled.
- Tools with strict schemas. Clients: `create_order`, `list_my_orders`, `get_order`, `cancel_order`. Operators also: `list_pending_orders`, `create_plan`, `get_plan_status`.
- Location comes from Telegram's share-location message.
- Model id from env, default `claude-opus-5-5`. Running a cheaper model is a config change and the owner's call.
- Tests: the tool loop against a scripted fake model; request and response shapes with wiremock.
- **Done when:** sharing a location and writing "deliver two parcels here tomorrow" produces, after confirmation, an order visible on the map.

## Phase 8: OSRM, hardening, presentation

- `OsrmProvider` in the solver using OSRM's table service, falling back to Haversine on error; an `osrm` compose profile.
- Real load tracking for mixed pickup and delivery routes.
- Address geocoding for the bot.
- README with benchmark numbers, quality against time, and the scaling table; final architecture doc; `cargo audit` in CI.

## Known traps

- **sqlx compile-time queries** are checked against the metadata in `.sqlx/`, not against a database: `.cargo/config.toml` sets `SQLX_OFFLINE`. After changing a query or a migration, run `scripts/sqlx-prepare` and commit the result, or the build fails with "no cached data for this query". The script needs `sqlx-cli`, and the command that installs it is at the top of the script. To have the compiler ask the live database while writing queries, set `SQLX_OFFLINE=false` in the shell.
- **An applied migration must not be edited.** sqlx keeps a checksum of each and refuses one that has changed. Add a new migration instead.
- **`localhost` on Windows with Docker Desktop.** It resolves to `::1` first, and a connection to the published port hangs there for about 20 seconds before the client falls back to IPv4. `DATABASE_URL` therefore says `127.0.0.1`.
- **Native build tools.** The development machine has no CMake or NASM. Phase 0 chose features that need neither (D12); a new dependency must not bring OpenSSL or aws-lc back in.
- **Recent major releases.** sqlx 0.9, utoipa 6, rand 0.10, jsonwebtoken 11, argon2 0.6 and criterion 0.8 are all newer than most tutorials. Read docs.rs for the pinned version.
- **2-opt assumes a symmetric matrix**, and OSRM road distances are not symmetric. `DistanceMatrix` rejects an asymmetric table (D14), so the solver symmetrizes for the search and reports the real directed length itself.
- **Two route jobs finishing at once** must not each see the other as unfinished. The plan row's counter update serializes them; Phase 2 has a test for it.
- **`NOTIFY` can be missed** on a listener reconnect, and an expired lease sends none, so workers always poll as well. Payloads are capped near 8 kB, so they carry ids and small numbers only.
- **Browser `EventSource` cannot send an `Authorization` header.** The page reads the SSE stream with `fetch`.

## Needed from the project owner

- `sqlx-cli` on any machine where a query or a migration is changed. The command that installs it is at the top of `scripts/sqlx-prepare`.
- Before Phase 7: a Telegram bot token from @BotFather and an Anthropic API key.
- If the bootcamp grades against a rubric (coverage, report, presentation format), it gets folded into Phase 8.

## Verification

1. `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` with the compose `db` up, and `cargo bench -p routeforge-tsp`.
2. Fresh clone: `docker compose up --build --scale solver=4`, open `http://localhost:8080`, log in as the seeded admin, generate 300 orders, press Plan. Routes appear and improve live; `/docs` lists every endpoint.
3. Determinism: run the same plan twice with the same seed and an iteration budget; the routes are identical.
4. Scaling: the same plan with 1 solver and with 4; completion time drops until workers outnumber routes.
5. Failure: `docker kill` one solver mid-solve; within the lease time another worker takes over and every order is still routed exactly once.
6. Tracing: take the `X-Request-Id` of the plan request and find it in the logs of the API and of every solver that worked on that plan.
7. Offline: disconnect the network and repeat step 2. Routes draw without map tiles and the bot idles.
8. Bot: place an order by chat and see it on the map.
