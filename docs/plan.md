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
- **The bot takes buttons and commands, not free text.** A location and a few buttons say what an order needs. Free text through the Claude API, which the description names, is an optional item of Phase 8 and is built only when there is a key to try it with (D58). Phase 8 had no key and did not build it. It is Phase 11 now.
- **A Telegram user gets an account of their own, the first time they write to the bot.** A client's, with no address and no password, which an admin can make an operator's. Tying a Telegram user to an account that is there already, by a code, was offered beside it and not taken (D59).
- **Phase 7 has two things beyond its list:** an order shows on the map as it is placed, and the bot takes coordinates that are typed where a location cannot be shared (D61, D63).
- **Phase 7 was merged before it was tried against Telegram.** There was no token for a bot while it was built. It was built and tried against a stand-in for Telegram (D64), and the try with a real bot is the one step of it that is open.
- **Phase 8 is OSRM, the real load, addresses for the bot, and four items of hardening,** beside the README, the architecture pass and `cargo audit` in CI. The four are: a limit on logins, a content security policy for the page, a warning when the API runs on the demonstration's keys, and a plan drawn from its own depot (D68 to D71). The rest of what earlier phases left out stays out, and is among the known limits in the README.
- **The bot looks addresses up only where it is given a service to ask.** The public Nominatim was the other choice. It is named in `.env.example`, with its usage policy, and is nobody's default (D72).
- **A route says what it was measured along, and the map draws it straight.** Lines along the streets were offered beside the label and not taken (D66).
- **There will be no token from Telegram, and there was no key for the Claude API.** So Phase 8 tried the bot as Phase 7 did, against stand-ins, and did not build free text (D58). The bootcamp holds the project to nothing formal beyond the approved description.
- **Three phases were added on 2026-10-13, after Phase 8 was done.** Phase 9 gives the map more to show, Phase 10 gives a client something to do on the page, and Phase 11 is the free text of the bot, which is a phase of its own now and not decided on. What each is to hold is listed under it, and no more than that: how a phase is built is planned when the phase begins.
- **Two things that were offered beside them were not taken:** starting the stack and running `scripts/demo.sh` in CI, and pictures of the page in the README. The project has no CI on GitHub, the project owner said the same day. The workflow under `.github/` is in the repository all the same, and nothing here has seen it run.
- **The vans of Phase 9 are reckoned on the page.** Each page moves its own vans along the stored routes, from the time since they set out on that page. Decided on 2026-10-13, while the phase was built. Reckoning from the clock of the database, positions sent in through an endpoint, and a simulator inside the system were offered beside it and not taken (D74).
- **Phase 9 has one thing beyond its list:** the map and the route sheet point at each other. A sheet that follows its van, with the length of every leg stored for it, was offered beside that and not taken (D75).
- **A route by road keeps the way it takes.** The solver asks the OSRM server for it, and it is stored with the route. That turns round the choice of 2026-10-12, for a label and straight lines (D73).
- **Phase 10 has four things beyond its list.** All four that were offered on 2026-10-13, while the phase was built, were taken:
  - the list of a client's orders and the map point at each other (D80);
  - an operator and an admin place an order by a click as well (D80);
  - an operator's page hears of a plan that was asked for elsewhere (D79);
  - a client is told which courier has their order and which stop of the route it is (D78). That one changes what a client may read (D41).
- **The key that makes an order safe to repeat is a header, and the bot does not ask again by itself.** Both were settled in the phase's plan. The header is `Idempotency-Key`. When no answer comes, the bot keeps the order open and says that pressing again cannot place it twice (D76).
- **Phase 12 is what the review of 2026-10-14 found, mended.** The project owner asked for the fixes it proposed, and chose, each time the option recommended to them:
  - a day whose plan is done is refused another plan until that plan is discarded, over offering a later plan only the couriers with no route and over calling it a second wave (D83);
  - the stats count the jobs of the last day and old workers are forgotten, over deleting old jobs and over leaving it as it was (D84);
  - all four extras that were offered: a script that tries the page, the bot's dialogue split by what it does, an index of the decisions, and the plumbing of the three services in one place, of which the last was not recommended (D81, D86);
  - the work as Phase 12 on a branch of its own, merged into `dev`.

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
| 9 | More to see on the map | Routes along the streets, vans driving them, a route getting shorter while it is searched for | M | done |
| 10 | The client's page | A client placing an order on the page and seeing it planned, beside an operator's page | L | done |
| 11 | Free text for the bot | An order placed by a sentence | M | not decided on; needs an Anthropic API key |
| 12 | Fixes from the review | What the review of 2026-10-14 found, mended, and the page tried by a script | L | done |

**Cut line.** Phases 0 to 6 are the core claim of the project. Phase 7 is in the approved description, so it stays, but it comes last because nothing depends on it; its minimum is two operations (create an order, list my orders). Phase 8 is where things get dropped first: OSRM is optional by the description, and the LLM part of the bot waits there for a key (D58). In the end one thing was dropped there: the LLM part, for which there was no key.

Phases 9 to 11 were added when all of that was done, and nothing depends on any of them. Each stands by itself, so they can be taken in any order, or left. Phases 9 and 10 make a demonstration show more, and both are built. Phase 11 is the one part of the approved description that is not built.

Phase 12 was added on 2026-10-14, after a review of the code (`docs/review.md`), and holds what the review found that code can mend.

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

The free text is Phase 11 now. What it was to be is listed there.

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
- **The page,** in a headless Edge driven over the DevTools protocol, with the pictures looked at. On the stack with the roads: the day above, with `by road` on one route and `as the crow flies` on three, and `at most 132 of 132 on board`. Then a day that was generated and planned on the page, with four seconds a route: four dashed lines while it was searched, and four routes stored by road, 470.61 km. Twenty tiles were loaded, the console stayed empty, the browser held nothing against the page, and the only hosts it asked were the API and the tile server. And at the very end, on a new database: an admin was offered the demo city, pressed the button and had four vans of 132, and the day that was then generated and planned on the page had the routes of the demo scripts, 126.13 km. The one line in the console was the browser's own note of the `404` for a depot that is not there yet. Earlier in the phase, against an API on the host: an inline script that was added later, a script from another host, a handler in an attribute, `eval`, a request to another host and a `style` attribute were each refused, and a plan whose depot had been moved was drawn from where the depot was.
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

- The try with Telegram, as "Needed from the project owner" says. Free text with a key is Phase 11 now.
- The known limits in the README. Each is a thing that was looked at and left. Phase 10 took up two of them: a client's page that hears of nothing, and an order that can be placed twice.
- Lines along the streets on the map. The routes are found by road and drawn straight (D66). That is in Phase 9 now.
- A route that is searched through tours which overload its courier. The search takes up none, which can keep it from a shorter one where the loads are tight (D65).

## Phase 9: more to see on the map

Built. What it was to hold was listed by the project owner on 2026-10-13, and how it was built was planned when the phase began. Two things were decided with the project owner then, and are among the decisions at the top of this plan: that the vans are reckoned on the page, and that the map and the route sheet point at each other.

Delivered:

- **Routes along the streets (D73).**
  - In `routeforge-domain`: `Polyline`, a line as text, which decodes when it is made, as `LatLon` checks its numbers. `DistanceProvider::path`, which `Haversine` answers with nothing. `Route.path`. Migration `0007_route_path.sql`, with `routes.path`, which the schema lets only a route by road have.
  - In the solver: the client asks OSRM's route service beside its table service. The route job asks for the way of a route when the search is over and the route is turned the way it is driven, with the heartbeat beside the wait, and stores a route without its way when it is given none.
  - In compose: the OSRM server is started for a way through 2002 positions.
  - On the page: a stored route is drawn along its way, and one that is by road and has none says that it is drawn from stop to stop.
- **Vans (D74).** A van for each route of a plan that is done, driving what the route is drawn with. They set out when the page sees the plan done, all at one speed, at which the longest route takes a minute. A button stops them and sends them out again. The page says beside them that they are simulated.
- **A chart of each search (D75).** In the row of a route, its length as the search announced it: a staircase across the plan's budget, from the shortest length so far to 2 % above it.
- **A route sheet, and the map and the sheet pointing at each other (D75).** A stored route opens as the list of its stops in calling order, with the parcels that come off or on at each and what is on board after it. A click on the route's line opens it as its button does, and a row of it shows its stop on the map.
- **The list of routes keeps its rows.** A row is replaced only when what it says has changed.
- The README says what there is to see on the page, and calls the vans simulated. D73 to D75 are in the decision log, and D52, D66 and D67 say where they were added to or turned round.

The API has no new endpoint and no new setting. `GET /api/plans/{id}/routes` gives a route its `path`, and that is the whole of what changed in `openapi.json`. Nothing was added to `Cargo.toml`.

Tests: 24 more, and one case more in a test that was there. 11 are in the domain crate, which has 135 now, 12 in the solver (81) and one in the API (133). They hold:

- the line as text to the published example of its format, to reading back what it wrote, for random lines, and to refusing whatever is no line of two points or more, in JSON as well;
- a route to being stored and read with its own way, beside routes without one and one without stops, and the schema to keeping a way off a route on great circles;
- the client of OSRM to what the route service takes, longitude first, to each way it can fail, and to the first route of an answer;
- the route job to asking for the way of the route as it is driven, to storing a route by road without a way when it is given none, to asking for none on great circles, and to keeping its lease while it waits;
- the compose file to starting the server for a way one position longer than its longest table;
- and the API to giving a route its way.

The tests were tried against 44 faults put into the code and into the compose file on purpose: 19 in the domain crate, one of them in the migration, 21 in the solver, 2 in the compose file, and one each for the API and for the page. Each made a test fail but one. With a line that breaks off in the middle of a latitude taken for the line before it, every test passed. The test cut a line off inside a longitude alone, which the reading of the next number refuses whatever the reading of this one says. It cuts one off inside a latitude now, after two points that are a line by themselves, and fails for the fault.

Among the others: five decimal places for six, and the longitude first, in the reading and in the writing; a sign lost; a line of one point taken; a letter from outside the alphabet taken; a number of any length; a line taken unread; a way not stored, not read, read from the second stop, and none for a route without stops; a way allowed on a route by great circles; latitude first to OSRM; the outline for the whole way; the service's own choice of going straight on; one position asked about; the last route of an answer for the first; a refusal taken for an answer; a way asked for on great circles, and for a route without stops; a job that fails for want of a way; no heartbeat beside the wait; a way that does not start, or does not end, at the depot; the limit missing from the compose file; the way left out of what the API sends; and a namespace named in the page.

One of them was caught by one test alone, which was written for it: the way asked for in the order the search found, and not in the order the route is driven. To the search a ring that is driven one way is the same ring driven the other, so it finds the same tour on both, and on one of the two that tour is against the traffic and is turned round. The job is run on both.

Tried by hand, with the development database left alone. First with the API and the solvers on the host, against a database made for it. Then in the stack: a clone of the phase branch in a directory of its own, with no `.env`, under another project name and on other ports, from images that were built there. The page was looked at in a headless Edge driven over the DevTools protocol, and the pictures were looked at.

- **The server asked first.** Before the client was written, the OSRM server of the stack was asked what its route service takes in a body and what it answers. That is where `continue_straight`, the limit of 500 positions and the kinds of value each option takes come from (D73).
- **The stack, as a clone has it.** Up 11 seconds after `docker compose up` on a new volume, with four solvers. The images are 140, 133 and 134 MB, as they were.
- **A day on the page.** An admin was offered the demo city and pressed the button, generated 300 orders and pressed Plan, with six seconds a route. Four dashed routes were searched for by four solvers, each named by the id of its container, and stored: 33.16, 33.65, 37.52 and 19.75 km, 124.08 km as the crow flies. As the plan was done a van set out along each. The one line in the console was the browser's own note of the `404` for a depot that is not there yet.
- **The charts.** On a day of 1200 orders, on vans of 528, the routes had 331, 344, 330 and 195 stops. Their charts began at 77.55, 84.32, 76.63 and 47.98 km and went on falling for one to six of the ten seconds, to 62.84, 66.16, 64.37 and 39.41 km. On the day of 300 orders each chart is one step, or a level line where the page heard of the search after its first tour (D75).
- **The sheet.** A route of 86 stops left the depot with 131 on board and came back with 37. The most in the column was 131, which is what its row says, and every row added up. It was opened by its button and by a click on its line, and a row brought the map to its stop.
- **The vans.** On the map 35 ms after the page showed the plan as done. Stopped and sent out again by the button, back after a minute, gone with a discard, and not setting out by themselves in a browser that was asked for less motion (D74).
- **A plan with fewer workers than routes,** two for four. The row of a stored route stayed the element it was while the other routes were searched for, and its button opened its sheet while the plan was still at it.
- **The roads.** With the profile on, the day of 300 orders came to 505.55 km by road, with 200 million moves a route, in 4.8 seconds. Each route had its way: 2067 to 4848 points, within 0.21 % of the route's length from point to point. The stored routes followed the streets and crossed the Dnipro on its bridges, and at three moments of a drive each van was within 1.1 pixels of its route's line. By the server's own log the four ways took 5 to 9 ms, beside 19 to 29 ms for the four tables.
- **Routes of more than 500 stops.** A day of 2000 orders on four vans had routes of 347, 548, 549 and 556 stops. Each got its way, which the server refuses unless it is started for it.
- **A server that refuses the way,** started for one through ten positions. The plan was done, with four routes by road and no way, a warning in the log of each solver, and `by road, drawn from stop to stop` on the page.
- **The server stopped three seconds into a plan,** with one solver and six seconds a route. The plan was done after 40.2 seconds: one route by road without its way, and three as the crow flies, each saying which.
- **The demonstration,** with `scripts/demo.ps1` on that stack, since the route job was changed: 15.4 seconds with one solver and 4.7 with four, the same routes. The killed solver's route was taken over 27.9 seconds after the kill, and the plan was done after 40.6, with every order on exactly one route. Those are the numbers of Phase 8, to a few tenths of a second.
- **In every run** the browser held nothing against the page, and the only hosts it asked were the API and the tile server.
- **The tests, on Linux,** as in Phases 7 and 8: the whole workspace in a container of the image the `Dockerfile` builds on, 116 seconds with the compiling, all of it green.

Where it differs from what was planned:

- The chart shows 2 % above the shortest length, and not 5 %. With 5 % the fall that goes on took the lowest third of the chart or less.
- The chart, the sheet and the vans were committed together. They are three parts of one file, and all three hang on the list of routes that keeps its rows.
- The map is moved at once by the page, and no longer by a zoom that is seen to happen. That is Phase 5's, and was changed for what is said below.

Found while it was tried, and guarded against since:

- **A way that is not the route.** Asked as its documentation leaves it, OSRM makes a way go straight on at every stop, where its table measures every leg by itself. Nearly half the legs of a long route came out longer. Found by asking the server before a line of the client was written.
- **A move of the map that was passed over.** While Leaflet shows a zoom, it passes over the next move it is asked for. A day that was picked just after the login kept the view of the day before, and a row of the sheet pressed just after another would not have moved the map. Found because a script picks a day faster than a person.
- **A row that named one length twice.** In the stack the first length the page heard of a search was sometimes its last, and the row said `from 33.16 km` beside 33.16 km.
- **A fault that no test caught,** which is above.

Seen, and left: on a day of 1200 orders the dashed lines of the searches can hardly be told from the dots of their own colour. They are there, as the page's own elements say. That is as it was in Phase 5.

What later phases need from it:

- Phase 10: none of this is on a client's page. A client sees no plan, so no route, no van and no sheet (D41). If a client is to see the van that has their order, that is a question of what a client may read, and not of the page.
- Phase 10: a list on the page that gets buttons has to keep its rows, as the list of routes does now (D75). `renderRoutes` shows how: what a row says is made into one text, and the row is replaced when the text has changed.
- Phase 10: the page moves the map with `{ animate: false }`. A click on the map that places an order should not start a zoom that the next move is lost in.
- Phase 10: the next migration is number 0008.
- Phases 10 and 11: the OSRM server of the stack logs the body of every request, which is the position of every stop. It is the stack's own server, and nothing of a person but where their order is.

What is left, for whoever goes on:

- A sheet that follows its van, and kilometres in the sheet. Both need the length of every leg of a route, which the solver has when it stores the route and does not keep (D73, D74).
- Vans that are at the same place on every page, and on their way when a page is opened later. That takes the clock of the database, as a span (D74).
- A chart for a page that did not see the search. Nothing stores what a search announces (D49).
- A way for a route that was stored while the server was away. It is asked for once.

## Phase 10: the client's page

Built. What it was to hold was listed by the project owner on 2026-10-13, and how it was built was planned when the phase began. The request named Phase 9, which had been built and merged the same day. Asked which phase was meant, the project owner said this one. Four things beyond the list were offered there and all four were taken, and they are among the decisions at the top of this plan. With them the phase was sized M and came out L.

What it was to hold, as it was listed:

- **An account opened on the page,** and an order placed there by a click on the map, and withdrawn there.
- **A client's page that hears of its orders:** one that was placed elsewhere, and one that a plan has taken, with nobody pressing Refresh.
- **A key that makes placing an order safe to repeat,** for the bot and for the page.

Done when, in two browser windows: somebody opens an account on the page and places an order by a click on the map. The operator's page shows it at once. The operator plans the day, and the client's page shows the order as planned, with nobody pressing Refresh. And an order that is sent twice with the same key is stored once, so that the bot can say for certain whether an order was placed.

All of that was shown, on the host and in the stack as a clone has it.

Delivered:

- **A key that makes placing an order safe to repeat (D76).**
  - In `routeforge-domain`: `IdempotencyKey`. Migration `0008_order_key.sql`, with `orders.idempotency_key`, which is unique for a client. `orders::place`, which is the statement of `create` with a conflict that changes nothing and returns the row that has the key. A repeat tells nobody. `NewOrder::asks_for` says whether an order is the one a request asks for.
  - In the API: `POST /api/orders` takes the header `Idempotency-Key`. It answers `201` for a new order, `200` with the stored one for a repeat, and `422` for what is no key and for a key that has placed another order.
  - In the bot: a draft has a key, which goes out with its order. When no answer comes, the draft stays, and the next press asks for the same order with the same key. A draft that is changed gets another. `FakeApi` places an order once for each key, as the API does, and can place one and lose the answer.
- **Where a planned order is on its route (D78).** `OrderStop`, `routes::stops_of_day`, and `GET /api/orders/stops?service_date=`: the courier's name, which call of the route, and of how many. A client gets it for their own orders.
- **A stream of one's own orders (D77).** `GET /api/orders/events?service_date=`, for anybody with a token, and `Pace::orders_every`. It says when something has become of an order of the caller's, and nothing of anybody else's.
- **A plan that was asked for elsewhere (D79).** The stats stream names every plan that changes, in a `plans` event, a `PlanChanged`. `Heard::ChangedWith` is what a stream makes of a signal that is both: an event to send on, and a reason to read.
- **The page (D80).**
  - The login opens an account.
  - A click on the map begins an order for the day that is shown, for whoever is logged in. It is placed with a key, and stays in the form when no answer comes.
  - A client has a list of their orders: pending, planned with the courier and the stop, or withdrawn, and a button that withdraws. A row shows its order on the map, and a click on an order marks its row.
  - A client's page opens the stream of its orders for the day it shows. An operator's shows a plan that was asked for elsewhere.
  - Stops, the depot, the lines of routes and the vans keep their clicks to themselves.
- The README says what a client has on the page. Two of its limits are gone, and four that are new are there. D76 to D80 are in the decision log, and D39, D41, D50, D52, D61 and D63 say what changed for them. `CLAUDE.md` has the rules that are easy to break here.

The API has two endpoints more, 31 in all, and no new setting. In `openapi.json` there are the two operations, the header and the `200` of `create_order`, the third event of the stats stream in its words, and the schemas `OrderStop` and `PlanChanged`. Nothing was added to `Cargo.toml`, and the compose file and `.env.example` are as they were. The solver was not touched.

Tests: 25 more. 11 are in the domain crate, which has 146 now, 12 in the API (145) and 2 in the bot (80). They hold:

- a key to its letters and its length, and a key that is made to being one that no other is like;
- an order with a key to being placed once, whoever asks again and however often, also when it was withdrawn or planned since, and four requests at once to being one order;
- a request that waits for one that is then rolled back to placing the order itself;
- a key to being its client's own, a repeat to announcing nothing, and the schema to refusing an empty key and one of 65 letters;
- the API to `201`, then `200` with the same body, to `422` for each of five ways an order can differ and for each of five texts that are no key, and to asking for the token first;
- where an order is to the whole route, counted from 0, for one client and for all, and to nothing before the route is stored and after its plan is discarded;
- the stream of one's orders to one event for each thing that becomes of an order, with silence between, and to silence for whatever somebody else places, withdraws or has planned;
- that stream to reading without a reason and when it may have missed something, to opening with its day, and to ending with its token and with the API;
- the stats stream to naming each plan that changes, by its id and nothing else;
- the bot to the same key on the second press, whether the first request was lost on the way there or on the way back, to another key for an order that was changed, and to the header the API's document has for it.

The tests were tried against 60 faults put into the code on purpose: 21 in the domain crate, three of them in the migration, 29 in the API and 10 in the bot. One of them was no fault. Each of the other 59 made a test fail, one of them only after its test was changed:

- **No fault:** one key for every draft, in the place where a draft is begun. A draft is given another key with every answer it gets, and it cannot go out before it has had three. So the key it begins with is never sent. Put in where the key of an order that goes out is made, the same fault failed its test.
- **A test that waited without end.** With a stream of one's orders that opened with nothing, one test did not fail: it hung. It reads what a stream opens with, and there was nothing to read. It waits thirty seconds now, and fails.
- **One fault needed a database of its own:** a key unique among all clients together. The index is in the migration and the statement names it, and a statement is checked against a database when it is compiled. So the tests of that fault were built against a database with the faulted schema.

Before the faults were run, two were plain from reading, and each got its test first: nothing would have failed for a stream that looks where the orders of everybody are, since no test put somebody else's order on a route while a stream was watched; and nothing for an order that was changed by a button of an earlier question after it had gone out, and was sent with the key it had before.

That second test showed something else. The bot gave a draft another key in two places, and the second, where the day of a draft has passed, came to nothing: such a draft cannot go out before it has a day again, and gets its key with that. It is gone.

Among the others: an empty text, a space and 65 letters taken for a key; every key that is made the same; a key not stored; a repeat announced, reported as placed now, and answered `201`; an order with a key reported as there before; the schema taking an empty key, and one of any length; an order of another demand, address, place or client taken for the one that was asked for; a key that has placed another order answered with that order; a key that is none passed over; a key read and not used; an order placed without being looked at; the calls of a route counted among one client's rows, and from one; the stops of everybody, and of every day, given for one; a client told where everybody's orders are, and an operator only of their own; each of the two new paths registered after the one that takes an id; the word of an order passed on unread, whoever placed it; a stream deaf to an order, to a plan, and to having missed something; one that reads the orders of everybody, or of every day; one that does not look where the orders are, or how they stand; one that speaks at every read, or never reads unasked; the stats stream not naming the plan, naming another, naming it under another event, and not reading the queue again; the bot sending no key, a key of its own making, one under another header, a new one for every press, and one for every order; an order that got no answer said not to be placed, dropped, and given another key for the next press.

Tried by hand, with the development database left alone. First with the API and a solver of four workers on the host, against a database made for it. Then in the stack: a clone of the phase branch in a directory of its own, with no `.env`, under another project name and on other ports, from images that were built there. The page was looked at in a headless Edge driven over the DevTools protocol, each page in a browser of its own, and the pictures were looked at.

- **The stack, as a clone has it.** Built and up in 44 seconds, with the layers of the dependencies there already, and then, on a new volume, up in 9.8 seconds, with four solvers, the API healthy and the bot idling. The API's log began with the warning that it runs on the demonstration's values. The images are 140, 133 and 134 MB, as they were. `docker compose down` took 3.1 seconds.
- **The "Done when", in two windows.** Somebody opened an account on the page, after a password of five letters was refused with the reason. A click on the map opened the form, and nothing was asked of the API until the button was pressed. The order was in their list as pending, and on the admin's page 0.34 seconds after the press, with nobody touching that page. The admin generated 40 orders and pressed Plan, with two seconds a route. The client's row said in turn that the order waits for its day to be planned, that a plan has taken it and its route is still searched for, and `Van 3 has it: stop 1 of 11`: that last 2.7 seconds after Plan was pressed. The client's page had asked nothing about plans.
- **Taken back.** The admin discarded the plan, and the client's order was pending again. The client withdrew it, and the admin's page showed the withdrawal.
- **Somebody else.** A third account placed an order for the same day. The admin's page showed it. The client's page showed nothing new, and in the two seconds after it asked the API nothing.
- **An answer that was lost on its way back.** The answer to the client's order was failed in the browser, after the API had sent it. The page said that the order may or may not be placed, and to press again, and kept it in the form. The admin's page showed that it was placed. The second press was answered `200`, and the day had the order once.
- **A key is of the order as it is asked for.** After a lost answer the parcels were changed from one to three before the button was pressed again. That went out with another key, and both orders were there: the one that got no answer, and the changed one. Pressed again unchanged, an order went out with the key it had, and was there once.
- **The same by hand,** against the stack, with `curl`: `201`, then `200` with the same id, `422` for another order under that key and for a key of two words, and one order.
- **The list and the map.** A click on an order marked its row and put a ring round it, and began no order under it. A row brought the map to its order.
- **An operator.** The admin placed an order by a click. A click on an order began none. A click on the line of a route opened its sheet and began none, and a click on a van began none.
- **A plan from elsewhere.** A script asked for a plan of the day the admin's page showed. The page showed it 0.37 seconds later, with its routes being searched for, and the client's page said where its order was on it. For a plan of another day the page read the plans of its own day once, and not once for each word of that plan, and showed what it had shown.
- **The corners.** A day that had passed took no order, and the page said why. The stream that was open was of the day that was shown, after each change of the day. An order that was open went with the day, and was dropped when the day was one that had passed. After a reload the list and the stream were there again. Somebody else who logged in at the same browser saw nothing of the one before: no order, no ring, and an empty form.
- **A login that ends,** with logins good for twenty seconds, on the host: the stream of a client ended after 21.0 seconds, for its token, and the page was at the login 22.1 seconds after it was logged in to, saying that the login had ended.
- **The API stopped with a client's stream open,** in the stack: it had stopped 1.3 seconds after `docker compose stop api`, and the stream had ended because the API was stopping.
- **The demonstration,** with `scripts/demo.ps1 scaling` on a new volume of that stack, since the statement that stores an order was changed: 15.2 seconds with one solver and 4.6 with four, the same four routes of 300 stops, 126.13 km. Those are the numbers of Phases 8 and 9.
- **In every run** the browser held nothing against the page but its own notes of what was brought about on purpose: the password that was refused, the answer that was lost, and the depot that was asked for before there was one. The only hosts the pages asked were the API and the tile server.
- **The tests, on Linux,** as in Phases 7 to 9: the whole workspace in a container of the image the `Dockerfile` builds on, 268 seconds with the compiling, all of it green.

Not tried, because nothing of it was changed: the failure run of the demonstration, `scripts/demo.sh`, and the stack with the roads. The bot was tried as before, against stand-ins (D64).

Where it differs from what was planned:

- A draft of the bot's is given another key in one place, and not in two. The fault pass found the second to come to nothing.
- An operator's page reads its orders again when it shows a plan that it heard of. Such a plan may have taken its orders already, and the page counted them as pending until the plan was done.
- The images were built from Git Bash, with `BUILDX_BAKE_ENTITLEMENTS_FS=0`, and not from PowerShell. Both get round the same trap.
- The page says `1 order` where it said `1 orders`. A client often has one.

Found while it was tried, and guarded against since:

- **A box round a route.** A click on the line of a route left a black box round the whole route. Leaflet listens for the focus of whatever has a tooltip, the browser takes a line that is listened to like that for one a click gives the focus to, and it draws the outline of the focus as a box. That was so since Phase 9, which made the lines something to click, and was not seen there. The page takes the outline off (D80).
- **A page that showed an order five seconds late,** where the other showed it in a third of a second. It was the driver: with two tabs in one browser, the timers of the tab behind are held back. Each page has a browser of its own now, and the trap is among the known ones.
- **A plan from elsewhere with its orders counted as pending,** which is above.

Seen, and left:

- When an answer is lost, the client's own stream may put the order into their list while the form still says that it may or may not be placed. Pressing again says the same, and closes the form (D80).
- A stream that its page has left, for the stream of another day, stays with the API until it next writes and finds nobody there. Three of one page's were still open when the API was stopped, and ended with it (D77).

What later phases need from it:

- Phase 11: an order that a model asks for has to go through the draft, as a button's does. `Chats::place` sends the draft's key. A tool that built an `OrderRequest` and called `create_order` by itself would need a key that it keeps from one turn of the model to the next, or a second answer of the model's places a second order (D76).
- Phase 11: `FakeApi::lose_next_answer` places an order and loses its answer. `fail_next` with `Failure::Away` is the request that never arrived.
- The next migration is number 0009.
- A new path of one word under `/api/orders` has to be registered before `/api/orders/{id}`.

What is left, for whoever goes on:

- An operator cannot withdraw an order on the page, and nobody can type a place there.
- A client sees no route, no van and no sheet, and is told no time: a route has none (D78).
- The page hears nothing of the depot or the couriers. A page that was open when an admin moved the one or added the other shows it after Refresh.
- Nothing limits how many accounts are opened or how many streams one person holds. Both are among the known limits in the README.
- The try with Telegram, as "Needed from the project owner" says, and Phase 11.

## Phase 11: free text for the bot

Not decided on. It is the one part of the approved description that is not built: the description calls the bot LLM-powered and names the Claude API (D58). The project owner made it a phase of its own on 2026-10-13, and had not decided then whether it will be built.

It is not begun without an Anthropic API key. Code for the Claude API that was never run against the Claude API is not shipped (D58).

What it is to hold, as it was planned for Phase 8:

- A Claude client over `reqwest` against the Messages API (there is no official Rust SDK), with a hand-written tool loop: capped iterations, all tool results of a turn returned in one message, assistant content echoed back unchanged, `refusal` and `max_tokens` stop reasons handled.
- The tools, with strict schemas, are the operations the bot has from Phase 7. A side effect is still confirmed with a button (D9). Bounded per-chat history in memory.
- Model id from env, default `claude-opus-5-5`. Running a cheaper model is a config change and the owner's call.
- Tests: the tool loop against a scripted fake model; request and response shapes with wiremock.
- It goes into `Chats::handle`, where a text that is no command arrives. Today such a text is an address, to a bot that has a geocoding service, and nothing to one that has not (D72). A model would be asked before either.

Done when: a sentence such as "deliver two parcels to Khreshchatyk 22 tomorrow" brings the question the buttons bring, with the same confirming button under it. The order is placed by the press of that button and by nothing before it (D9). And this was tried against the Claude API itself, and not against a stand-in for it alone.

What is known so far:

- **To be shown in a chat it needs a token from Telegram as well,** and the project owner said on 2026-10-12 that there will be none. Without one, the half that faces Telegram is tried against the stand-in, as all of the bot is (D64), and the half that faces the model against the Claude API.
- **The README says today that the bot understands no sentences, and why.** When this is built, that paragraph goes.

## Phase 12: fixes from the review

Built. Added on 2026-10-14, after a review of the code at the end of Phase 10 (`docs/review.md`). The project owner asked for the fixes it proposed, and chose four things it left open: a day whose plan is done is refused another plan until that plan is discarded; the stats count the jobs of the last day; all four of the extras it offered; and the work as a phase of its own, into `dev`.

What it was to hold, as the review ranked it:

- **R1:** a job that panics outside its search took its worker down for good.
- **S1:** the login limit could be reset by a flood of logins for made-up addresses.
- **Q1:** the page had no test that could be run again.
- **P1, R2:** the live view counted every job there ever was, every two seconds a page, and nothing was ever deleted.
- **A1:** a second plan of a day gave every courier a second round.
- **S2, S3, T1, P2 to P4, Q5:** the bot's key minted an admin's token, one person could hold any number of streams, a test waited 200 ms for an answer, three reads did more than they had to, and a mistyped `RUST_LOG` said nothing.
- **Extras:** a decision index, the services' plumbing in one place, and the bot's dialogue in files by what each does.

Done when: every finding with a fix in code has it, with a test that fails without it; the page is tried by a script; and the checks of `CLAUDE.md` pass.

Not mended here, because no code mends them: the bot is not LLM-powered (C1, Phase 11), it has not met Telegram (C2), and the compiler stays where teloxide's `proc-macro-error2` lets it (Q4, D10).

Delivered:

- **A job that panics is a failed attempt (D82).** `worker::guarded` polls a job through `catch_unwind`, and the panic's words go to `queue::fail`. A worker that is lost all the same ends its solver with the reason and an error status, after the others have given their jobs back.
- **One plan that stands for a day (D83).** `plans::create` locks the plans of the day and refuses one while a plan of it is done: `DbError::DayIsPlanned`, a `409`. The page keeps Plan off for such a day and says why.
- **The jobs of the last day (D84).** `queue::depth` takes a span, and the API asks for a day. A solver that starts forgets the workers not seen for a day. Migration `0009_indexes.sql`: an index on when a job ended, and one on an order's client and day in the place of the one on its client.
- **Streams by the person (D85).** `stream::Streams` and its `Slot`, `API_STREAMS_PER_USER` (16), and a `429` with `Retry-After` on the three streams.
- **The login limit (D68).** The count that makes room is one with the fewest tries, the newest first, and never one at its limit.
- **The bot's token (D60).** `Tokens::issue_to_the_bot` gives an operator's rights at the most.
- **The reads.** `orders::of_client_on` for the stream of one's own orders, `routes::stops_of_day` counting a route once, and `orders::create_many` for a day of demo orders.
- **The services' plumbing (D81).** `routeforge_domain::service`, behind the feature `service`: `init_logging`, which says so of a `RUST_LOG` it cannot read, `told_to_stop` and `Backoff`.
- **The bot's dialogue** in five files (D61), and its two `causes` one.
- **The probe's tests** give a stand-in that answers the two seconds the probe has in the image.
- **`scripts/page-check.mjs` (D86):** the page in Edge or Chrome without a window, through what it is for, with nothing held against it.
- **The decision log** has an index of its decisions, D1 to D86, and the older decisions say what Phase 12 changed for them.

The API has no new endpoint and one new setting, `API_STREAMS_PER_USER`, which compose hands on and `.env.example` explains. In `openapi.json`: the `429` of the three streams, the `409` of `POST /api/plans` in its words, what the counts of the stats count, and the words of `POST /api/auth/telegram`. The solver depends on `futures-util`, which was in the build already. The bot no longer depends on `tracing-subscriber`, and the API and the solver have it for their tests alone.

Tests: 20 more, 577 in all. 8 are in the domain crate, which has 154 now, 8 in the API (151 and the 2 of its binary), and 4 in the solver (85). They hold:

- what `RUST_LOG` gives when it is set, empty, missing and mistyped, and a pause that doubles to its limit and starts again;
- a day whose plan is done refused another, by `create` and by `submit`, with nothing left behind, and planned once the plan is discarded; a day whose plan was cancelled planned again; and a plan asked for while the last route of the day's plan is stored waiting for that plan, and refused;
- the depth of the queue counting the jobs that ended within the span and those queued however long ago, and through the API, a job that ended counted and a day later not;
- a worker unseen for a day forgotten, by the registry and by a solver that starts;
- one client's orders of a day, and many orders stored together, each day told once, and none stored when one of them cannot be;
- a route job whose distances panic failing its attempts with the panic's words while its worker goes on, and a worker that panics or stops by itself ending the others and the process with the reason;
- an address at its limit, and one a try short of it, keeping their counts through a flood of made-up addresses, and a new address waiting when every count is at its limit;
- the bot given a client's, an operator's and, for an admin, an operator's token, and through the API not let into what is an admin's;
- a person's streams counted, refused one over the limit with the reason and when to come back, and given back when a stream ends or is refused;
- the `409` of a planned day through the API, with its words.

The tests were tried against 48 faults put into the code on purpose: 17 in the domain crate, 11 in the solver, 18 in the API, the compose file among them, and 2 in the bot. One did not compile, and was put in again in another form. 44 made a test fail. Three were missed:

- **The line in the solver's `main`** that ends the process with the error a lost worker gives. No test runs the solver's binary, and a worker cannot be made to be lost from outside. It is one `?` (D82).
- **Starting the pause again after a success,** in the solver's listener of the jobs channel and in the bot's way to Telegram. Without it a second outage waits longer before the next try, and polling, or the next try, covers it meanwhile. `Backoff::reset` itself is tested. Neither of the two places was tested before this phase either.

Two faults were plain from reading before the pass, and each got its test first: the count that makes room among the logins taken as the oldest of those below the limit, which no test would have seen, and the stats counted of any span, since no test of the stats had a job that ended. Both were caught in the pass.

A fault in the page, Plan left pressable for a day whose plan is done, made `scripts/page-check.mjs` fail where it should, with a picture of the page.

Tried, with the development database left alone:

- **The checks of `CLAUDE.md`,** offline and `--locked`: format, clippy with warnings as errors, the bot without `db`, and the 577 tests, in 108 seconds with the compiling. `cargo audit` found what it found before: one allowed warning, `proc-macro-error2`, unmaintained. `.sqlx/` was prepared against a database with migration 0009, and `cargo sqlx prepare --check` agrees with it.
- **The page, on the host:** an API and a solver of four workers from the debug build, on a database of their own. `scripts/page-check.mjs` passed in 6.4 seconds on a new database and in 6.0 on the same day again. With Plan left pressable for a planned day it failed there, with a picture of the page.
- **The stack, as a clone has it:** a clone of the phase branch with no `.env`, under the project `routeforge-try12` and on other ports. Built in 114 seconds with the layers of the dependencies there already, and up on a new volume in 10 seconds, with four solvers, the API healthy and the bot idling. The images are 140, 133 and 134 MB, as they were. `scripts/page-check.mjs` passed against it in 4.6 seconds. Its logs held the warning about the demonstration's values and the depot a new database does not have, and nothing else. Taken down with its volume and its images.

Not tried, because nothing of it was changed: the demonstration's scripts, the stack with the roads, and the bot against Telegram.

What is left, for whoever goes on:

- Phase 11, and the try with Telegram, as before.
- The decision in the solver's `main`, to end with the error a lost worker gives, is a line no test runs (D82).
- A day planned and then given a late order is planned again by discarding the plan: every order of the day is searched for again (D83).
- `scripts/page-check.mjs` is run by hand.

What later phases need from it:

- The next migration is number 0010.
- A new way to make a plan goes through `plans::create`, which holds the rule of D83.
- A new event stream goes through `stream::open`, which takes its place among the user's (D85).
- After a change to the page, `node scripts/page-check.mjs` against a stack that was built with it.

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
- **An endless response hangs a test that reads its body to the end.** `Api::send` in the API's tests takes what an event stream opens with and drops the rest, and `Api::open` reads a stream one event at a time. Without the first, the test of who may call what would never return from the stats stream. A stream that opens with nothing hangs `Api::send` all the same: a test that reads one that way wraps it in `soon`.
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
- **OSRM gives a way through five hundred positions and no more, unless it is started for more.** That is another limit than the table's, `--max-viaroute-size`, and raising the one does not raise the other. A server that is started for long tables alone gives a long route its distances and refuses it its way (D73).
- **For cars, OSRM makes a way go on from a stop in the direction it came to it.** Its table does not: there every leg is measured by itself. So the way it gives through the stops of a route is not the way the route was measured along, unless it is asked with `continue_straight` off. Left on, a way through 330 stops was 1.6 % longer than the table's legs, and nearly one leg in two differed (D73).
- **In a body, OSRM takes each option as one kind of value.** `steps`, `continue_straight` and `skip_waypoints` are booleans, `overview` and `geometries` are words, `annotations` is a boolean or a list. The other kind is refused with `InvalidQuery`. An option it does not know is passed over without a word, so a misspelt one changes nothing and says nothing.
- **The extract of Kyiv is called `Kiew`** at BBBike, where the cities have their German names: `https://download.bbbike.org/osm/bbbike/Kiew/Kiew.osm.pbf`.
- **A reqwest error names the address it asked.** `error.to_string()` ends in `for url (...)`, with the whole query. Where the query holds what a person wrote, as the bot's search for an address does, `error.without_url()` comes before the error goes anywhere (D72).
- **Nominatim gives one address several times.** `Хрещатик 22` came back as three buildings under one name, and with `limit=3` that was all. The bot asks for ten and shows the first three that differ by name (D72).
- **The page's policy names one script and one style.** `page.rs` hashes what stands between the first `<script>` and `</script>` of the page, and between the first `<style>` and `</style>`, to the byte. A second element of either kind, one with an attribute, a `style` attribute and a handler in an attribute are all refused by the browser, and the page then does nothing or looks wrong, with the reason in the browser's console alone (D69).
- **In Windows PowerShell 5.1, `2>$null` on a native command can end a script.** With `$ErrorActionPreference = 'Stop'`, what the command writes to its error stream becomes an error. `scripts/osrm-prepare.ps1` sets the preference to `Continue` round such a call and pipes `2>&1` to `Out-Null`.
- **`wsl.exe -- sh -c "..."` from PowerShell loses its quoting.** Write the commands into a file and have `sh` run the file.
- **Docker Desktop starts a container whose port is taken, and says nothing.** On the development machine another program listens on `127.0.0.1:5001`. An OSRM server that was to be published there came up, was reported as started, and had no port: `docker ps` shows `5000/tcp` with nothing in front of it, and what answers on the port is the other program, with a `404`. `OSRM_PORT`, `API_PORT` and `POSTGRES_PORT` move the ports.
- **A button that is replaced between the press of the mouse and its release is not pressed.** A list that is written anew several times a second cannot have a button in it, and loses the place of whoever moves through it by keyboard. The page keeps a row of its list of routes until what it says has changed (D75).
- **To a test that reads the page for addresses, a namespace is an address.** `http://www.w3.org/2000/svg` in the page fails the test that it names no host but the tile server. The page has Leaflet make its SVG elements, with `L.SVG.create` (D75).
- **JavaScript shifts in 32 bits.** `31 << 30` is not thirty-one times two to the thirtieth. The page reads the numbers of an encoded line with multiplication, and the domain crate, which has 64 bits, refuses a number of more than seven letters.
- **OSRM's length of a way is not the length of its line from point to point.** Measured along great circles between its points, a way came out 0.2 % shorter than OSRM says it is. Whatever compares the two has to allow for that.
- **PowerShell rounds a fraction away in `[math]::Min(1, 0.5)`.** With a whole number among its arguments it takes the function for whole numbers, and the answer is `0`. Write `1.0`. It made every great-circle length nought in a script that checked the lengths of Phase 8, and none of the repository's scripts has it: `scripts/demo.ps1` takes the least of whole numbers.
- **`/api/orders/{id}` takes any one word.** A path is given to the first route it fits. `/api/orders/stops`, registered after the route that takes an id, is answered `400`: `stops` is no UUID. A path of one word under a path that takes an id has to be registered before it (D77, D78).
- **A plain `SELECT` in a CTE runs only when something reads it.** A CTE that changes rows runs whether it is read or not, but one that only selects, a `pg_notify` among its columns, is left out when the statement does not refer to it, and nobody is told. Notify in what the statement returns, as `orders::create_many` does.
- **A `SELECT ... FOR UPDATE` that waits for a row gives the row as it is after the wait.** In READ COMMITTED it looks at the row again once the other transaction has committed. That is what lets `plans::create` see a plan that was done meanwhile (D83). Without the lock the statement sees the row as it was when it began.
- **`EnvFilter` writes its directives in an order of its own.** `debug,sqlx=warn` comes back as `sqlx=warn,debug`. Compare it with a filter made of the same text.
- **`cargo sqlx prepare` is run from the root of the workspace.** Anywhere else it ends with `Failed to read Cargo.toml`.
- **A method of `Chats` in a file of its own under `chat/` is private to that file** unless it is `pub(super)`. `use super::*` there brings in the parent's types and imports, private ones too, and the fields of a struct of the parent are open to it.
- **The process that starts Edge is not Edge for long.** On Windows it hands over to another process and ends, so `taskkill /T` on the process that was started kills nothing, and the browser goes on, as three runs of `scripts/page-check.mjs` left it at first. The script tells the browser to close over the protocol, `Browser.close`, waits until it no longer answers, and ends what still runs with its profile.
- **A headless browser says where it listens in its profile.** Started with `--remote-debugging-port=0`, Edge and Chrome write the port they took into `DevToolsActivePort` in the `--user-data-dir`. `scripts/page-check.mjs` reads it from there, and so needs no free port of its own.
- **`ON CONFLICT DO UPDATE` is how one statement answers with the row that was there** (D59, D76). On an index that covers only some rows the statement has to say which: `ON CONFLICT (client_id, idempotency_key) WHERE idempotency_key IS NOT NULL`. Whether the row that comes back is new is read off its id, which the application made.
- **A function that returns nothing can stand in a `CASE`.** `CASE WHEN … THEN pg_notify(…) END` notifies for some rows and not for others, in the statement that changes them. sqlx reads the column as an optional unit.
- **utoipa says of an optional header that it may be null.** Of an optional query parameter it does not. `nullable = false` among the attributes of the parameter puts that right, and `min_length`, `max_length` and `pattern` are taken there too.
- **A line with a tooltip takes the focus of a click.** Leaflet listens for the focus of whatever has a tooltip. To Chromium an SVG element that is listened to like that is one a click gives the focus to, and it draws the outline of the focus as a box round the whole element: a black rectangle round a route. `.leaflet-interactive:focus { outline: none; }` in the page's style takes it off (D80).
- **A click on something on the map is a click on the map as well.** Leaflet passes the click of a layer on to the map unless the layer was made with `bubblingMouseEvents: false`. What is not `interactive` lets the click through to the map and has nothing to say about it.
- **`crypto.randomUUID` is there only where the page is trusted:** over TLS, or from the machine itself. Served over plain HTTP from another machine, a page has `crypto.getRandomValues` and no more.
- **A tab that is not in front is slow to show what it heard.** In a headless Edge with two tabs, the timers of the tab behind are held back. A page reads its orders a third of a second after it hears of one, by a timer, and the tab behind showed the order 5.4 seconds late where the one in front took 0.37. To try two pages at once, give each a browser of its own: another `--remote-debugging-port`, another `--user-data-dir`.
- **An answer is lost on its way back, in a browser, at the stage of the response.** `Fetch.enable` with `requestStage: 'Response'` and then `Fetch.failRequest`: the request has reached the API, and the page gets an error. At the stage of the request it never arrives, which is the other case. What is let through at that stage is let through with `Fetch.continueResponse`.
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
- For Phase 9: nothing new. The lines along the streets show where an OSRM server is asked, which takes the roads that `scripts/osrm-prepare` fetches. An OSRM server that was started before Phase 9 takes a way through 500 positions and no more: `docker compose up` makes it anew from the compose file as it is now.
- For Phase 10: nothing. An account for the client's page is opened on the page, and the admin is the one the stack starts with.
- For Phase 12: Node 22 or later and Edge or Chrome on a machine where `scripts/page-check.mjs` is run. Nothing else.
- For Phase 11, the free text of the bot: the decision whether to build it, and an Anthropic API key, made in the Claude Console at platform.claude.com, on an account with credit. A Claude subscription does not include one (D58). The phase is not begun without the key.
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
12. The map: while a plan is searched for, each route has a chart in its row that falls, for seconds if the day has 1200 orders. When the plan is done, a van sets out along each route and is back within a minute, and the page says that the vans are simulated. The button in a route's row, or a click on its line, opens its sheet, and a row of the sheet shows its stop. With the roads of step 9 the stored routes follow the streets, and so do the vans (D73 to D75).
13. The client's page, in two windows on one day. In the first, press "Open an account" with an address and a password of your own, click the map, and place the order: it is in your list as pending. In the second, as the admin, the order is on the map with nobody pressing Refresh. Press Plan there: the first window shows the order as planned, and then which van has it and which stop it is. Discard the plan, and it is pending again; withdraw it, and the admin's window says so. And by hand: `POST /api/orders` twice with the same `Idempotency-Key` is answered `201` and then `200`, with one order (D76 to D80).
14. The page by a script: `node scripts/page-check.mjs` against the stack, with `ADMIN_PASSWORD` if `.env` sets one. It plans a day a month ahead and says what it tried (D86).
