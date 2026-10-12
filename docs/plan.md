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
- **The stack starts with no settings.** A fresh clone has no key to sign tokens with and no admin. Compose falls back to values for a demonstration, which are in the repository for everybody to read, and the ports are open to the machine alone (D54).
- **The bot is in the stack before it is built.** Its image and its compose service are Phase 6's, with a stub that idles. The bot itself stays Phase 7's (D56).
- **Phase 6 has a quick start in the README, and leaves CI alone.** Starting the stack in CI, and running the demo script there, were offered beside it and not taken.
- **The bot takes buttons and commands, not free text.** A location and a few buttons say what an order needs. Free text through the Claude API, which the description names, is an optional item of Phase 8 and is built only when there is a key to try it with (D58).
- **A Telegram user gets an account of their own, the first time they write to the bot.** A client's, with no address and no password, which an admin can make an operator's. Tying a Telegram user to an account that is there already, by a code, was offered beside it and not taken (D59).
- **Phase 7 has two things beyond its list:** an order shows on the map as it is placed, and the bot takes coordinates that are typed where a location cannot be shared (D61, D63).
- **Phase 7 was merged before it was tried against Telegram.** There was no token for a bot while it was built. It was built and tried against a stand-in for Telegram (D64), and the try with a real bot is the one step of it that is open.
- **Phase 8 is OSRM, the real load, addresses for the bot, and four items of hardening,** beside the README, the architecture pass and `cargo audit` in CI. The four are: a limit on logins, a content security policy for the page, a warning when the API runs on the demonstration's keys, and a plan drawn from its own depot (D68 to D71). The rest of what earlier phases left out stays out, and is among the known limits in the README.
- **The bot looks addresses up only where it is given a service to ask.** The public Nominatim was the other choice. It is named in `.env.example`, with its usage policy, and is nobody's default (D72).
- **A route says what it was measured along, and the map draws it straight.** Lines along the streets were offered beside the label and not taken (D66).
- **There will be no token from Telegram, and there was no key for the Claude API.** So Phase 8 tried the bot as Phase 7 did, against stand-ins, and did not build free text (D58). The bootcamp holds the project to nothing formal beyond the approved description.

Derived from the description:

- **Crate boundaries.**
  - `routeforge-tsp`: no I/O, no async, no workspace dependencies. It works on a distance matrix and planar points and knows nothing about lat/lon, orders or couriers.
  - `routeforge-domain`: domain types and request/response DTOs, geo helpers, the `DistanceProvider` trait with its Haversine implementation, and, behind a `db` feature, the sqlx repositories, migrations and job queue. Persistence lives here because both `api` and `solver` need it and the workspace is fixed at five crates.
  - `routeforge-api`: depends on `domain` only. It never links solver code.
  - `routeforge-solver`: depends on `domain` and `tsp`; owns the OSRM client.
  - `routeforge-bot`: reaches the system through `api` over HTTP only; uses `domain` without `db` for the DTOs. Since Phase 8 it also asks a geocoding service where an address is, if it is given one (D72).
- **Costs are integer metres** (`u32` matrix). Move deltas are exact, local search cannot loop on float noise, and property tests can assert that incremental cost equals recomputed cost.
- **Anytime and deterministic.** The RNG is a named seeded generator (ChaCha8). Annealing cools by iteration count, in fixed-length epochs with reheats; the deadline and the cancel flag only cut that trajectory short. Same seed plus same iteration budget gives an identical route. Tests use iteration budgets, not wall-clock time.
- **Queue guarantee.** Delivery is at-least-once: a lease can expire and the job is retried. Completion is fenced: results are written in the same transaction that marks the job done, and only if the worker still holds the lease. Lease times use the database clock, never the worker's.
- **Capacity.** Until Phase 8 a route was feasible when the sum of all its demands, pickups and deliveries together, fitted the courier's capacity (D7). That was stricter than mixed routes need and always safe. Since Phase 8 the load is followed along the route: a courier leaves with its deliveries and comes back with its pickups, and is within its capacity at every stop (D65).
- **Offline.** Haversine by default, Leaflet vendored into the repo, map tiles optional (routes still draw without them), and the bot idles when its Telegram token is absent. OSRM and the bot's search for an address are off until they are turned on (D67, D72).
- **Bot authority.** The bot calls the API with the Telegram user's own short-lived JWT, so it can never do more than that user's role allows. Anything with a side effect needs a button confirmation handled in code.

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
├─ scripts/                 sqlx-prepare, demo and osrm-prepare, each as .ps1 and .sh
├─ docs/                    description.md, plan.md, architecture.md
├─ .sqlx/                   offline query metadata (committed)
└─ osrm/                    the roads scripts/osrm-prepare fetched (not committed)
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
| 6 | One-command stack, scaling, failure demo | `docker compose up --scale solver=4` from a fresh clone | M | done |
| 7 | Telegram bot (`routeforge-bot`) | Placing an order by chat | M | built; not tried against Telegram yet |
| 8 | OSRM, hardening, presentation | Road distances, benchmark report | L | done; free text for the bot was not built |

**Cut line.** Phases 0 to 6 are the core claim of the project. Phase 7 is in the approved description, so it stays, but it comes last because nothing depends on it; its minimum is two operations (create an order, list my orders). Phase 8 is where things get dropped first: OSRM is optional by the description, and the LLM part of the bot waits there for a key (D58). In the end one thing was dropped there: the LLM part, for which there was no key.

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

Delivered:

- The image has a target for each service: `api`, `solver`, `bot`. The one builder stage builds all three, so what they share is compiled once (D38).
- Compose has the whole stack: `db`, `api`, `solver`, `bot`. A clone needs `docker compose up --build --scale solver=4` and nothing else. It needs no `.env`, because every setting has a value to fall back on, and the three that are secrets fall back to values for a demonstration (D54). The API is published on `127.0.0.1:8080`.
- Health checks where something waits for one: the database's, now asked over TCP, and the API's. The API waits for the database and the bot for the API. A solver waits for the database and, by itself, for the schema.
- In the API: `routeforge-api healthcheck` asks the API that runs beside it for `/readyz`, as the health check of an image that has no `curl`. A health check that went well is logged at `debug` (D55).
- The stub of the bot idles until it is told to stop, and hears SIGTERM (D56).
- `scripts/demo.ps1` and `scripts/demo.sh`: the scaling run and the failure run, through the API alone (D57).
- `README.md` says how to run the stack and the demonstration. `.env.example` says what compose passes on, and what the three demo values are good for.
- Tests: 8 more, all in the API, which has 100 now. Four hold the probe to what it asks and to what it takes for a yes, one of them against the application over a real socket. One reads the log after health checks that went well and one that did not. One reads the `api` service out of `docker-compose.yml` and starts the API's settings from what it falls back to. Two run the binary itself, with `healthcheck` and with an argument it does not know.
- The tests were tried against 18 faults put into the code and into the compose file on purpose. Each made a test fail but two: with the `healthcheck` argument not looked at, and with an unknown argument taken for none, every test passed, because none of them came in through `main`. The two tests that run the binary were added for that, and fail for both. Among the others: a probe that asks `/healthz`, takes any status, takes a `200` from what is no HTTP, or waits without end; an address that means everywhere asked as it is; a check that went well logged like any request, and one that did not go well not logged; a demo key too short to sign with, a demo password too short for an account, a variable with nothing to fall back on, and the port published to every machine.
- Tried by hand, with the development database left alone: a clone of the phase branch in a directory of its own, with no `.env`, under another project name and on other ports.
  - **The one command.** The stack was up, with the database and the API healthy, four workers alive, the admin made and the bot idling. Ten starts on a new volume took 8.5 to 9.0 seconds each and all went well. The API logged no health check. `docker compose down` took 2.8 seconds.
  - **The database's health check.** Ten starts with the check as it was, over the socket, went well too. The window is there: on a new volume the socket said yes for 0.14 seconds while the port was closed. sqlx's five seconds of trying again cover it. With the set-up of the database stretched by twelve seconds, the old check let the API start and the API gave up; with the new one the stack came up (D54).
  - **In a browser,** a headless Edge driven over the DevTools protocol at `http://localhost:8080`, with the pictures looked at. On a new database: logged in as the seeded admin, set up the demo city, generated 300 orders, pressed Plan. Four dashed routes of 87, 86, 78 and 49 stops were searched by four solvers, each named by the id of its container, and stored: 125.29 km, the figures of Phase 5. Once more with 1200 orders: routes of 333, 336, 328 and 203 stops, of which one went from 64.43 km a second in to 63.40, 63.25 and at last 62.97 km. Swagger UI listed 27 operations. The only line in the browser's console was its own note of the `404` the page gets when it asks for a depot that is not there yet.
  - **The scaling run,** 300 orders on four vans, 200 million moves a route: 15.2 seconds with one solver, 8.1 with two, 4.5 with four and 4.6 with eight, the same routes every time. A second round gave 15.1, 7.9, 4.5 and 4.4.
  - **The failure run,** four solvers and ten seconds a route: the killed solver's route was taken over 27.8 seconds after the kill, and the plan was done after 40.2, with every order on exactly one route. With a lease of ten seconds and a heartbeat of two it was 12.4 and 24.7.
  - **Both scripts,** all the way through: `demo.ps1` in PowerShell 7 and in Windows PowerShell 5.1, and `demo.sh` under dash on Ubuntu with curl and jq 1.8.1.
  - **At the end, the branch as it was committed,** in a clone made anew: built from an empty cache in a minute and a half, started, and shown by `demo.ps1` once more, with the numbers above.
  - **The request id** a script sent for a plan was in the log of the API and of each of the four solvers, and the job that was taken over was logged with `attempt=2`.

Where it differs from what this plan first said:

- Two services have a health check and not four. Nothing waits for a solver or for the bot, and which solvers are alive is what the registry says (D54).
- The scaling run has a budget of moves, and compares the routes of its runs. That makes it verification steps 3 and 4 at once: the same seed gives the same routes, with one solver or with eight.
- The scripts plan tomorrow and not the day the page opens on, and size their orders to the couriers that are there. A day of 300 orders on vans that were sized for 1200 is one route, and one route is nothing to share out.
- The database's health check, which is Phase 0's, was changed.
- By the project owner's choice: the bot is in the stack now, as a stub; the README has a quick start, which is otherwise Phase 8's; and CI is as it was.

Three things went wrong while it was tried. Two are guarded against now (D57), and the third can only be known about:

- `pwsh -File scripts/demo.ps1 scaling -Solvers 1,2,4,8` asked compose for 1248 solvers. Every one of those containers was created, in a project made for trying things, before the run was stopped and they were removed. The scripts read the counts as text now and refuse more than twenty.
- A moment later Docker could not resolve the registry's name, and `docker compose up --build` failed with every layer cached. The scripts no longer pass `--build`. It happened once more, with crates.io, in a build that had its dependencies to fetch, and the next attempt went through.
- At the very end `docker compose build` was refused three times running, with `additional privileges requested`. The shell it was started from, a Git Bash, had its directory spelled `d:\`. From PowerShell the same build went through. Compose 5 does this on Windows, nothing in the repository can stop it, and the README now says to start the stack from PowerShell. The trap is among the known ones below.

What later phases need from it:

- Phase 7: the `bot` service is there and waits for the API to be healthy. It is handed nothing yet. It will need its tokens, the address of the API on the compose network, which is `http://api:8080`, and `LOG_FORMAT` and `RUST_LOG` as the other two have them. Each of them needs a fallback, or has to be allowed to be missing: the stack starts with no `.env` (D54), and a bot without tokens idles (D8). The test that holds the compose file to that reads the `api` service alone.
- Phase 7: the bot is the first process of its container. Whatever takes the place of the stub's wait has to hear SIGTERM: check that teloxide's `ctrlc_handler` does, or keep `told_to_stop` beside it (D56).
- Phase 7: the runtime image has no CA certificates (D38), and the bot is the first service to speak TLS. reqwest's `rustls-tls` carries its own roots. If teloxide's client does not go that way, the certificates have to be added to the runtime stage.
- Phase 7: the key that guards the endpoint for the bot's tokens is a fourth secret for compose. It needs a demo fallback like the other three, or an endpoint that is off without it.
- Phase 7: in the image the bot is linked against a domain crate that was built with `db` (D38). `cargo check -p routeforge-bot` is what keeps the bot's source from using it.
- Phase 8: the scaling table for the README is in D57, and `scripts/demo.ps1 scaling -Solvers 1,2,4,8` prints it again. It wants a machine that is doing nothing else: one run on a busy one took 28.6 seconds where the others took 15.
- Phase 8: OSRM as a compose profile. A service with a profile stays out of `docker compose up` unless the profile is named, which keeps the stack free of the network by default (D8). The solver's setting for it goes onto the list of variables compose passes on.
- Phase 8: CI builds the `solver` target and nothing else of Phase 6. Starting the stack there, and running `scripts/demo.sh`, were offered and left out. They are a step each if they are wanted: `docker compose up -d --wait --scale solver=2`, and the script, for which the runners have `curl` and `jq`.
- Phase 8: a presentation without a network wants the images built beforehand. `docker compose up --build` asks the registry (D54).
- Phase 8: hardening that was left out. The demo key and the demo admin are public by design (D54), so the stack is for one machine as it stands. Nothing is said to a person who starts it with those values on a port they have opened wider themselves.

## Phase 7: Telegram bot (`routeforge-bot`)

Built, and tried against a stand-in for Telegram. The "Done when" of the phase is not shown yet: sharing a location and choosing a delivery of two parcels for tomorrow produces, after confirmation, an order visible on the map. That takes a bot of Telegram's own, and there was no token for one.

Delivered:

- In `routeforge-domain`: migration `0005_telegram.sql`, with `users.telegram_id` and an account that needs no address (D59); `User` with an optional `email` and an optional `telegram_id`; `users::find_or_create_by_telegram`; the bodies `TelegramLogin` and `OrdersChanged`; and a channel of the orders' own, which `orders::create` and `orders::cancel` notify (D63).
- In the API: `POST /api/auth/telegram`, behind a fourth extractor, `Bot`, which takes the bot's key and no token (D60); tokens that are told how long they are good for; the settings `BOT_SERVICE_KEY` and `BOT_JWT_TTL_SECS`; the orders' channel on the connection that listens, and an `orders` event on the stats stream.
- On the page: the orders of the shown day are read again when the stats stream says that one was placed or withdrawn, when that stream opens again after a break, and every thirty seconds.
- The bot, in three parts (D61): `api`, what it asks of the API and how; `chat`, the dialogue, with no Telegram in it; `telegram`, which turns updates into events and replies into messages.
  - **An order** begins with a location, a venue, or `/order 50.45 30.52`. The kind, the parcels and the day are asked for with buttons, the order is shown, and a button places it.
  - **The commands** are the six operations this plan named. `/orders` is `list_my_orders`, and its buttons are `get_order` and `cancel_order`. For operators, `/pending` is `list_pending_orders`, `/plan` is `create_plan` and `/status` is `get_plan_status`, each for a day that is picked with a button. Beside them are `/start`, `/help` and `/cancel`.
  - **It runs** on a token from @BotFather and on nothing else. Without one it idles. It waits for a Telegram that is away, ends on a token that Telegram refuses, and stops when it is told to (D62).
- In compose: the bot's token, passed on from `.env`; one demo key for the bot and the API; the address of the API; `TZ`. `.env.example`, the README and `CLAUDE.md` say what there is to set.
- The manifest of the crate no longer calls the bot an LLM agent.
- Tests: 65 more. 47 are the bot's, 11 the API's, which has 111 now, and 7 the domain crate's (122). Of the bot's, 23 try the dialogue against a fake of the API, 6 its requests against a server that answers where the API's OpenAPI document says and nowhere else, 7 the whole bot against a stand-in for Telegram, 6 its settings, and 5 run the binary (D64).
- The tests were tried against 68 faults put into the code and into the compose file on purpose: 38 in the bot, 14 in the API, 11 in the domain crate, three of those in the migration, and 5 in the compose file. Each made a test fail but one. With an address of the API taken that carries a password and no user name, every test passed, because the test of the settings tried an address with both and no other. It tries one with a password alone now, and one with a name alone, and fails for the fault. One test was found too weak before its fault was tried: the bot was told to stop while its first request was on its way, and never during the pause between two attempts. It has a second half for that, which a pause that cannot be cut short fails. Among the others: an order not taken out before the API is asked, so that two presses place two; an order asked for with the bot's key and not with the person's token; a token kept from one update to the next; a button of a draft that is gone taken for one of the draft that is there; a day that has passed still taken; an order withdrawn, and a day planned, by the first press; a group answered like a private chat; a refused token tried again and again, and a Telegram that is away ending the bot; the bot stopping without telling Telegram which updates it has had; a key taken that only begins as the key does; the body read before the key; a token that says a minute and is signed for an hour; whoever writes to the bot first made an admin; the loser of the race for a new account given nothing; an account that nobody can reach; an order that says nothing when it is placed; the day written as the session writes its dates; and compose giving the bot another key than the API.
- Tried by hand, in containers and in a browser, with the development database left alone:
  - **The stack,** in a project of its own and on a new volume: up nine seconds after `docker compose up`, with the bot saying that it idles. The idle bot had stopped 0.4 seconds after `docker compose stop bot`. With `TZ=Europe/Kyiv` it said that its day was `2026-10-12 +03:00`. The stack was down after 2.2 seconds.
  - **A token that was made up.** The bot, in its container, reached `api.telegram.org` over TLS, was told that there is no such bot, and ended with status 1 and the reason, 2.8 seconds after it was started. The image has no CA certificates: the roots are in the binary (D62).
  - **The bot's endpoint,** through the API of the stack and with the demo key: a token good for 60 seconds, of a new client's account that has no address; an order placed with it; a `403` for the plans; the account made an operator's by the admin, and the next token opening the plans; and a `401` each for no key, a wrong key and the admin's token.
  - **The map,** in a headless Edge against an API on the host. An order placed through the API with a Telegram user's token was on the map 0.37 seconds later, with nobody pressing Refresh, and its withdrawal after 0.37 seconds. An order of another day caused no read. One that was stored by an `INSERT` that announces nothing was shown 26 seconds later, by the read the page makes unasked. The console stayed empty, and the page asked no host but the API and the tile server.
  - **The images:** 140, 126 and 134 MB, the bot's binary 12 MB. The first build failed in `cargo chef cook` and the second went through: the trap of the registry, below.
  - **The tests, on Linux.** They had only ever run on Windows here, and CI runs them on Linux. In a container of the image the `Dockerfile` builds on, with the repository mounted read-only and on the network of the development database, the whole workspace passed as it does on Windows, and the bot's tests eight times more: `docker run --rm --network routeforge_default -v "<the repository>:/src:ro" -v routeforge-linux-target:/target -e CARGO_TARGET_DIR=/target -e DATABASE_URL=postgres://routeforge:routeforge@db:5432/routeforge -w /src rust:1.99.0-slim-trixie cargo test --workspace --locked`. It took 136 seconds, the compiling included.

Not verified yet, because it needs something outside the repository:

- **The "Done when".** The stand-in for Telegram shows that the bot does what it should with what Telegram sends. It cannot show that Telegram takes what the bot sends (D64). That takes a token from @BotFather, and "Needed from the project owner" says what to do with it, step by step.

Where it differs from what this plan first said:

- A Telegram user is not mapped to an account that is there. They are given one (D59).
- The state of a dialogue is in a map of the bot's own and not in teloxide's storage for dialogues, and the bot reads its commands itself. Both are in the part that knows nothing of Telegram, which is what lets the dialogue be tested step by step (D61).
- An order shows on the map as it is placed, and the bot takes coordinates that are typed. Both by the project owner's choice (D61, D63).
- The tests have a stand-in for Telegram, beside the fake of the API and the mock server this plan asked for. Without a token it was the only way to try the half of the bot that faces Telegram (D64).
- The workspace asks teloxide for neither its `ctrlc_handler` nor its `macros` feature any more (D61, D62).
- The page was changed, which is Phase 5's, and so were two functions of the domain crate that are Phase 2's.

What later phases need from it:

- Phase 8: free text goes on top of `chat`. A text that is no command arrives in `Chats::handle` as `Event::Text` and is answered with `NOT_UNDERSTOOD` today: that is where a model would be asked. What it may ask for is what the buttons do, the methods of `Chats` that take a `Sender`. A tool that changes something has to end in the same question with the same confirming button (D9), and `Press` is where a new button gets its data.
- Phase 8: geocoding. A draft has an `address` already, and `Chats::begin` takes a place. An address that is typed needs coordinates before it is one, from a service the bot would ask as it asks the API: behind a trait, so that the dialogue's tests can put a fake in its place.
- Phase 8: the README says that the bot takes no free text, and why. When free text is built, that paragraph goes.
- Phase 8: hardening that was left out. Nothing limits how often the bot's endpoint is asked, or how many accounts it makes. The bot's key is one key, with no way to change it but to restart both. An account with an address cannot be reached from Telegram. An order whose answer was lost on the way back can be placed a second time. A message the bot could not send, because Telegram said that it was sending too many, is logged and not sent again.
- Phase 8: what the page still learns only from Refresh: a plan that was asked for elsewhere, by chat for one, and for a client everything, since a client's page has no stream (D63).
- Phase 8: the bot's tests need no network and no token, so `cargo test --workspace` in CI runs them as it is. CI still builds the `solver` target alone, which compiles the bot as well (D38).

## Phase 8: OSRM, hardening, presentation

Built, but for one item. The bot's free text was not built, because there was no key for the Claude API to try it with (D58). And one step of Phase 7 is as open as it was: the bot has not been tried against Telegram itself.

What the phase was to hold was settled with the project owner on 2026-10-12, and is among the decisions at the top of this plan.

Delivered:

- **What a courier has on board is followed along the route (D65).**
  - In `routeforge-tsp`: a stop asks for a delivery or for a pickup. The sweep gives a courier room for what it carries out and room for what it carries back. `Loads` is what a route asks of its courier, and `optimize_route_within` searches as `optimize_route` does and takes up no tour that overloads the courier. A benchmark measures the whole search with loads and without.
  - In the solver: an order is a delivery or a pickup to the clustering and to the search, and a route is checked once more before it is stored.
  - On the page and in the demo scripts: the vans are sized for what they deliver, and the page says for each route how much its van has on board at the most.
- **Distances by road (D66, D67).**
  - In `routeforge-domain`: `Distances`, `DistanceProvider::measures`, and migration `0006_route_distances.sql` with `routes.distances`.
  - In the solver: `osrm.rs`, which asks OSRM's table service; the settings `OSRM_URL` and `OSRM_TIMEOUT_SECS`; and in the route job, the table waited for with the heartbeat beside it, great circles where there is no table, the search on the mean of the two directions, and the route stored the shorter way round, measured as it is driven.
  - In compose: the service `osrm` behind a profile, and `osrm-tools`. `scripts/osrm-prepare.ps1` and `.sh` fetch the roads of Kyiv and prepare them.
  - On the page: `by road` or `as the crow flies` beside the length of each route.
- **Hardening (D68 to D71).**
  - A limit on the logins for one address, and on the passwords hashed at one time: `API_LOGIN_ATTEMPTS` and `API_HASHES_AT_ONCE`, a `429` and a `503`, each with a `Retry-After`.
  - A content security policy for the page, with the hashes of its script and its style.
  - A warning in the log when the API starts on the demonstration's key, password or bot key.
  - `GET /api/depots/{id}`, and the page drawing a plan from the depot it was made for.
- **Addresses for the bot (D72).** `geocode.rs`, which asks a service that speaks as Nominatim does, behind a trait; the setting `GEOCODER_URL`, with nothing to fall back on; the dialogue that shows the places a search found and begins an order at the one that is picked; `Api::get_depot`.
- **Numbers and documents.** The example `quality` in `routeforge-tsp`, which prints the length of a route against the moves its search was given. The README, with what the moves buy, what a stage takes, the scaling table made again, distances by road, addresses in the bot and the known limits. D65 to D72 in the decision log, with D7 marked as replaced. `cargo audit` in CI, as a job of its own.
- In the solver, where a solver takes its distances from is decided in `Provider::of` and no longer in `main`, and the bot's binary has a test that starts it with a geocoding service. Both came of asking what a test would say to a fault in `main`.

What the free text was to be, for whoever builds it with a key:

- A Claude client over `reqwest` against the Messages API (there is no official Rust SDK), with a hand-written tool loop: capped iterations, all tool results of a turn returned in one message, assistant content echoed back unchanged, `refusal` and `max_tokens` stop reasons handled.
- The tools, with strict schemas, are the operations the bot has from Phase 7. A side effect is still confirmed with a button (D9). Bounded per-chat history in memory.
- Model id from env, default `claude-opus-5-5`. Running a cheaper model is a config change and the owner's call.
- Tests: the tool loop against a scripted fake model; request and response shapes with wiremock.
- It goes into `Chats::handle`, where a text that is no command arrives. Today such a text is an address, to a bot that has a geocoding service, and nothing to one that has not (D72). A model would be asked before either.

Tests: 117 more, and a doctest. 35 are in `routeforge-tsp`, which has 100 now beside its two TSPLIB tests; 28 in the solver (69); 21 in the API (132); 31 in the bot (78), six of which run the binary; and 2 in the domain crate (124). They hold:

- the sweep to the two kinds of room, and every stage of the search to tours the courier can drive, for random routes and loads;
- the search within loads to the shortest tour the courier can drive, on routes small enough to try every order, and to the very tour of the search without loads where nothing can overload the courier;
- the client of OSRM to what OSRM takes, longitude first, and to each way it can fail;
- the route job to the fallback and its label, to the mean of the two directions, to the way round that is shorter and can be driven, and to keeping its lease while it waits;
- the login to its count, its window and its turn, and both refusals to the OpenAPI document;
- the page to its policy, made from the page as it is on disk;
- the bot's client to its pace, to the name it gives and to what it says of a failure, and the dialogue to what it sends, shows and keeps;
- and each of the three services to a compose file that hands on every setting the service reads. The solver's holds the OSRM server to its profile as well, and the bot's the geocoder to having nothing to fall back on.

The tests were tried against 108 faults put into the code and into the compose file on purpose: 16 in `routeforge-tsp`, 24 for the solver, 4 in the domain crate, one of them in the migration, 31 for the API and 33 for the bot. Eight of them were in the compose file. Each made a test fail but four, and each of the four was a test that looked at too little:

- With the shape of OSRM's table not looked at, rows of the wrong lengths that add up to the right count were taken for a table. The test had only tables that the next check refused anyway.
- With a login counted after its password was checked, the login over the limit was refused all the same, one hash too late. The test now takes every turn there is to hash, and holds the refusal to coming at once.
- With the "No" of a search not asked which search it is of, the button of an old search dropped the new one. The test that was to catch it had ended the search itself, with its first press: a button that named a place the search did not have. That was a fault in the bot and not in the test. Such a press ends nothing now, and the test goes on to pick the place that is there.
- With `/cancel` leaving a search alone, the test passed, because it had an order open as well, which `/cancel` dropped. It has a test of its own.

Before the faults were run, three were plain from reading: nothing would have failed for a solver that ignores `OSRM_URL` in `main`, for a bot that ignores `GEOCODER_URL` there, or for a compose file that hands a setting on to nobody. Each has its test now, and the faults were then caught.

Among the others: a pickup that takes no room; loads that are never, or always, minded; a van that may not be full; each of the three stages making a move that overloads; a route stored without its last check; a route turned round although that overloads, and one never turned; the search on one direction alone; a fallback labelled as by road, and a route by road labelled as the crow flies; a table waited for with no heartbeat; latitude first; `null` taken for nought; a login never refused, never cleared, never taken back, counted by how it was typed, refused for ever, and one that puts the wait off by asking; a refusal without its `Retry-After`; a policy that allows inline script, any image or any host, and one made from the wrong part of the page; a depot read without a token, and the newest given for any id; the words of a person left in an error; requests with no pause between them; a search with no name for the bot; the box round the depot with its corners swapped; equal names not folded; a command that is none sent to the service; an old button picking from a new search; a place picked twice; and a geocoder that compose falls back to.

Tried by hand, with the development database left alone. The stack ran in a project of its own, on other ports, from images that were built from the phase branch:

- **The stack, as a clone has it.** Up with four solvers, the API healthy and the bot idling, 9.5 seconds after `docker compose up` on a new volume. The API's log began with the warning that it runs on the demonstration's key, password and bot key. `docker compose down` took 3 seconds, with an OSRM server among what it stopped.
- **The demonstration,** with `scripts/demo.ps1` in PowerShell 7 on a new database: a depot, four vans of 132 and 300 orders. With one, two, four and eight solvers the plan took 15.5, 8.3, 4.5 and 4.8 seconds, and a second round 15.4, 8.2, 4.5 and 4.5, with the same four routes every time: 73, 73, 81 and 73 stops, 126.13 km. In the failure run the killed solver's route was taken over 27.9 seconds after the kill, and the plan was done after 40.5, with every order on exactly one route. At the very end both runs were made once more in Windows PowerShell 5.1, on a new database and with the images of the last commit. They gave the same routes, the job that was taken over was logged with `attempt=2`, and the request id of the plan was in the log of the API and of the solvers. The machine had just built the images then, and one solver took 17.5 seconds where it had taken 15.5.
- **`scripts/demo.sh`,** under dash, on another new database, both runs: the same four vans of 132, the same 300 orders and the same routes as the PowerShell one, in 16.2 and 4.9 seconds, and the killed solver's route taken over after 29.7. The machine has no `jq`. With the project owner's leave, the static binary of jq 1.8.1 was fetched from the jq project's release for this one run, and checked against the checksums of the release.
- **The load.** Of those four vans, three left the depot with 132 of 132 on board, and the fourth had 118 at the most. Each route asked for more in all than D7 would have let its van take: 148, 155, 161 and 155. Before that, on a stack on the host, days of 100, 300, 1200 and 2000 orders each gave four routes and left no order out.
- **The roads.** `scripts/osrm-prepare.ps1` and, under dash, the `.sh` one each fetched the extract and prepared it. That took 16 seconds and left 106 MB in `osrm/`, and the preparing used half a gigabyte of memory at the most. With the profile on, the same day came to 475.92 km by road, the same routes with one solver and with four. A script outside the stack asked the server for the same legs and got 475.91 km. The routes that were found on great circles are 603.61 km by road. The numbers are in D66, with what explains them.
- **The server stopped in the middle of a plan,** while the first of four routes was searched for by one solver. It was gone in 0.9 seconds. The plan was done: one route by road, three as the crow flies, each saying which. The solver logged a warning for each of the three, with the reason.
- **The page,** in a headless Edge driven over the DevTools protocol, with the pictures looked at. On the stack with the roads: the day above, with `by road` on one route and `as the crow flies` on three, and `at most 132 of 132 on board`. Then a day that was generated and planned on the page, with four seconds a route: four dashed lines while it was searched, and four routes stored by road, 470.61 km. Twenty tiles were loaded, the console stayed empty, the browser held nothing against the page, and the only hosts it asked were the API and the tile server. Earlier in the phase, against an API on the host: an inline script that was added later, a script from another host, a handler in an attribute, `eval`, a request to another host and a `style` attribute were each refused, and a plan whose depot had been moved was drawn from where the depot was.
- **The login,** against a release build on the host: five wrong passwords were answered `401` in 37 to 137 ms each, and the sixth `429` in 2 ms, with `Retry-After: 60`. And in the stack, with `API_LOGIN_ATTEMPTS=2` in the shell that started it: two were answered `401` and the third `429`.
- **The public Nominatim,** through the bot's own client: eight requests in all, a second apart. It took the bot's name, answered over TLS from the roots in the binary, and what it answered was read. `Хрещатик 22` came back as three buildings under one name, which is why the bot now asks for ten places and shows those a person can tell apart. The dialogue round it was tried against stand-ins, as all of the bot is.
- **The numbers of the README:** `cargo run -p routeforge-tsp --release --example quality`, three minutes on the development machine, `cargo bench -p routeforge-tsp`, two minutes, and the scaling run above.
- **The tests, on Linux,** as in Phase 7: the whole workspace in a container of the image the `Dockerfile` builds on, 188 seconds with the compiling, all of it green. And once more after the fault pass, on the code as it was committed.
- **The images:** 140, 133 and 134 MB for the API, the solver and the bot. The solver's has grown by 7 MB with the client it asks OSRM with.
- **`cargo audit`,** as CI will run it: nothing but the warning that `proc-macro-error2` is unmaintained, which D10 knows of.

Not verified, because it needs something outside the repository:

- **The bot against Telegram,** as at the end of Phase 7. That takes a token, and "Needed from the project owner" says what to do with it.
- **The new job in CI.** `cargo audit` was run on the development machine. What CI makes of the workflow cannot be seen from there.

Where it differs from what this plan first said:

- The fallback on great circles is a route's and not a solver's, and every route says what it was measured along. A plan can have routes of both kinds (D66).
- `OsrmProvider` is `Osrm`, one of the two things a `Provider` can be. The other is great circles.
- Geocoding is off until the bot is given a service to ask. This plan said "address geocoding for the bot" and no more (D72).
- "Hardening" is four items, which the project owner picked from what the earlier phases had left out. The others are the known limits of the README.
- `cargo audit` is a job of its own in CI and not a step, so that an advisory that is published tomorrow fails nothing else.
- The page was changed, which is Phase 5's, and so were the demo scripts, which are Phase 6's: for the load, the roads and the depot of a plan.
- This phase was sized M and came out L. It was committed in parts, one for each of the five items above.

Found while it was tried, and guarded against since:

- **The words of a person in an error.** reqwest names the address it asked in its errors. The bot's search has the words in that address, so a search that failed would have logged them. They are taken out, and a test fails if they come back.
- **One address, three times.** The public service gave the same name for three buildings, and the bot would have asked which of three equal lines was meant. Found only against the real service: every stand-in had answered with places that differ.
- **A command that is none.** `/oder Хрещатик 22` would have been looked up as an address. A text that begins with a slash is not sent now.
- **OSRM over `POST`.** The server refused `"annotations": "distance"` in a body, where its documentation for an address has the word. A list is what it takes there. The first tag that was tried for its image does not exist. Both are among the traps.
- **A port that was taken.** Another program on the development machine listens on 5001. Docker started the OSRM server that was to be published there without the port, and said nothing. That is among the traps too.

What is left, for whoever goes on:

- The try with Telegram, and free text with a key: both as "Needed from the project owner" says.
- The known limits in the README. Each is a thing that was looked at and left.
- Lines along the streets on the map. The routes are found by road and drawn straight (D66).
- A route that is searched through tours which overload its courier. The search takes up none, which can keep it from a shorter one where the loads are tight (D65).

## Known traps

- **sqlx compile-time queries** are checked against the metadata in `.sqlx/`, not against a database: `.cargo/config.toml` sets `SQLX_OFFLINE`. After changing a query or a migration, run `scripts/sqlx-prepare` and commit the result, or the build fails with "no cached data for this query". The script needs `sqlx-cli`, and the command that installs it is at the top of the script. To have the compiler ask the live database while writing queries, set `SQLX_OFFLINE=false` in the shell.
- **An applied migration must not be edited.** sqlx keeps a checksum of each and refuses one that has changed. Add a new migration instead.
- **`localhost` on Windows with Docker Desktop.** It resolves to `::1` first, and a connection to the published port hangs there for about 20 seconds before the client falls back to IPv4. `DATABASE_URL` therefore says `127.0.0.1`.
- **Native build tools.** The development machine has no CMake or NASM. Phase 0 chose features that need neither (D12); a new dependency must not bring OpenSSL or aws-lc back in.
- **Recent major releases.** sqlx 0.9, utoipa 6, rand 0.10, jsonwebtoken 11, argon2 0.6 and criterion 0.8 are all newer than most tutorials. Read docs.rs for the pinned version.
- **2-opt assumes a symmetric matrix**, and OSRM road distances are not symmetric. `DistanceMatrix` rejects an asymmetric table (D14), so the solver symmetrizes for the search and reports the real directed length itself (D66).
- **Two route jobs finishing at once** must not each see the other as unfinished. The plan row's counter update serializes them; Phase 2 has a test for it.
- **`NOTIFY` can be missed** on a listener reconnect, and an expired lease sends none, so workers always poll as well, and so do the event streams of the API (D50). A payload has to be shorter than 8000 bytes. A change to a plan is announced by its id alone, and the progress of a search leaves its route out when it would not fit (D48, D49).
- **Browser `EventSource` cannot send an `Authorization` header.** The page reads the SSE stream with `fetch`.
- **An endless response hangs a test that reads its body to the end.** `Api::send` in the API's tests takes what an event stream opens with and drops the rest, and `Api::open` reads a stream one event at a time. Without the first, the test of who may call what would never return from the stats stream.
- **A graceful stop waits for an endless response.** actix-web drains its connections when it is told to stop and cuts no body short. The streams have to be ended first (D51).
- **A token is accepted through the second it expires in.** jsonwebtoken refuses one when `exp` is less than now, not when it is equal. A stream that ended at `exp` could be opened again for the rest of that second, so it ends a second later (D50).
- **Leaflet draws a canvas in the order things were added to it.** Dots that are drawn again cover a line that was there before them. The page draws its lines in SVG, above the canvas of the dots (D52). A tooltip that is given a string takes it as HTML, so the page gives it an element.
- **On Windows, whether a process ignores Ctrl-C is inherited.** A service started from a tool that ignores it cannot be stopped with it, whatever the service does. To try D51 the API was started by a launcher that turns Ctrl-C back on (`SetConsoleCtrlHandler(NULL, FALSE)`), in a console of its own.
- **A `tracing` subscriber for one thread loses lines in tests.** `tracing` remembers for every place that logs whether anybody listens, and with one subscriber that belongs to one thread it may ask a thread that has none. A test that reads the log sets a subscriber for the whole process and picks its own lines out by an id: see `captured_log` in the solver's tests.
- **A fresh database has no schema until an API has started against it.** A solver waits for it and applies none (D32). `docker compose up` starts an API. `docker compose up -d db solver` does not, and its solvers wait. `scripts/sqlx-prepare` applies the migrations to the compose database as well.
- **`pg_isready` over the socket says yes too early.** While it sets up a new database, the postgres image runs a server that listens on the socket alone. A service that is let go then and comes over the network finds the port closed. The compose file asks over TCP (D54).
- **sqlx waits five seconds for a database that refuses.** A refused connection is tried again until the pool's acquire timeout. That hid the trap above in every ordinary start, and it is why an API with no database takes five seconds to say so (D47).
- **The first process of a container is not ended by SIGTERM unless it listens for it.** A binary that does not is killed after the ten seconds `docker stop` allows. The solver, the API and the bot all listen (D35, D51, D62).
- **`docker compose up --build` asks the registry every time,** for the Dockerfile frontend and the base images, with every layer cached. Without a network it fails, where `docker compose up` starts the images that are there (D54).
- **On Windows a build is refused from a directory spelled with a lower-case drive letter.** `docker compose build`, and `docker compose up` when it has something to build, end with `additional privileges requested: pass "--allow=fs.read=Dockerfile"`. Compose 5 hands its builds to `buildx bake`, which takes the Dockerfile to lie outside the directory it was started in when the two are spelled `d:\` and `D:\`. `cmd` and Git Bash keep a working directory as they were given it, and the terminals of VS Code give it as `d:\...`. PowerShell hands `D:\...` on to what it starts, and is not affected. Three ways out: run it from PowerShell, `cd` to the directory spelled with a capital, or set `BUILDX_BAKE_ENTITLEMENTS_FS=0`, which turns that check off. `COMPOSE_BAKE=false` does not help. Seen with Compose 5.5.1 and buildx 0.37.2. `docker build --target api .` is not affected, and neither is starting images that are built.
- **The compose file names its project.** A second copy of the repository on the same machine is the same project, with the same containers and the same volume. To try something beside the development stack: `COMPOSE_PROJECT_NAME`, with other ports in `POSTGRES_PORT` and `API_PORT`. A script that calls `docker compose` acts on whichever project that is.
- **A list on PowerShell's command line is a list only inside PowerShell.** `pwsh -File demo.ps1 -Solvers 1,2,4,8` hands the script the one text `1,2,4,8`, which an `[int[]]` parameter reads as the number 1248. The demo script takes the counts as text (D57).
- **PowerShell's two versions read JSON differently.** Windows PowerShell hands a JSON array on as one thing where PowerShell 7 takes it apart, and PowerShell 7 writes a `double` that is a whole number as `165.0`, which serde does not take for a `u32`. `scripts/demo.ps1` passes what it reads through the pipeline once more, and casts what it sends to `[int]`.
- **Headless Edge wants an absolute `--user-data-dir`.** With a relative one it ends at once and says nothing, and nothing listens on the DevTools port.
- **utoipa leaves things out without a word.** An enumeration whose values come out of a macro as `literal`s is documented by the names of its variants; `web::Path<OrderId>` is read as a struct of parameters; a query struct that is not named in `params(...)` is not described at all. Each compiled and gave a document that was wrong (D43). Read `openapi.json` after changing a type or a handler.
- **A number that has been through JSON may differ in its last digit.** serde_json reads a float fast and not exactly, unless its `float_roundtrip` feature is on. A test that compares a position the API returned with one it computed allows for that.
- **A handler's name is taken.** `#[post("/api/auth/login")]` on `async fn login` makes `login` a type. A `let login = …` in any handler of that module then fails to compile, with a message about mismatched types.
- **sqlx 0.9 takes no SQL that was put together at run time.** `sqlx::query(&format!(…))` does not compile. Tests write their SQL out.
- **Argon2 is slow in a debug build**: a third of a second for one hash. Tests that need accounts store them directly and sign their tokens themselves, and those that log in use `Passwords::cheap()`.
- **teloxide's handler of Ctrl-C hears no SIGTERM.** `enable_ctrlc_handler` listens for Ctrl-C alone, and `docker stop` sends SIGTERM. The bot listens for both itself and stops the dispatcher through `shutdown_token()` (D62).
- **teloxide's `dispatch()` panics when Telegram does not answer its first request.** A network that is not up yet, and a token that is mistyped, are both a panic. `try_dispatch_with_listener` gives the error back instead.
- **teloxide writes the name of a method as `GetMe`.** Telegram's documentation writes `getMe`, and Telegram takes either. A stand-in that answers the second alone answers nothing, and a test that waits for the bot waits for good.
- **teloxide waits ten seconds over a `5xx` of Telegram's** before it hands the error on. In a test, a Telegram that is away is better played by an answer that is no JSON, which fails at once.
- **A button of Telegram's carries 64 bytes.** A UUID is 36 of them. What a button means is written short and read back by one type, `Press`, and a test holds every one to the 64.
- **A venue comes with a `location` and a `venue`.** Read the venue first, or the name and the address of the place are lost. teloxide does.
- **`INSERT ... ON CONFLICT DO NOTHING RETURNING` returns no row to whoever loses the race.** The row that won is not in the snapshot of the statement that lost. `DO UPDATE` with an update that changes nothing returns it (D59).
- **A column that comes through a CTE is nullable to sqlx.** `WITH placed AS (INSERT ... RETURNING ...) SELECT id FROM placed` gives an `Option`, whatever the column is in the table. `id AS "id!"` says that it is there.
- **A date cast to text follows the session's `DateStyle`.** `service_date::text` is `2026-10-12` in one session and `12.10.2026` in another. `to_char(service_date, 'YYYY-MM-DD')` is the same in both (D63).
- **A migration that is still being written wants a database of its own.** Once `scripts/sqlx-prepare` has applied it to the development database, changing it means resetting that database. Until it is final, prepare against a scratch database on the same server: `DATABASE_URL` in the shell, `cargo sqlx database create`, `cargo sqlx migrate run --source crates/routeforge-domain/migrations`, `cargo sqlx prepare --workspace -- --all-targets`.
- **cargo does not see that a migration has changed.** The migrations are read when the domain crate is compiled, and a change to a `.sql` file alone does not compile it again. A change to any source file of the crate does.
- **On Windows a connection to a port nobody listens on takes two seconds to be refused.** A test that wants something to fail at once should have something there that answers wrongly.
- **wiremock asks its mocks in the order they were mounted,** the first that matches. To have a later one count, mount it with a lower `with_priority`.
- **OSRM's image has no tag `v26.10.0`.** The release is published as `v26.10.0-debian`, and the bare version ends a pull with `manifest unknown` (D67).
- **OSRM's `annotations` is a list in a body.** In an address it is a word, `annotations=distance`. Sent as a word in the body of a `POST`, it is refused with `InvalidQuery`: "annotations must be a boolean or an array of strings" (D66).
- **OSRM and Nominatim take longitude first.** OSRM's coordinates are `[lon, lat]`, and Nominatim's `viewbox` is two corners as `lon,lat`. `LatLon` says latitude first, as people do. Each client has a test that would fail for the other order.
- **OSRM answers a table of a hundred positions and no more, unless it is started for more.** Beyond `--max-table-size` it refuses with `TooBig`. An answer for 2001 positions is 28 MB (D66).
- **The extract of Kyiv is called `Kiew`** at BBBike, where the cities have their German names: `https://download.bbbike.org/osm/bbbike/Kiew/Kiew.osm.pbf`.
- **A reqwest error names the address it asked.** `error.to_string()` ends in `for url (...)`, with the whole query. Where the query holds what a person wrote, as the bot's search for an address does, `error.without_url()` comes before the error goes anywhere (D72).
- **Nominatim gives one address several times.** `Хрещатик 22` came back as three buildings under one name, and with `limit=3` that was all. The bot asks for ten and shows the first three that differ by name (D72).
- **The page's policy names one script and one style.** `page.rs` hashes what stands between the first `<script>` and `</script>` of the page, and between the first `<style>` and `</style>`, to the byte. A second element of either kind, one with an attribute, a `style` attribute and a handler in an attribute are all refused by the browser, and the page then does nothing or looks wrong, with the reason in the browser's console alone (D69).
- **In Windows PowerShell 5.1, `2>$null` on a native command can end a script.** With `$ErrorActionPreference = 'Stop'`, what the command writes to its error stream becomes an error. `scripts/osrm-prepare.ps1` sets the preference to `Continue` round such a call and pipes `2>&1` to `Out-Null`.
- **`wsl.exe -- sh -c "..."` from PowerShell loses its quoting.** Write the commands into a file and have `sh` run the file.
- **Docker Desktop starts a container whose port is taken, and says nothing.** On the development machine another program listens on `127.0.0.1:5001`. An OSRM server that was to be published there came up, was reported as started, and had no port: `docker ps` shows `5000/tcp` with nothing in front of it, and what answers on the port is the other program, with a `404`. `OSRM_PORT`, `API_PORT` and `POSTGRES_PORT` move the ports.
- **PowerShell rounds a fraction away in `[math]::Min(1, 0.5)`.** With a whole number among its arguments it takes the function for whole numbers, and the answer is `0`. Write `1.0`. It made every great-circle length nought in a script that checked the lengths of Phase 8, and none of the repository's scripts has it: `scripts/demo.ps1` takes the least of whole numbers.

## Needed from the project owner

- `sqlx-cli` on any machine where a query or a migration is changed. The command that installs it is at the top of `scripts/sqlx-prepare`.
- `curl` and `jq` on a machine where `scripts/demo.sh` is run, and `curl` where `scripts/osrm-prepare.sh` is. The `.ps1` of each needs neither.
- For distances by road: nothing but the network, once. `scripts/osrm-prepare` fetches the roads and OSRM's image, and the README says which two lines then go into `.env`.
- For addresses in the bot: a geocoding service to name in `GEOCODER_URL`. `.env.example` names the public one, and what using it takes on.
- To close Phase 7: a Telegram bot token from @BotFather, and a few minutes with a phone. It is the one step of the phase that could not be made without it:
  1. In Telegram, ask @BotFather for a new bot with `/newbot`, and put the token it gives into `.env` as `TELEGRAM_BOT_TOKEN=...`. Add `TZ=Europe/Kyiv`, or your own zone, so that the bot's "tomorrow" is yours.
  2. `docker compose up -d --build --scale solver=4`, and `docker compose logs bot`: it says that Telegram knows the bot.
  3. Open `http://127.0.0.1:8080/` as the admin, set up the demo city if the database is new, and pick tomorrow as the day.
  4. In the chat with the bot: `/start`, then share a location in Kyiv, press Delivery, 2, Tomorrow and Place the order. The dot appears on the map as the bot says that the order is placed.
  5. To try the operator's side: `/start` says the id of your account. In Swagger UI, as the admin, `PUT /api/users/{id}/role` with `{"role": "operator"}`. Then `/pending`, `/plan` and `/status` in the chat.

  If something that Telegram answers differs from what the stand-in answered (D64), it shows in `docker compose logs bot` as a warning with Telegram's own words.

  With `GEOCODER_URL` set as well, step 4 has a second way in: write an address where the location was shared, and press the button of the place.
- For the free text of the bot, which Phase 8 left out: an Anthropic API key, made in the Claude Console at platform.claude.com, on an account with credit. A Claude subscription does not include one (D58). The item is described under Phase 8 as it was planned.
- The owner said on 2026-10-12 that the bootcamp holds the project to nothing formal: no rubric, and no word on whether the bot may come without its LLM part. If that changes, the README's paragraph on the bot is where the difference from the approved description is said.

## Verification

1. `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` with the compose `db` up, and `cargo bench -p routeforge-tsp`.
2. Fresh clone: `docker compose up --build --scale solver=4`, open `http://localhost:8080`, log in as the seeded admin, `admin@example.com` with the password `routeforge` (D54), set up the demo city, generate 300 orders, press Plan. Routes appear and improve live; `/docs` lists every endpoint. With 1200 orders in the field before the city is set up, the routes are long enough to be seen improving for seconds (D49).
3. Determinism: run a plan, discard it (D30), and run it again with the same seed and an iteration budget; the routes are identical. The scaling run of `scripts/demo` does this with every run it makes (D57).
4. Scaling: `scripts/demo scaling` plans the same day with 1 solver and with 4, discarded in between. With `-Solvers 1,2,4,8`, or `DEMO_SOLVERS='1 2 4 8'` for the sh one, completion time drops until workers outnumber routes.
5. Failure: `scripts/demo failure` kills one solver mid-solve; within the lease time another worker takes over and every order is still routed exactly once.
6. Tracing: take the `X-Request-Id` of the plan request and find it in the logs of the API and of every solver that worked on that plan. The demo scripts print the one they sent.
7. Offline: disconnect the network and repeat step 2 without `--build`, which asks the registry (D54). Routes draw without map tiles. The bot idles if it has no token, and with one it says that it cannot reach Telegram and goes on trying (D62).
8. Bot: place an order by chat and see it on the map, as "Needed from the project owner" says step by step. Without a token: `cargo test -p routeforge-bot` places the same order against a stand-in for Telegram (D64), and an order placed through the API appears on an open page without Refresh (D63).
9. Roads: `scripts/osrm-prepare`, the two lines in `.env`, and step 2 again. Every route says `by road`, and the day is several times as long as it was. `docker compose stop osrm` in the middle of a plan leaves the plan done, with the routes that came after it `as the crow flies` (D66).
10. Load: on the page, each route says the most its van has on board, and none says more than the van carries. With the demo city's vans, three of four leave the depot full (D65).
11. Numbers: `cargo run -p routeforge-tsp --release --example quality` and `cargo bench -p routeforge-tsp` print what the README's tables were made from.
