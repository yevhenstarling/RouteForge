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
- **Planning a date again.** A plan that is done can be discarded: its routes are deleted, its orders are pending again, and the date is planned afresh. Without a discard, a later plan for the date takes only the orders that are still pending (D30).
- **Route budget.** A plan says how long each of its routes may be searched for: a time, a number of moves, or both (D31).
- **Migrations.** The API applies them. A solver applies none and waits until the schema is there (D32).
- **Phases keep to their list.** When Phase 4 was planned, the image and compose service of the API were offered ahead of Phase 6, and a list of plans ahead of Phase 5. Both stayed where this plan has them.
- **Routes are drawn while they are searched for.** The search hands out its best tour, the worker announces it, and the map shows it as a line that moves. The progress of a job as two numbers was not enough for "improve live" (D49).
- **The page has three controls beyond the list of Phase 5:** discarding a done plan, a demo setup for an empty database, and a budget and a seed for a plan (D52).

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
| 3 | Solver worker (`routeforge-solver`) | Workers draining a plan and surviving a killed worker | M | done |
| 4 | API (`routeforge-api`) | The whole flow through Swagger UI at `/docs` | L | done |
| 5 | Live updates and map | Routes appearing and improving live in the browser | M | done |
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

- The CI workflow has not run. It runs on the first push of `dev` or the first pull request. Phase 2 added two steps to it and Phase 3 a job that builds the solver image, and they are as untested as the rest.

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
- Phase 4: discarding a plan that is done is decided (D30) and not built. The undo exists as `end_plan` in `db::queue`, for plans in progress only. Discarding needs a `discarded` plan status, so a new migration for the two `CHECK`s on `plans` that list the statuses, and a `queue::discard_plan` beside `cancel_plan`.

## Phase 3: solver worker (`routeforge-solver`)

Delivered:

- In `routeforge-domain`, what the worker needed: migration `0003_route_budget.sql` and `RouteBudget` on `NewPlan` and `Plan` (D31), `orders::list_by_ids`, and `db::is_migrated`.
- The worker loop: connect with retry, wait for the schema, register, claim until nothing is due, then wait for a notification, for a jittered poll interval or to be told to stop (D33). One job at a time per worker, and `SOLVER_CONCURRENCY` workers per process, one by default.
- The `plan` job, all in the transaction that completes it: lock the pending orders of the date, project them round the depot, `cluster`, mark the orders planned, enqueue a route job for each cluster with the plan job's request id, move the plan to `routing` (D36).
- The `route` job: the matrix through `DistanceProvider`; `optimize_route` on a blocking thread under the plan's budget; a heartbeat beside it that extends the lease, stores the progress from a `watch` channel, and stops the search when the plan was cancelled or the job was taken (D34); a fenced completion that writes the route and counts it.
- Graceful shutdown on Ctrl-C or SIGTERM: stop claiming, drop the job, give it back (D35).
- A failed attempt is reported with its reason, and trouble with the database outside a job is waited out (D37).
- `tracing` spans with the ids of the worker, the job, the plan and the request. `LOG_FORMAT=json` for JSON lines.
- `Dockerfile` with cargo-chef and a `solver` target, a `solver` service in compose, the solver's settings in `.env.example`, and a CI job that builds the image (D38).
- Tests: 38 in the crate, 19 of them against PostgreSQL, 9 of those with workers running inside the test. The three this plan asked for are among them: three workers route every order once within capacity; a worker is aborted in the middle of a route and the plan completes once its lease has run out; a cancel stops the searches within a heartbeat. Beside them: a job taken by another worker is given up; a route that outlasts its lease is not taken over; a worker told to stop gives its job back; a job that keeps failing fails its plan with the reason; the same day planned twice gives the same routes; a solver waits for the schema.
- The tests were tried against eleven faults put into the worker on purpose: notifications not counted, no polling, the answer of a heartbeat ignored, no heartbeat, no release, a dropped search left running, tour nodes mapped to the wrong orders, a failure not reported, the request id not passed on, seeds that differ from run to run, and a `plan` job that fails where it should stand aside. Each made the test written for it fail. The seed fault did so only after its test was given a route long enough for the seed to matter.
- Tried by hand in containers, in a database made for it: three solvers waited for the schema and started when it was applied; a plan of 100 orders was done in 24 seconds with every order on one route, although one solver was killed in the middle of a route; a solver stopped with `docker stop` gave its job back within half a second.

Two things differ from what this plan first said, for reasons in the decision log:

- The heartbeat is not a task. It runs beside the search in the job's own task, so that a job that goes away cannot leave its lease behind (D34).
- A solver applies no migrations and waits for the schema (D32). Until the API exists a fresh database is migrated by hand, so `docker compose up --scale solver=3` starts three idle workers only on a database that `scripts/sqlx-prepare` or `cargo sqlx migrate run --source crates/routeforge-domain/migrations` has been run against. On any other the three wait, and say so.

What later phases need from it:

- Phase 4: a `NewPlan` needs a `budget`. The API has to give it a default and an upper limit (D31), and `RouteBudget` needs an OpenAPI schema like the other DTOs.
- Phase 4: the API runs `db::MIGRATOR` when it starts, which ends the migrating by hand. The comment on the `solver` service in `docker-compose.yml` and the note in `CLAUDE.md` then go.
- Phase 4: `workers::list` gives a row per worker, and a process can run several (D33). A worker marks itself as seen every `SOLVER_HEARTBEAT_SECS`, 5 by default, so "alive" is a few times that.
- Phase 4: the migration for `discarded` (D30) is number 0004.
- Phase 5: `jobs.progress` is as fresh as the last heartbeat. The search reports every 16 384 moves into a `watch` channel that `route_job::heartbeats` reads, which is the place to publish from more often.
- Phase 6: a service gets into the image by adding its package to the two `--package` lines of the builder stage and a target of its own (D38). The services need no start order, since a solver waits for the schema.
- Phase 6: after a `docker kill` the job is taken over when the lease has run out and an idle worker has polled: 30 seconds and then up to 6 more by default. `SOLVER_LEASE_SECS` and `SOLVER_HEARTBEAT_SECS` shorten that for a demo. With a budget of time, a plan takes as many budgets as it has rounds: its routes divided by the workers, rounded up.
- Phase 8: `main.rs` hands `Haversine` to `worker::run`, and an OSRM provider goes in its place. `route_job::search_matrix` is where its table has to be made symmetric, and `route_job::complete` stores `tour.cost()` as the distance, which then has to become the directed length (D14).

## Phase 4: API (`routeforge-api`)

Delivered:

- In `routeforge-domain`, what the API needed: migration `0004_discarded.sql`, `PlanStatus::Discarded` and `queue::discard_plan` (D30); paging in `orders::list` through `after` and `limit` on `OrderFilter` (D44); `queue::depth`; the `openapi` feature, with a schema on every type the API sends or receives; and the module `dto` with the bodies that are not entities (D43).
- Start-up: settings from the environment, the pool, the migrations, the first admin from `ADMIN_EMAIL` and `ADMIN_PASSWORD`. A database that does not answer ends the start (D47).
- A request id on every request: taken from `X-Request-Id` when it is well formed, made otherwise, echoed, in the span, and stored on the jobs of a plan (D42).
- Errors as `application/problem+json`, the framework's own included (D39). `/healthz` and `/readyz`.
- Auth: register and login, Argon2id on blocking threads, an HS256 token with the role in it, and the extractors `AuthUser`, `Staff` and `Admin` (D40, D41).
- Orders, which a client sees and withdraws as their own and operators and admins see all; couriers; the depot; users and their roles.
- Plans: create, with the plan row, the `plan` job and its `NOTIFY` in one transaction, a `409` when the date has a plan in progress, and the request id on the job; get; the routes with their stops in calling order; cancel; discard. A plan that names no budget or no seed is given one (D45).
- System stats: jobs by status, and the workers that were seen lately with the job each holds.
- The OpenAPI document from the handlers and the types, committed as `crates/routeforge-api/openapi.json`, and Swagger UI at `/docs/` out of the binary (D43).
- Demo data: `POST /api/demo/orders`, from a seed (D46).
- Tests: 71 in the crate, 39 of them against PostgreSQL, and 10 more in the domain crate. The five this plan asked for are among them: the role matrix, each role and nobody against each endpoint, with the endpoints read from the document so that none can be left out; ownership; validation; the plan and its job appearing together or not at all; and the snapshot of the document. Beside them: tokens that are forged, expired or altered; the request id on the response, on the job and in the log; paging; the demo seed; the stats; the admin made once; and the whole flow with a stand-in for the solver, which the API may not link.
- The tests were tried against 29 faults put into the code on purpose, six of them in the domain crate. Each made a test fail. Among them: a signature or an expiry not checked, a role gate removed, a list not forced to the client's own orders, an order withdrawn before it is checked whose it is, the body read before the token, the request id not passed on, a plan stored outside the transaction of its job, an endpoint added without a row in the matrix.
- Tried by hand, on a database that had just been created, with the API on the host and two solvers in containers: the solvers waited for the schema and started when the API had applied it; through the API an admin logged in, set the depot, added four couriers and generated 300 orders, and a client placed one; a plan put the 301 orders onto four routes, each order on exactly one and no courier over its capacity; the plan was discarded and planned again with the same seed and a budget of moves, and the routes were identical; a plan with a minute a route was cancelled while routing; and the `X-Request-Id` of the plan request was in the API's log and in the log of both solvers.

Where it differs from what this plan first said:

- There are three extractors and not one. `AuthUser` is anybody with a token, and `Staff` and `Admin` are the gates (D41).
- Two things were left where this plan has them, although they were considered: the API has no image and no compose service yet, and plans cannot be listed.
- The code was committed as one commit for the domain crate and one for the API, because the API's parts do not build without each other.

What later phases need from it:

- Phase 5: plans cannot be listed. The page needs `GET /api/plans?service_date=` to find the plan of a date, and `plans::list` under it. Whoever asked for a plan has its id from the answer, and nobody else has it.
- Phase 5: the API serves nothing at `/`. Whatever matches no route is answered `404` as a problem by `error::not_found`, the default service in `app.rs`, so the page and `web/vendor` have to be registered before it.
- Phase 5: a token lasts an hour. The page has to send the user back to the login on a `401`. The login answers with the user and the role, so the page need not read the token.
- Phase 5: `GET /api/plans/{id}/routes` gives the routes that are stored so far, so it grows while a plan is routing. `GET /api/system/stats` is the queue depth and the worker list.
- Phase 5: the request span stays open while a body is streamed, and "handled the request" is logged when the response starts, not when the stream ends.
- Phase 6: the image needs `routeforge-api` in the two `--package` lines and a target (D38). In a container the API needs `API_BIND=0.0.0.0:8080`, a `JWT_SECRET`, for which the binary has no default, and `ADMIN_EMAIL` with `ADMIN_PASSWORD` for the admin the verification logs in as.
- Phase 6: the API ends when the database does not answer (D47), so its service needs `depends_on` with the health of `db`, as `solver` has. `/healthz` and `/readyz` are there for a health check, but the runtime image has no `curl` to ask them with.
- Phase 6: every request is logged at `info`, a health check every few seconds included.
- Phase 6: the scaling run asks for its plans with a budget of moves and the same seed, and discards with `POST /api/plans/{id}/discard`. A budget of moves ends as soon as the moves are made: four routes of a hundred stops with three million moves each were done in a third of a second on two solvers, so the run needs a larger budget, or one of time, to show anything.
- Phase 7: the bot's DTOs are in `routeforge_domain`, with no feature: `Login`, `Session`, `OrderRequest`, `PlanRequest`, `Problem`. The ids of the operations in `openapi.json` are the names of the handlers: `create_order`, `list_orders`, `get_order`, `cancel_order`, `create_plan`, `get_plan`.
- Phase 7: `Tokens` in `auth.rs` signs with one lifetime. The endpoint that gives the bot a short-lived token needs it to take one. The migration for `users.telegram_id` is number 0005.
- Phase 8: hardening that was left out: nothing limits how often a login is tried or how many passwords are hashed at once, and each hash takes 19 MiB. The words of the OpenAPI document are the doc comments, which cite decisions by number.

## Phase 5: live updates and map

Delivered:

- In `routeforge-tsp`: the progress callback of a search is handed the best tour beside the two numbers, and a search reports once more as it returns (D49).
- In `routeforge-domain`: two channels beside the one of the workers, `routeforge_plans` and `routeforge_progress`. Every function of the queue that changes a plan or one of its jobs announces the plan, in the statement or the transaction of the change (D48). `queue::announce_progress`, `RouteProgress` and `RouteJob`. `plans::list` and `PlanFilter` (D53). No migration.
- In the solver: a search under way is announced with its best route, at most every `SOLVER_PROGRESS_MS`, by a third future beside the search and the heartbeat (D49). A worker takes three connections.
- In the API: one connection that listens, and a hub the streams subscribe to. `GET /api/plans/{id}/events` and `GET /api/system/stats/events` as `text/event-stream` (D50). `GET /api/plans?service_date=` (D53). The page at `/` and Leaflet under `/vendor/`, out of the binary (D52). Ctrl-C and SIGTERM heard by the API itself, so that it stops with streams open (D51).
- `web/index.html`: login, date picker, demo orders, plan with a budget and a seed, cancel, discard, and a list of the day's plans. The map has the depot, the stops in the colour of their courier, a dashed line for each route that is searched for and a solid one for each that is stored. Beside it are, for each route, its courier, stops, status, worker, moves and length, and the queue with its workers.
- Leaflet 1.9.4 under `web/vendor/`, and tiles that a switch on the page turns off.
- Tests: 33 more. 21 are in the API, which has 92 now, and 15 of those run against PostgreSQL. 8 are in the domain crate (115), 3 in the solver (41) and 1 in `routeforge-tsp` (65). They hold the queue to announcing every change once and when it commits, the solver to the pace of its announcements and to the route it names in them, a stream to what it sends, to when it reads again and to when it ends, the API to serving every file under `web/`, and the page to naming no host but the tile server.
- The tests were tried against 45 faults put into the code on purpose: 2 in `routeforge-tsp`, 15 in the domain crate, 6 in the solver and 22 in the API and the page. Each made a test fail but one. A search that did not report the tour it returns passed every test, because the searches the tests ran had stopped improving before their budgets ended. A test whose budget ends in the middle of local search was added, and fails for it. Among them: each of the seven places that announce a change gone quiet, notifications of one transaction that no longer fold, the watchers told on the workers' channel, a route too long for a notification sent all the same, a search announced as often as it reports, the stops of a route counted from one, a stream that passes on the searches of every plan, one that does not read again after it may have missed something, one that outlives its token and one that ends a second before it, the streams left open when the API stops, a gate taken off a stream, a file under `web/` without its row, and a script loaded from another host.
- Tried in a browser, on a database made for it, with the API on the host and four solvers in containers. A script drove a headless Edge over the DevTools protocol and took pictures, and the pictures were looked at:
  - On a fresh database: logged in as the seeded admin, set up the demo city, generated 300 orders and pressed Plan. Four routes of 87, 86, 78 and 49 stops were searched by the four solvers for the ten seconds the API gives by default, and stored, 125.29 km in all. The stream of the plan, read by a plain client beside the page, sent 167 events in 11.9 seconds. One search was announced at 38 975 m, a quarter of a second later at 32 642 m, and at 32 548 m from then on.
  - A day of 1200 orders, on couriers large enough for it: routes of up to 353 stops. One went from 81.04 km at its first announcement to 67.34 km at its second, and to 66.38 km after ten seconds. Two announcements of one search were 250 to 256 ms apart.
  - With the tile server blocked in the browser, which is the network unplugged as far as the page can tell: the page said that the tiles could not be loaded, and drew everything else on paper with a grid.
  - A plan cancelled in the middle of its searches was shown as cancelled, with its four jobs stopped and its orders pending again. A second and a half later the four workers were idle. The solvers ran with a heartbeat of two seconds then.
  - A done plan was discarded and the day planned again, twice with seed 7 and 30 million moves a route: 65.21, 65.10, 63.34 and 41.87 km both times. Seed 8 gave 65.40, 65.96, 63.40 and 41.77 km.
  - With logins that last twenty seconds, both streams ended after 21 seconds, and two seconds later the page was back at the login, saying that the login had ended.
  - With a page open the API was sent Ctrl-C. It had stopped 0.33 seconds later, and had logged the end of both streams with the reason.
  - A client saw the depot and their own two orders, and no planning. An address with HTML in it was shown as the text it is.
  - In every run the console of the browser stayed empty, and the only hosts the page asked were the API and the tile server.

Where it differs from what this plan first said:

- The search hands out its tour, and the map draws it while the search runs. The plan had the progress as two numbers. Decided with the project owner (D49).
- The page has three controls more than the list: discard, a demo setup, and a budget and a seed for a plan. Decided by the project owner (D52). The vans of the demo setup are sized for the day that is asked for, which was not asked for and is easy to take out again.
- "Workers publish status" became: whoever changes a plan announces it. The functions of the queue do, so a cancel or a discard through the API is announced like a job that a worker completes (D48).
- The API hears Ctrl-C and SIGTERM itself. An event stream never finishes, and actix-web waits for a response to finish before it stops (D51).

What there is to see depends on the size of the routes (D49). With 300 orders on four vans a route has under a hundred stops and is at its final length a quarter of a second after the search began, so its line appears, jumps once and stands. With 1200 orders the lines go on changing for the ten seconds of the budget.

What later phases need from it:

- Phase 6: the image of the API needs `web/` in the build context, because `page.rs` includes its files when the crate is compiled. It is there: `.dockerignore` does not leave it out. `cargo chef cook` does not need it.
- Phase 6: the API stops within a second of SIGTERM, streams or no streams (D51). `SOLVER_PROGRESS_MS` is passed on to the solvers by compose.
- Phase 6: on a fresh database the page offers an admin "Set up a demo city", which replaces setting the depot and the couriers by hand in verification step 2. Its vans are sized for the number in the Orders field at that moment.
- Phase 6: for the scaling run, a budget of 150 million moves took about three and a half seconds for a route of a hundred stops on the development machine. On routes that short every seed ends at the same lengths, so such a run says nothing about seeds. On routes of 330 stops with 30 million moves the same seed gave the same lengths twice, and another seed gave others. A day of 300 orders shows next to nothing on the map while it is searched. A day of 1200 does.
- Phase 7: the bot's `get_plan_status` can find the plan of a day with `GET /api/plans?service_date=`.
- Phase 7: an order placed through the bot shows on the map when Refresh is pressed. Nothing announces an order, and `routeforge_plans` says nothing of one that no plan has taken. If the map is to show it by itself, placing an order has to notify a channel of its own.
- Phase 8: OSRM. What a search announces as its best length is the length on the matrix it searches, which will be the symmetric one (D14). The length that is stored will be the directed one, so the two will differ a little at the moment a route is stored.
- Phase 8: hardening that was left out. A request that is still running while the API stops may get no database connection: seen once, on a stream that was left open on purpose, and not looked into (D51). Nothing limits how many streams one person holds open. The page has no Content-Security-Policy: its script is inline, and moved into a file of its own it could be held to `script-src 'self'`. The page draws every route from the depot as it is now, not from the depot of the plan (D21). And the page is tested by hand: what a machine checks is that it is served and what it loads.

## Phase 6: one-command stack, scaling and failure demo

- Compose: `db`, `api`, `solver` (no fixed name or ports, so it scales), `bot`; healthchecks and `depends_on: service_healthy`.
- Scaling run: the same seeded plan with 1 and with 4 solvers, time to completion recorded. The plan is discarded between the two runs (D30).
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
- **`NOTIFY` can be missed** on a listener reconnect, and an expired lease sends none, so workers always poll as well, and so do the event streams of the API (D50). A payload has to be shorter than 8000 bytes. A change to a plan is announced by its id alone, and the progress of a search leaves its route out when it would not fit (D48, D49).
- **Browser `EventSource` cannot send an `Authorization` header.** The page reads the SSE stream with `fetch`.
- **An endless response hangs a test that reads its body to the end.** `Api::send` in the API's tests takes what an event stream opens with and drops the rest, and `Api::open` reads a stream one event at a time. Without the first, the test of who may call what would never return from the stats stream.
- **A graceful stop waits for an endless response.** actix-web drains its connections when it is told to stop and cuts no body short. The streams have to be ended first (D51).
- **A token is accepted through the second it expires in.** jsonwebtoken refuses one when `exp` is less than now, not when it is equal. A stream that ended at `exp` could be opened again for the rest of that second, so it ends a second later (D50).
- **Leaflet draws a canvas in the order things were added to it.** Dots that are drawn again cover a line that was there before them. The page draws its lines in SVG, above the canvas of the dots (D52). A tooltip that is given a string takes it as HTML, so the page gives it an element.
- **On Windows, whether a process ignores Ctrl-C is inherited.** A service started from a tool that ignores it cannot be stopped with it, whatever the service does. To try D51 the API was started by a launcher that turns Ctrl-C back on (`SetConsoleCtrlHandler(NULL, FALSE)`), in a console of its own.
- **A `tracing` subscriber for one thread loses lines in tests.** `tracing` remembers for every place that logs whether anybody listens, and with one subscriber that belongs to one thread it may ask a thread that has none. A test that reads the log sets a subscriber for the whole process and picks its own lines out by an id: see `captured_log` in the solver's tests.
- **A fresh database has no schema until an API has started against it.** A solver waits for it and applies none (D32). The API has no service in compose before Phase 6, so until then it is run on the host: `CLAUDE.md` says how. `scripts/sqlx-prepare` applies the migrations to the compose database as well.
- **utoipa leaves things out without a word.** An enumeration whose values come out of a macro as `literal`s is documented by the names of its variants; `web::Path<OrderId>` is read as a struct of parameters; a query struct that is not named in `params(...)` is not described at all. Each compiled and gave a document that was wrong (D43). Read `openapi.json` after changing a type or a handler.
- **A number that has been through JSON may differ in its last digit.** serde_json reads a float fast and not exactly, unless its `float_roundtrip` feature is on. A test that compares a position the API returned with one it computed allows for that.
- **A handler's name is taken.** `#[post("/api/auth/login")]` on `async fn login` makes `login` a type. A `let login = …` in any handler of that module then fails to compile, with a message about mismatched types.
- **sqlx 0.9 takes no SQL that was put together at run time.** `sqlx::query(&format!(…))` does not compile. Tests write their SQL out.
- **Argon2 is slow in a debug build**: a third of a second for one hash. Tests that need accounts store them directly and sign their tokens themselves, and those that log in use `Passwords::cheap()`.

## Needed from the project owner

- `sqlx-cli` on any machine where a query or a migration is changed. The command that installs it is at the top of `scripts/sqlx-prepare`.
- Before Phase 7: a Telegram bot token from @BotFather and an Anthropic API key.
- If the bootcamp grades against a rubric (coverage, report, presentation format), it gets folded into Phase 8.

## Verification

1. `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` with the compose `db` up, and `cargo bench -p routeforge-tsp`.
2. Fresh clone: `docker compose up --build --scale solver=4`, open `http://localhost:8080`, log in as the seeded admin, set up the demo city, generate 300 orders, press Plan. Routes appear and improve live; `/docs` lists every endpoint. With 1200 orders in the field before the city is set up, the routes are long enough to be seen improving for seconds (D49).
3. Determinism: run a plan, discard it (D30), and run it again with the same seed and an iteration budget; the routes are identical.
4. Scaling: the same plan with 1 solver and with 4, discarded in between; completion time drops until workers outnumber routes.
5. Failure: `docker kill` one solver mid-solve; within the lease time another worker takes over and every order is still routed exactly once.
6. Tracing: take the `X-Request-Id` of the plan request and find it in the logs of the API and of every solver that worked on that plan.
7. Offline: disconnect the network and repeat step 2. Routes draw without map tiles and the bot idles.
8. Bot: place an order by chat and see it on the map.
