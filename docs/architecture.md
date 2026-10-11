# RouteForge architecture and decision log

RouteForge turns a day's parcel orders into courier routes. This file records how the system is put together and why it is built that way.

Decisions are numbered so that code comments and commits can refer to them. Each one says whether it is **in place** or **planned**; [plan.md](plan.md) says which phase builds the planned ones. When a phase makes a new decision or changes an old one, it is recorded here in the same commit.

## System at a glance

Three services and one database:

- **api** accepts orders, handles authentication and puts work on the queue.
- **solver** runs as several identical instances. Each one takes jobs from the queue and computes routes.
- **bot** is a Telegram assistant that places orders through the api.
- **PostgreSQL** holds both the domain data and the job queue.

There is no load balancer and no message broker. A free solver takes the next job from the queue itself.

How a plan is made:

1. An operator asks the api to plan a service date. In one transaction the api inserts the plan and one `plan` job.
2. A solver claims the `plan` job and groups the date's pending orders into one cluster per courier. In the transaction that completes that job, it marks those orders as planned and inserts one `route` job per cluster.
3. Solvers claim the `route` jobs. Each one optimizes the stop order for a single courier and writes the route in the transaction that completes its job.
4. When the last `route` job completes, the plan is marked done.

A plan that is cancelled, or that loses a job for good, undoes itself: its routes are deleted and its orders are pending again (D20). A plan that is done can be discarded, which undoes it the same way, so that its date can be planned again (D30).

How a plan is watched:

1. Every change to a plan or to one of its jobs is announced in the database, by the function of the queue that makes it and in the transaction that makes it (D48). A solver that is searching for a route also announces, four times a second, how far it has got and which route is the best so far (D49).
2. One connection of the api listens. What it hears reaches every open event stream, and a stream that hears of its plan reads the plan again and sends what has changed (D50).
3. The page, which the api serves at `/`, reads the stream and draws: a dashed line for a route that is still searched for, a solid one when it is stored (D52).

How it is run:

1. `docker compose up --build --scale solver=4`, on a clone with no settings, brings up the database, the api, four solvers and the bot (D54).
2. The api waits for the database to be healthy and applies the schema. The solvers wait for the schema (D32). The bot waits for the api.
3. `scripts/demo` shows the two claims about the workers on that stack: more solvers finish a plan sooner, and a solver that dies loses no route (D57).

## Crates

The workspace has exactly five crates, as the [approved description](description.md) requires.

| Crate | Role | Workspace crates it may use |
|---|---|---|
| `routeforge-tsp` | Clustering and route optimization. Pure computation. | none |
| `routeforge-domain` | Domain types, DTOs and geo helpers. With the `db` feature: persistence and the job queue. With the `openapi` feature: the schemas of the DTOs. | none |
| `routeforge-api` | HTTP API, auth, OpenAPI, the map page. | `domain` with `db` and `openapi` |
| `routeforge-solver` | Queue worker. | `domain` with `db`, `tsp` |
| `routeforge-bot` | Telegram bot. | `domain` without `db` |

## Decisions

### D1. One order is one stop, relative to a depot (in place)

An order is either a delivery from the depot to a client or a pickup from a client back to the depot. Couriers start and end at the depot.

Why: this is the capacitated vehicle routing problem, which the approved algorithms solve directly. The alternative, point-to-point orders with their own pickup and drop-off address, needs "pickup before drop-off" ordering that sweep clustering and plain 2-opt do not respect.

The schema has had it since Phase 2: an order has one kind, `delivery` or `pickup`, and one position.

### D2. A plan is one `plan` job that fans out into `route` jobs (in place)

Why: optimizing one courier's route is the expensive part, and the routes of a plan are independent of each other. With one job per route, adding solver instances shortens a plan. With one job per whole plan, extra instances would help only when several plans were queued.

Cost: the plan must be marked done exactly once when its last route job finishes, including when two finish at the same moment. A counter on the plan row, incremented in each completing transaction, serializes them.

The counter is `plans::route_finished`. Two tests complete route jobs at the same moment, once with two transactions that are made to overlap and once with eight at a time. In both, exactly one completion sees the plan become done.

How the solver runs the two kinds of job is in D34 and D36.

### D3. Persistence lives in `routeforge-domain`, behind the `db` feature (in place)

Why: both `api` and `solver` need the repositories and the queue, and the workspace is fixed at five crates, so there is no room for a separate storage crate. The feature keeps sqlx out of the bot, which reaches the system only over HTTP. CI builds the bot on its own to prove that.

### D4. Costs are integer metres (in place)

The distance matrix holds `u32` metres. The length of a tour is a `u64` sum and the change a move makes is an `i64`, so neither can overflow.

Why: the change in tour length caused by a move is then exact. Local search cannot keep "improving" by floating-point noise, and tests can assert that the incrementally tracked cost equals the recomputed one.

### D5. The search is deterministic; a deadline only cuts it short (in place)

All randomness comes from a seeded ChaCha8 generator. Annealing cools by iteration count, in fixed-length epochs with reheats. The deadline and the cancel flag stop the search; they never steer it.

Why: the description promises both "works until its deadline" and "deterministic given a seed", and a search steered by the clock cannot be reproduced. With this design the same seed and the same iteration budget give the same route, and a run limited by a deadline returns a prefix of that same search. Tests use iteration budgets. One of them checks the prefix property directly: a larger budget never gives a longer route.

The promise holds per build: the same binary, seed and budget give the same route. `exp` in the acceptance test and `atan2` in the sweep come from the platform's maths library, whose last bit may differ between operating systems, so a bit-identical route across platforms is not promised. No test depends on one.

The workspace manifest compiles `rand` without OS entropy, so `rand::rng()` and `rand::random()` do not exist in any build and unseeded randomness cannot reach `routeforge-tsp` by accident. proptest, a dev-dependency, switches on rand's `sys_rng` feature in test builds only; `cargo check -p routeforge-tsp --lib` builds the library without it.

### D6. Jobs are delivered at least once; completion is fenced (in place)

A worker holds a job under a lease and renews it with a heartbeat. If the worker dies, the lease expires and another worker takes the job. Results are written in the same transaction that marks the job done, and only if the worker still holds the lease.

Why: with a lease, a slow worker and its replacement can both finish the same job. Fenced completion turns the second one into a no-op, so results are stored once. Lease times are computed with the database clock, so clock differences between workers do not matter.

How it is built is in D22 to D26. A worker passes spans of time, how long a lease is to run, and no call returns a timestamp, so a worker has nothing to compare with its own clock.

### D7. Capacity, first version: all demands on a route share the limit (in place)

A route is feasible when the sum of its demands, pickups and deliveries together, is within the courier's capacity.

Why: it never overloads a courier and needs no knowledge of the stop order. It is stricter than necessary, because a courier who delivers before picking up could carry more in total. Tracking the real load along the route is planned for Phase 8.

### D8. The demo works without the internet (in place, but for the bot)

Distances default to Haversine. The Leaflet library is vendored into the repository. Map tiles are optional: without them the routes still draw. The bot idles when its Telegram token is missing.

Haversine has been the default since Phase 3. The page and its Leaflet are in the binary of the API, and a test fails if the page names any host but the tile server (D52). The bot is Phase 7. Until then a stub that idles has its place in the stack (D56).

Building is not part of this. A build asks the registry for its base images, and the first one fetches every crate. Once the images are there, `docker compose up` without `--build` starts them and asks nobody (D54).

### D9. The bot has no authority of its own (planned)

The bot calls the api with a short-lived token of the Telegram user it is talking to. Actions with side effects are confirmed with a button, and the confirmation is handled in code.

Why: whatever the language model decides, the api applies that user's role, so the model cannot do more than the user could. The confirmation step keeps a misread message from creating or cancelling anything.

### D10. Rust 1.99.0, pinned (in place)

`rust-toolchain.toml` pins the compiler for developers and CI. The Docker image is built with the same version, which the Dockerfile repeats in the tag of its base image (D38).

Why: sqlx 0.9 needs Rust 1.94 or newer, and one compiler everywhere removes differences between machines.

Check before raising the pin: teloxide 0.17 depends, through `aquamarine`, on `proc-macro-error2` 2.0.1. Rust 1.99 compiles it with a future-incompatibility warning (rust-lang/rust#127909), and a later compiler will reject it. The crate is also marked unmaintained (RUSTSEC-2026-0173).

### D11. PostgreSQL 17 (in place)

Why: the queue needs `FOR UPDATE SKIP LOCKED` and `LISTEN/NOTIFY`, which 17 has, and nothing in the design needs a newer server. Identifiers are UUIDv7 generated in the application, so the schema does not depend on a server-side generator.

### D12. One TLS stack, no native crypto libraries (in place)

- Outgoing HTTP uses reqwest 0.12 with rustls and ring. reqwest 0.13 exists, but teloxide 0.17 is built on 0.12, and mixing the two would put two HTTP stacks and two crypto libraries into the bot.
- JWT handling uses the pure-Rust backend of jsonwebtoken.
- sqlx is built without TLS, because Postgres is reached only on localhost or the compose network.
- actix-web is built with only its `macros` feature.

Why: the build needs no OpenSSL, CMake or NASM, on Windows or in the Docker image.

Side effect: the pure-Rust jsonwebtoken backend compiles in the `rsa` crate, which has an open advisory with no fixed release (RUSTSEC-2023-0071, a timing side channel in RSA private-key operations). RouteForge uses HS256 tokens and never performs an RSA operation, so the advisory is ignored in `.cargo/audit.toml` with that reason. The ignore must be removed if tokens ever move to an RSA algorithm.

### D13. Lint baseline (in place)

`unsafe` is forbidden in every crate. `unwrap()` is rejected outside tests, as are `dbg!` and `todo!`. CI runs clippy with warnings as errors.

Clippy's exemption for tests covers `#[cfg(test)]` modules and `#[test]` functions. The unit tests of `routeforge-tsp` therefore live inside the crate, where their helpers may unwrap. Code under `tests/`, `benches/` and `examples/` is linted like production code.

### D14. Node 0 is the depot, and the distance matrix is symmetric (in place)

`optimize_route` takes a matrix whose node 0 is the depot. A tour is the closed cycle from node 0 through every other node and back. `DistanceMatrix::new` rejects a table in which the distance from A to B differs from the distance from B to A.

Why: 2-opt reverses part of a tour and Or-opt may turn a segment round. Both compute the new length from the few edges that change, which is only right when the reversed part keeps its length. Checking once, when the matrix is built, makes that a property of the type instead of an assumption in every stage.

Consequence for Phase 8: OSRM road distances are not symmetric. The solver must symmetrize the table before it builds the matrix, and compute the real directed length of the finished route itself.

### D15. One control for all stages; one iteration is one candidate move (in place)

A search gets one `Control`: an iteration budget and/or a deadline, an optional cancel flag and an optional progress callback. Local search and annealing draw on the same budget. One iteration is one candidate move evaluated. Annealing asks for one iteration per proposal; local search asks for a whole scan row at a time and does not start a row the budget cannot cover. The cancel flag, the deadline and the callback are looked at every 16 384 iterations. Construction is not charged and always completes.

Why:
- One budget for the whole call bounds the total work, whatever the size of the route.
- A row is charged before it is scanned, so the budget is never exceeded and a budget cut falls at a point that depends on the input alone.
- Polling every 16 384 iterations keeps the flag and the clock out of the inner loop and still reacts in well under a millisecond: annealing tries 35 to 50 million moves a second. The clock is not read at all unless a deadline is set, so runs under an iteration budget do not depend on time.
- Because construction always completes, there is a valid tour to return even when the search is cancelled before its first move.

A `Control` cannot be built without a budget or a deadline. Annealing has no end of its own (D18), so a control without a limit would be a search that never returns.

Since Phase 5 the callback is handed the best tour so far beside the two numbers, and the search reports once more as it returns (D49).

Good to know when choosing a budget: it counts local search moves too, and Or-opt has many. Starting from a nearest-neighbour tour, local search alone takes 30 to 45 thousand iterations at 50 stops, more than a million at 250 and about 25 million at 1000. Annealing gets what is left.

### D16. Sweep: largest courier first, then repair (in place)

`cluster` sorts the stops by their direction from the depot and walks round that circle. It fills one courier after another, the largest first. A stop that does not fit the current courier opens the next one if that courier can take it; otherwise the stop is passed over and the current courier stays open. Afterwards each stop that was passed over, largest first, goes into the cluster that has room for it at the place where it adds the least distance.

The walk is repeated from up to 36 evenly spaced start positions. The result is the one with the fewest unassigned stops and, among those, the shortest total when every route is driven in sweep order.

Why:
- With the couriers in descending order, a stop too large for the next courier is too large for every later one. That makes the walk a single pass.
- A plain sweep can leave a stop out although an earlier courier still has room. The repair removes that case, and with it comes a guarantee that tests can check: an unassigned stop fits into no courier's remaining capacity.
- The start position decides where the circle is cut into sectors, and a bad cut splits a group of neighbours. Trying several is cheap, since one walk is linear in the number of stops.

Every stop ends up in exactly one cluster or in `unassigned`, whatever the input. Coordinates that are not finite give an arbitrary clustering, but still a complete one.

### D17. The annealing schedule (in place)

An epoch has 100 temperature levels. It starts at a temperature of half the mean edge length of the tour that annealing was given, and the temperature is multiplied by 0.97 from one level to the next, so an epoch ends at about a twentieth of where it started. Each level tries 20 moves per node. A move is a 2-opt reversal half of the time; relocating one node and swapping two share the other half. At the end of an epoch the best tour seen goes through local search, and the next epoch starts from it at the start temperature again.

Why these values: they come from a sweep over the three TSPLIB instances in the tests (eil51, berlin52, kroA100) and random instances of 30 to 250 stops, with 16 seeds each at budgets of 0.2, 1 and 5 million moves. This setting was the best one at 1 million moves, tied for best at 5 million and within 0.1 percentage points of the best at 0.2 million. Lower start temperatures were clearly too cold: at a tenth of the mean edge the search never left the local optimum it started from on berlin52, and at a quarter it rarely did. Splitting an epoch of the same length and temperature range into 25, 50 or 200 levels changed little.

With these values and a budget of 1 million moves the three TSPLIB instances end, averaged over 16 seeds, 0.4 %, 0.0 % and 0.3 % above their optima.

Why the temperature is tied to the mean edge: distances are in metres, and a city-wide route and a neighbourhood route differ by an order of magnitude. A fixed temperature would suit only one of them.

Why the best tour is polished inside the search and not after it: the best tour of an epoch comes from random moves and is usually not a local optimum. Polishing at the end of each epoch is part of the fixed sequence of the search, so it is covered by the budget and the deadline like everything else. A polish after the stop would run past the budget or the deadline.

### D18. Annealing runs until it is stopped (in place)

There is no convergence test. `optimize_route` returns when the budget is used up, the deadline has passed or the cancel flag is set. Routes with up to three stops are the exception: local search is exact for them, so they return at once.

Why: "anytime" means the caller decides how long a route is worth. A convergence test would be a second, hidden stop condition, and the description asks for a solver that works until its deadline.

Consequence for the solver: a route job with four or more stops takes the whole budget its plan gives it (D31). The budget per route is therefore what sets the length of a plan.

Deliberately not built in Phase 1, because the plan does not ask for them: neighbour lists to speed up large instances, an exact solver for small routes, and a "stop when nothing improves" option.

### D19. Schema conventions (in place)

- An enumeration is a `TEXT` column with a `CHECK`, not a PostgreSQL enum type.
- An id is a UUIDv7 made by the application (D11). Every table has its own id type in Rust, so an order id cannot be passed where a plan id is expected.
- A value that is a `u32` in Rust is a `BIGINT` with a range check.
- Every timestamp is the database's `now()`.
- The domain types have no sqlx in them. The `db` module reads rows into private structs and converts.

Why:

- Text needs no cast in a query and no type override in the compile-time checked macros, and a new value is a change to one `CHECK`. The price is that the values are listed twice, in the Rust enum and in the `CHECK`. One macro, `text_enum!`, gives each enum a single text for JSON, the database, `Display` and `FromStr`, and a test stores every value of every enum, so the two lists cannot drift apart unnoticed.
- PostgreSQL has no unsigned integers. As a `BIGINT`, everything a `u32` can hold can be stored, so a write never fails on range. Reading converts back. A value outside the range is an error, `DbError::Unreadable`, and so is a status text or a job payload that the build does not know. None of them is a panic.
- The bot uses the same types without the `db` feature (D3). With sqlx kept out of them there is no `cfg_attr` on every type, and they can hold what a flat row cannot: a `LatLon`, a `u32`, an enum.

### D20. An order is on one route at most, and a plan that ends early undoes itself (in place)

An order is `pending`, `planned` or `cancelled`. When a plan is clustered, the pending orders of its date that fit a courier become `planned` and carry the plan's id. The rest stay pending. `route_stops.order_id` is unique. A plan that fails or is cancelled deletes the routes it had stored and makes its orders pending again, in the transaction that ends it.

Why:

- "Every order routed exactly once" is what the system is judged by. Fenced completion (D6) already guarantees it. The unique constraint keeps it true even if a later change breaks the fencing.
- A plan takes its orders under a row lock (`orders::lock_pending`). Without it a client could cancel an order between the moment it was counted into a cluster and the moment it was marked. With it one of the two is first and the other sees that.
- After a plan that undid itself the date looks as it did before, and can simply be planned again. Half a plan would need rules of its own: which of its routes count, and which orders are free.

A plan that is done keeps its orders. A later plan for the same date takes the orders that are still pending and adds its routes to those that are there. Getting the orders of a done plan back is D30.

### D21. The depot has versions (in place)

Setting the depot inserts a row, and no row is ever updated. The newest row is the current depot. A plan records the id of the depot it was made for.

Why: the clusters and the routes of one plan are computed by different jobs, possibly minutes apart. With one row that is updated in place, moving the depot in between would give routes that start somewhere other than the place the clusters were formed round, and every old plan would be redrawn from the new place. An id on the plan rules out both, and needs no lock.

### D22. The job row and the claim (in place)

A job is `queued`, `running`, `done`, `failed` or `cancelled`. `run_at` says from when a queued job may be claimed. `lease_expires_at`, set exactly while the job is running, says from when another worker may take it over. A job is due when it is queued and `run_at` has passed, or running and its lease has expired.

A claim is one statement. A subquery selects the oldest due job `FOR UPDATE SKIP LOCKED`, and the update round it makes the job `running`, adds 1 to `attempt` and sets the lease. A partial index over the unfinished jobs serves it, so the finished jobs, which pile up, are never scanned.

`attempt` is the fencing token of D6. A worker gets the value with its claim and shows it with every later call. A call changes the job only while the job is still `running` for that worker with that value.

A lapsed lease alone does not shut a worker out. Until another worker claims the job, its heartbeat extends the lease again and its completion is accepted.

Why:

- `SKIP LOCKED` is what makes "whoever is free takes the next one" work without a coordinator. Concurrent claimers neither wait for each other nor get the same job.
- Two columns, each with one meaning, read better than one "visible from" column that serves both cases.
- The worker's id cannot be the token. A worker can lose a job and claim the same job again, and then only the attempt number tells the abandoned work from the current one. One test covers exactly this case. It is the only test that fails when the comparison of the number is weakened.
- Refusing a late worker when nobody else has taken the job would throw finished work away and make nothing safer. What makes a job safe to complete is the row lock that `begin_complete` takes, not the time left on the lease.

### D23. Retries, final failure and cancelling (in place)

- A job may be claimed 3 times. After a failed attempt it is due again after 2 s, then after 4 s: `queue::retry_delay` doubles from 2 s up to a minute. The delay is passed to the database as a span and added to its clock.
- A claim counts as an attempt even when the worker dies without a word. `queue::release`, which a worker calls when it shuts down, gives the attempt back.
- When the last attempt fails, the job is `failed` and so is its plan, with the job's error. The plan undoes itself (D20).
- A job whose worker died during the last attempt cannot be claimed again, and nobody is left to report it. A worker that finds nothing to claim fails such jobs on its way out of `queue::claim`.
- Cancelling is a change of state, not a flag to be acknowledged. `queue::cancel_plan` makes the plan and its unfinished jobs `cancelled` in one transaction. A worker in the middle of one of them is told `Cancelled` by its next heartbeat, and its completion is refused by the fence like any stale one.

Why:

- Counting the attempt of a dead worker bounds the damage of a job that kills whoever runs it. Otherwise such a job would go round for ever. A shutdown says nothing against the job, so it costs the job nothing.
- A plan with one route missing is not a plan, so a job that is given up takes its plan with it.
- Burying from an idle `claim` keeps the queue free of a process of its own, in line with "no load balancer and no broker". The price is that such a job is noticed only when some worker is idle, which is when the queue has nothing more urgent to do.
- A cancel that waited for the worker to acknowledge it would hang on a dead worker. As a change of state it is complete the moment it commits, and it needs nothing that the fence does not already provide.

`docs/plan.md` first called the function `request_cancel`. It was renamed because nothing is requested.

### D24. Lock order: the plan row first (in place)

Three operations take more than one lock on a plan and its jobs: completing a job, the final failure of a job, and cancelling a plan. Each locks the plan row before any job row.

Why: with the job first, a completing route holds its job and waits for the plan, to count itself, while a cancel holds the plan and waits for that job. PostgreSQL would break the deadlock after a second by failing one of the two. Locking every job in id order before the plan has no cycle either, but a job that fails has to lock itself first, which breaks that order. Plan first is the one rule all three can follow.

Claim, heartbeat, release and the retry of a failed attempt lock one job row in one statement. They hold nothing while they wait, so they cannot be part of a cycle.

A test races completions, a final failure and a cancel in every starting order, and checks that none of them fails and that the plan ends whole: done with all its routes, or over with none. With the plan lock taken out of the completion, or out of the final failure, that test fails with PostgreSQL's "deadlock detected".

### D25. Completing a job is a handle, not a closure (in place)

`queue::begin_complete` opens a transaction, locks the plan, locks the job if the fence holds, and returns a `Completion`. The caller writes the job's results through `completion.conn()` and calls `commit()`, which marks the job done. Dropping the completion rolls everything back and leaves the job under its lease.

Why: D6 asks for the results to be written in the transaction that marks the job done. The obvious shape, a closure that is handed the transaction, has to be an async closure that borrows it, and the future of such a call is hard to prove `Send`, which `tokio::spawn` in the solver needs. A value with two methods does the same and has no such problem.

### D26. A notification is a hint (in place)

`queue::enqueue` inserts the job and calls `pg_notify` in one statement, on the channel `routeforge_jobs`, with the kind of the job as payload. `queue::release` notifies as well. PostgreSQL delivers a notification when the transaction commits. So a worker never hears of a job it cannot see yet, and a job enqueued in the transaction that completes another, as route jobs are, is announced at the moment it appears.

A job that becomes due because its retry time has come, or because a lease has expired, announces nothing. That, and a listener that was reconnecting at the wrong moment, is why a worker polls as well.

Why the kind as payload: PostgreSQL folds equal notifications of one transaction into one. A plan that fans out into twenty route jobs wakes every worker once, not twenty times. Once is enough, because a worker that is awake claims again after each job.

This channel is for the workers alone. Whoever shows a plan to a person is told on two channels of their own (D48).

### D27. The worker registry is not part of the queue (in place)

`workers` has a row per worker: its id, its host name, when it started and when it was last seen. A solver process runs one worker unless it is told to run more (D33), and a worker marks itself as seen once per heartbeat interval. `jobs.claimed_by` holds a worker's id and is not a foreign key to that table.

Why: the registry is there for the load view. If the queue depended on it, a worker that could not register could not work, and a row could not be removed while a finished job still named it. As it is, the registry can be wrong or empty and no job is lost.

### D28. Query metadata is committed, and a build needs no database (in place)

Every query in `routeforge-domain` goes through sqlx's compile-time checked macros. What they need to know about the schema is stored in `.sqlx/` at the workspace root, and `.cargo/config.toml` sets `SQLX_OFFLINE=true`, so a build reads that directory and connects to nothing. `scripts/sqlx-prepare` rewrites the directory from the compose database after a query or a migration has changed. With `--check` it only compares, and the CI workflow has that as a step, so a stale directory fails the build.

Why:

- The macros go online whenever they can see a `DATABASE_URL`, and `.env` has one, for the tests and for compose. A fresh clone therefore has the URL and an empty database, and would not compile. Offline by default makes "clone, `docker compose up -d db`, `cargo test`" work, and the Docker image of Phase 3 needs no database to build.
- `sqlx-cli` is the one tool this adds, and only for whoever changes a query. It is installed with Postgres support alone and without TLS, so it needs no OpenSSL (D12). The project does not depend on it.
- Tests use plain `sqlx::query` for their own setup and assertions, so changing a test does not change `.sqlx/`.

Each database test gets a database of its own from `#[sqlx::test]`, with the migrations of the crate applied. The crate has a dev-dependency on itself with the `db` feature, so `cargo test -p routeforge-domain` cannot skip those tests by leaving the feature out. A dev-dependency does not reach the bot: `cargo check -p routeforge-bot` still builds the crate without `db`.

A migration is not edited once any database but a developer's own has seen it. sqlx keeps a checksum of each and refuses one that has changed.

### D29. Distances and the flat map (in place)

- `haversine_m` is the great-circle distance on a sphere of the mean Earth radius, 6 371 008.8 m.
- `DistanceProvider` has one method. It returns a future, and the trait has an associated error type. The result is a `DistanceTable`: the distance from every position to every other in whole metres (D4), directed.
- `Projection` is an equirectangular map round one position, for the sweep.
- A `LatLon` cannot hold a latitude or longitude outside the globe, or a NaN. Construction and deserialization check, and the schema repeats the range.

Why:

- The provider behind OSRM (Phase 8) does HTTP, so the trait cannot be synchronous. It can also fail, which `Haversine` cannot: its error type is `Infallible`. The method is declared to return `impl Future + Send`, so the solver can call it from a spawned task.
- The table is directed because road distances are (D14). The domain crate may not use `routeforge-tsp` (see Crates), so it cannot return a `DistanceMatrix`. The solver builds one, and symmetrizes first when it has to.
- The sweep needs the direction of each stop from the depot and a rough distance. On an equirectangular map round the depot, distances are within 2 % of the globe for stops up to 0.3 degrees from the depot each way, which is 33 km to the north and south, at latitudes up to 60 degrees. A property test checks that. A real map projection would be a dependency for nothing.
- A sphere is within about 0.5 % of the ellipsoid. A straight line differs from a road by far more.

### D30. A plan that is done can be discarded, and its date planned again (in place)

Decided by the project owner on 2026-10-11.

Discarding undoes a plan that is done, the way a plan that fails or is cancelled undoes itself (D20): its routes are deleted and its orders are pending again. The plan row stays, with the status `discarded`. The operator then plans the date again, and the new plan takes every pending order of the date. Only a plan that is done can be discarded. One that is still in progress is cancelled instead.

A date whose plan is not discarded is planned as before: a later plan takes the orders that are still pending.

Why:

- Without it a plan could never be corrected. After a courier is added, or after a run that was given too little time, the date could not be planned afresh, and an order that a plan holds could never be withdrawn.
- The verification steps in `docs/plan.md` plan the same orders twice: a second time with the same seed, and once with one solver and once with four. The second run needs the orders back.
- Keeping the old routes until the new plan is done would leave no moment without routes. It would also put an order on two routes at once, an old one and a new one, which is what the unique constraint of D20 rules out. The price of a discard is that the date has no routes until the new plan is done.
- Discarding and planning stay two steps, so each does one thing. A discard made by mistake costs the routes and nothing else: the orders are all there, pending.

How it is built:

- Migration `0004_discarded.sql` adds `discarded` to the two `CHECK`s that list the statuses of a plan.
- `queue::discard_plan`, beside `queue::cancel_plan`, is one transaction. Its first statement makes the plan `discarded` if it is `done`, which locks the plan row before anything else, as every operation on a plan does (D24). Then come the two statements that undo a plan, which `end_plan` runs as well and which are now `undo_plan`.
- A plan that is done has no job left on the queue. So a discard touches no job, and no worker has to be told.
- The plan keeps its `finished_at`, the moment it was done. When it was discarded is not recorded.
- The API offers it as `POST /api/plans/{id}/discard`, to operators and admins. A plan that is not done is answered `409`, with what can be done to it instead.

Tests: a discard frees the orders and keeps the row; a plan that is queued, routing, cancelled or failed cannot be discarded; a date that was discarded is planned again and the new plan is offered every order of it; and of several discards at the same moment exactly one succeeds.

### D31. How long a route is searched for is part of the plan (in place)

Decided with the project owner on 2026-10-11.

A plan carries a budget for each of its routes: a span of time, a number of candidate moves, or both, in which case whichever is reached first ends the search. These are the two limits of the search itself (D15). `plans.budget_time_ms` and `plans.budget_iterations` hold them, and a `CHECK` wants at least one. In Rust the budget is a `RouteBudget`, which cannot be built, or read from JSON, without a limit. A route job reads it from the row of its plan, which it reads anyway for the depot.

Why:

- Two promises of the description pull apart here. "Works until its deadline" is a budget of time. "Deterministic given a seed" needs a budget of moves, because a clock cuts the search wherever the machine has got to (D5). With both on offer, a demo runs for a known time and the determinism check runs on moves.
- On the plan, every route of a plan gets the same budget whichever solver runs it. As a setting of the solver, instances that were configured differently would give one plan routes of different quality, and changing the budget would mean restarting them all.
- A budget without a limit would be a search that never returns (D18). Ruling it out in the type and in the schema is cheaper than checking for it wherever a budget is used.

Each attempt of a route job gets the whole budget. The solver measures the time itself, from the moment the search starts. It is a span, like every time a worker handles (D6).

The schema accepts any time a `u32` of milliseconds can hold, which is seven weeks. The API gives the budget a default and an upper limit (D45).

### D32. The API owns the schema, and a solver waits for it (in place)

Decided by the project owner on 2026-10-11.

A solver applies no migrations. After it has connected it asks once a second whether the database has every migration its build knows (`db::is_migrated`), and starts its workers when it has. The API applies the migrations when it starts, before it serves anything (D47).

Why:

- The schema has one owner. With two kinds of service migrating, each would have to be trusted to carry the newest migrations.
- A solver that is older than the schema still starts: a database with migrations the build does not know counts as migrated. That is in line with how the queue treats a job such a solver cannot read (D22). Had the solver run the migrator itself, the migrator would refuse a database that is ahead of it.
- The stack needs no start order. A solver that comes up before the API waits.

The two sides treat a database that is ahead of them differently, and on purpose. A solver takes it as migrated and works. The API refuses to start: sqlx's migrator does not run against migrations it does not know, and the API, which writes to every table, must not write to a schema it has not seen.

Cost: a database gets its schema only when an API has started against it, and on any other a solver waits for ever. Since Phase 6 the stack has an `api` service, so `docker compose up` brings one (D54). `scripts/sqlx-prepare` applies the migrations as well, since it needs them to describe the queries.

Tried in containers: two solvers started against a database that had just been created said that they were waiting, and began to look for jobs within a second of the API having applied the schema.

`is_migrated` reads `_sqlx_migrations`, the table sqlx keeps its own books in, through a compile-time checked query like every other (D28). The metadata in `.sqlx/` is therefore prepared against a database that has been migrated, which the script and the CI step both see to.

### D33. The worker loop (in place)

A worker does one job at a time. It claims until nothing is due, then waits for whichever comes first: a wake-up, its poll interval, or the order to stop.

- **Wake-ups.** One connection per process listens on the jobs channel and counts the notifications. A worker notes the count before it claims and waits for it to change after, so a job that is announced between the claim and the wait is not missed. When the listener has had to reconnect it counts as well, because a notification may have been lost meanwhile.
- **Polling.** A job that becomes due because its retry time has come or its lease has run out announces nothing (D26). So an idle worker also looks at the queue every `SOLVER_POLL_SECS`, 5 by default, give or take a quarter. The quarter comes from a generator seeded with the worker's id, so that workers started together do not ask in step. `rand` is built without the operating system's entropy (D5), and the id is random already.
- **Leases.** `SOLVER_LEASE_SECS` is 30 and `SOLVER_HEARTBEAT_SECS` is 5 by default. The lease has to be at least twice the heartbeat, so that one heartbeat that does not get through does not cost the job.
- **Concurrency.** A process runs `SOLVER_CONCURRENCY` workers, 1 by default. Each has an id and a registry row of its own (D27), and they share the pool and the listening connection. Scaling is by instances, as the description says. More workers in one process are for running a few without Docker.
- **Spans.** What a worker logs is inside a `worker` span with its id, and what it logs about a job is inside a `job` span with the ids of the job and the plan, the kind, the attempt and the id of the request that asked for the plan.

Why:

- Claiming until nothing is due is what makes one notification enough for a plan that fans out into twenty jobs (D26).
- The claim is not raced against the order to stop. A claim abandoned half way could have taken a job that nobody then knows of, and the job would wait out its lease.
- One listening connection for the process, not one for each worker: a notification means "look at the queue" to every worker alike, and a connection that only listens is a connection less for work.

Two tests keep the two ways of waking apart. In one, three workers with a poll interval of a minute take up a plan that is submitted while they wait, which nothing but the notification can make them do. In the other, the lease of a dead worker runs out some two seconds after the other workers have run out of work, and they take its job over, which nothing but polling lets them do.

### D34. A lease lives exactly as long as the job's future (in place)

While a route is searched for, a heartbeat extends the lease every `SOLVER_HEARTBEAT_SECS` and stores the progress the search last reported. When it is answered `Cancelled` or `Lost`, the worker stops the search and drops what it had found.

The search runs on a blocking thread. The heartbeat is a future polled beside the wait for that thread, in the same task, and not a task of its own. The search is a value, `Search`, that tells its thread to stop when it is dropped.

Why:

- A job's future can end in ways that nothing inside it sees: the worker is told to stop, its task is aborted, or it panics. With the heartbeat inside the future the lease stops being extended in each of these cases, and the job passes to another worker. A heartbeat in a task of its own would have to be stopped by hand in each, and one that was forgotten would keep a lease with nobody working behind it, for good.
- The same goes for the search. A thread cannot be dropped. Without the flag, a search whose job is gone would run to the end of its budget and hold a processor for nobody, and a solver that is stopping would not exit until it had.

`docs/plan.md` first called it a heartbeat task. This is why it is not one.

The search writes its progress to a `watch` channel every 16 384 moves (D15), and the heartbeat stores what it finds there. So `jobs.progress` is as fresh as the last heartbeat.

Since Phase 5 a third thing is polled beside the search, and for the same reason: the announcer, which tells whoever watches the plan how far the search has got (D49). The event streams of the API follow the rule too: a stream is a value its response owns, with no task behind it (D50).

Tests: a search that is dropped stops, and so does one whose owning task is aborted; a route that takes two and a half times its lease is not taken over while an idle worker looks for work; a cancel frees three workers that each had two minutes of search ahead of them; a job that another worker has claimed is given up.

### D35. A worker that is told to stop drops its job and gives it back (in place)

On Ctrl-C or SIGTERM a solver stops claiming. A worker in the middle of a job drops the job's future and calls `queue::release`: the job is due at once and has used no attempt (D23). Both kinds of job go the same way. Dropping a `plan` job rolls its completion back (D25), and dropping a `route` job stops its search (D34).

Why:

- `docker stop` allows ten seconds. Finishing the job first would take the rest of its budget. Giving it back takes one query.
- There is one way out for both kinds of job, and an abort or a panic takes the first half of it too. So it is not a path that only ever runs at shutdown.

A job that completes in the very moment the order comes is not harmed either way: `release` changes nothing when the job is already done.

In a container, a solver in the middle of a route was stopped with `docker stop` and had exited within half a second. The job was queued again with no attempt used, and another solver finished it.

### D36. The `plan` job is one transaction (in place)

Everything a `plan` job does happens inside its completion (D25). It locks the pending orders of the date, clusters them, marks those that fit a courier as planned, enqueues a route job for each cluster and moves the plan to `routing`. Clustering itself runs on a blocking thread.

What follows from that:

- An order cannot be withdrawn between being counted into a cluster and being marked (D20).
- The route jobs appear, and are announced, in the moment the plan becomes `routing` (D26).
- A job that fails half way leaves nothing behind: no order marked, no route job.
- The job sends no heartbeat. `begin_complete` locks the job's row and a claim skips a locked row, so the job cannot be taken over while the transaction is open, however long it takes.

The seed of a route is the plan's seed in the high half and the number of its cluster in the low half. Clustering gives the same clusters in the same order for the same orders and couriers, and both are read in the order of their ids. So a plan that is run again with its seed gives every route its seed again (D5). A test plans the same day twice and compares the routes. One of its routes has a hundred stops and a budget that ends in the second round of annealing, because on routes of a handful of stops any seed finds the same best tour and the test would prove nothing.

Each route job carries the request id of the plan job. That is how one request can be followed into the log of every solver that worked on its plan.

### D37. What counts as a failed attempt (in place)

- An error while a job runs is reported with `queue::fail`, with the chain of its causes as the text. The queue retries the job or, after the last attempt, fails it together with its plan (D23). The reason is then on the plan for the operator to read.
- A panic in the search is such an error.
- A job this build cannot read is logged and the next one claimed. The queue has counted that attempt already (D22).
- Trouble with the database outside a job is not held against any job. A claim that fails is tried again after the poll interval, a heartbeat at the next heartbeat, and a connection at startup after a pause that doubles up to 30 seconds.
- A failure that cannot be reported is left alone. The lease runs out, and the queue treats the job as it treats that of a worker that died.

Why: the queue already bounds the damage of a job that takes its worker down with it. Reporting the reason makes the same outcome quicker and readable. And a database that is away for a minute says nothing about any job, so it must not use up the attempts of the jobs that happened to be running.

### D38. One Dockerfile, a target for each service (in place)

The Dockerfile at the root builds every service, and each service is a target: `docker build --target solver .`. Phase 3 made the `solver` target, and Phase 6 the targets `api` and `bot`.

- **cargo-chef.** The dependencies are built as a layer of their own, from the manifests and the lock file alone. A change to the workspace's crates then rebuilds in seconds. cargo-chef is pinned, like everything else.
- **Base images.** The Rust image of the pinned compiler to build, Debian slim to run. `rust-toolchain.toml` is kept out of the build context: the tag of the base image is the pin (D10), and with the file there rustup would also fetch the clippy and rustfmt it names.
- **What the build needs.** No database, because queries are checked against `.sqlx/` (D28), and nothing native that the base image does not have (D12). The solver's binary links to the C library and nothing else, so the runtime image needs neither OpenSSL nor CA certificates. It runs as a user without privileges.
- **Compose.** The `solver` service has no container name and publishes no port, which is what lets `--scale solver=N` work. It is given the database's address on the compose network. The solver's other settings are passed on when they are set in `.env` and left to their defaults when they are not.

Why one file: the services are binaries of one workspace and share almost all their dependencies. With one builder stage they are compiled once, however many targets are built.

The builder stage builds the three packages with one `cargo build`. Cargo then gives a crate they share the features of all three, so in the image the bot is linked against a domain crate that was compiled with `db` and `openapi`, as it is by `cargo test --workspace` on any machine. What the bot's own manifest asks for is unchanged, and `cargo check -p routeforge-bot` in CI is what holds it to that (D3).

The binaries are 16 MB for the API, which has the page and Swagger UI in it, 6 MB for the solver and 1 MB for the stub of the bot. Each links to the C library, `libm` and `libgcc`, and to nothing else. The rest of an image is Debian: the images are 140, 126 and 119 MB.

CI builds the `solver` target in a job of its own, so a dependency that needs a native library fails there and not on the day of the demo. The builder stage is shared, so that job compiles all three services. The targets `api` and `bot` themselves, and the stack, are built and started by hand: `docker compose build`.

### D39. The HTTP API: one prefix, plain JSON, one shape of error (in place)

Everything the API serves as data is under `/api`. Beside it are `/healthz` and `/readyz`, the OpenAPI document at `/api/openapi.json` and Swagger UI at `/docs/`. The page is at the root, and what it loads under `/vendor/` (D52).

Two endpoints answer with `text/event-stream` and go on answering: `/api/plans/{id}/events` and `/api/system/stats/events` (D50). A caller they refuse is answered as by any other endpoint, with a problem.

- A thing is read with `GET`, made with `POST`, which answers `201`, replaced with `PUT` and changed in part with `PATCH`. What changes the state of a thing and is no edit of it is a `POST` to a path of its own: `/api/orders/{id}/cancel`, `/api/plans/{id}/cancel`, `/api/plans/{id}/discard`.
- An id is a UUID in the path.
- Every error is `application/problem+json` (RFC 9457), a `Problem` of the domain crate: `type`, `title`, `status`, a `detail` in words, and `errors` with the fields that were refused, where it is known which.

| Status | |
|---|---|
| 400 | The request could not be read: broken JSON, a path or a query that does not parse. |
| 401 | No token, or one that is not valid. |
| 403 | A valid token whose role does not allow this. |
| 404 | No such thing, or none that the caller may know of. |
| 409 | The thing is not in a state for this: the date has a plan in progress, the order is planned already, the plan is not done. |
| 413, 415 | A body that is too large, or is not JSON. |
| 422 | The request was read, and something in it cannot be accepted. |
| 500 | The fault is the server's. |

An order that belongs to another client is a `404` to a client, with the very body an id that names nothing gets.

Why:

- A client has one shape of error to read: the page, and the bot, which shows a person the `detail`. actix-web answers a body it cannot read, or a path it does not serve, in plain text. So the API sets its own handlers for bodies, paths and queries, and its own answer for whatever matches no route.
- A `403` for somebody else's order would say that the order exists.
- The cause of a `500` may name a table, a constraint or a host. It is logged, with the id of the request (D42), and not sent.
- The path has no version in it. Nothing plans a second one, and whoever calls the API beside Swagger UI is in this repository.

Good to know: a path that is served, asked for with a method it does not take, is a `404` as well. actix-web tells routes apart by method before it knows that the path exists.

### D40. Passwords and tokens (in place)

- A password is stored as an Argon2id hash, with the parameters the `argon2` crate recommends: 19 MiB, two passes, one lane. Hashing and checking run on blocking threads, through `web::block`. The parameters are part of the stored hash, so they can be raised later without touching the accounts that are there.
- A login for an address that has no account is checked all the same, against the hash of a password nobody has. Then it is refused exactly as a wrong password is, with the same status and the same words.
- A login gives a JWT signed with HS256. It names the user (`sub`) and the role, and when it was issued and expires (`iat`, `exp`). It is good for `JWT_TTL_SECS`, an hour by default, and there is no leeway. The key is `JWT_SECRET`, at least 32 bytes long. The binary has no default for it.
- The role is in the token, and nothing is looked up to check a request.
- An account is opened as a client's. An admin makes operators and admins of them, and cannot change their own role.
- The first admin comes from `ADMIN_EMAIL` and `ADMIN_PASSWORD`, when the API starts and no account has that address. An account that is there is left as it is.

Why:

- Hashing is slow on purpose. On the threads that serve requests, a handful of logins at once would stall every other request.
- Without the decoy the time a login takes would say which addresses have accounts.
- With the role in the token a request costs no query. The price is that a changed role applies from the user's next login. Until the token they hold expires, an hour at most by default, they keep the old one. A lookup on every request would make that immediate, and would also make the role in the token pointless.
- There is no way to end a token early, and none to renew one. A token ends by expiring, and the user logs in again. For a demonstration that is enough, and it keeps the API free of state.
- An admin who could demote themselves could leave the system without an admin, and only the database could mend that.
- If the settings could change an existing account, a stale `ADMIN_PASSWORD` would reset a password at every start, and somebody who had registered the address first would be made an admin by it.

Only HS256 tokens are accepted, so the RSA advisory of D12 still does not apply.

A debug build takes about a third of a second over a hash. The tests use the smallest parameters there are, except one test that checks the real ones.

### D41. Who may call what is a table, and the tests hold the API to it (in place)

| | Client | Operator | Admin |
|---|---|---|---|
| Open an account, log in, health, the document, the page | without a token | | |
| Orders: place, list, read, withdraw | their own | all | all |
| The depot: read | yes | yes | yes |
| The depot: set | no | no | yes |
| Couriers | no | yes | yes |
| Plans: ask, list, read, watch, their routes, cancel, discard | no | yes | yes |
| The stats, read and watched | no | yes | yes |
| Users, roles, demo orders | no | no | yes |

A handler says who may call it by the type of its first argument: `AuthUser` for anybody with a token, `Staff` for an operator or an admin, `Admin`. actix-web takes the arguments of a handler in their order, so with this one first the gate is checked before the body is read.

One test has the table, written out by hand as a list of every endpoint with the least role it needs. It sends every request as nobody and as each role, and expects `401`, `403` or anything else, as the table says. Another test reads the endpoints out of the OpenAPI document and compares them with the list.

Why:

- The type in the signature is the gate. A handler cannot run for a caller its signature does not name, and what a handler needs is read off its first line.
- With the body first, somebody without a token would be told what is wrong with their JSON, which is none of their business, and the API would have parsed input from a stranger for nothing.
- The table is written by hand so that a gate changed by mistake does not change what is expected along with it. Reading the endpoints from the document closes the other gap: an endpoint that is added without a line in the table fails the test.

### D42. One id follows a request into the logs of the solvers (in place)

Every request has an id. A client may send its own as `X-Request-Id`. One of 1 to 64 letters, digits, dots, underscores and hyphens is kept, and otherwise the API makes one, a UUIDv7. The id

- is echoed on the response, a refusal included,
- is the `request_id` of the span everything about the request is logged in, together with the method, the path, the user once the token has been read, and the status,
- and, when the request asks for a plan, is stored on the plan's job by `plans::submit`. The route jobs inherit it (D36), and a solver logs each job in a span that has it under the same name (D33).

So one id is found in the log of the API and of every solver that worked on the plan. In a run with two solver containers it was on the request for the plan, on the `plan` job and on all four route jobs.

Why the span is built here: `tracing-actix-web` gives every request an id of its own making and cannot take one from a header. Its middleware is kept, with a `RootSpanBuilder` of ours, `RequestSpan`. A small middleware in front of it decides the id, so that the id is there when the span is made, and puts it on the response.

Why an id from outside is checked: it goes into a log line, a response header and a column. One with a line break or a quote in it could forge a log line.

The middleware logs one line when a request has been handled, with the status. What a handler refuses is logged as a warning with the reason, and what fails on the server as an error with its cause, both by `tracing-actix-web` and both inside the span.

### D43. The OpenAPI document comes from the types and the handlers (in place)

- The types the API sends and receives have their schema derived by utoipa, in the domain crate, behind its `openapi` feature. Each of them has a `cfg_attr` for it.
- The bodies that are not entities, such as a login or the request for a plan, are in the domain crate too, in `dto`. The bot uses them without either feature.
- Every handler has a `#[utoipa::path]`, and its name is the id of the operation. The routes are registered through `utoipa-actix-web`, which collects what they say about themselves. So what is served is what is documented.
- The document is committed, as `crates/routeforge-api/openapi.json`, and a test compares what the API serves with it. `UPDATE_OPENAPI=1` rewrites the file.
- Swagger UI is served at `/docs/` from files inside the binary. The `vendored` feature of `utoipa-swagger-ui` carries them in a crate, so neither the build nor the page fetches anything (D8).

Why:

- D19 kept sqlx out of the domain types, and with it a `cfg_attr` on every one of them. Here the attribute is the lesser cost. A schema has to describe the very type that serde writes, and a second type that mirrors it for the document could drift from it unnoticed. utoipa also needs nothing of a type but its shape, where sqlx would have wanted the types flat.
- The bot is to send what the API reads and read what it sends. Shared types make that a matter of compiling.
- The document is the API's contract with the page and the bot. Committed, a change to it shows up in review, including one nobody meant, such as a field renamed in passing.

Three traps, each of which gave a document that was wrong and looked right:

- `text_enum!` has to take the text of a value as a `tt`. As a `literal` it is handed on wrapped in an invisible group, which serde looks into and utoipa does not, and the document named the values `Queued` and `Routing`.
- A handler takes an id as `web::Path<Uuid>`. utoipa reads `Path<OrderId>` as a struct of parameters.
- A query that is a struct, such as `OrderFilter`, has to be named in `params(...)` as well as being an argument. Without that utoipa leaves its parameters out and says nothing.

The words of the document are the doc comments of the types and the handlers. So some of them cite decisions of this file by number.

### D44. A list is read in pages, by the id of the last item (in place)

`GET /api/orders` takes `limit` and `after`. It answers with at most `limit` orders, oldest first, that come after the order with the id `after`. A client reads on by passing the id of the last order it got. `limit` is 100 unless said otherwise and 1000 at most.

`OrderFilter` is both the query string of the endpoint and the filter of `orders::list`. Without a `limit` the repository returns everything, which is what the solver and the tests want.

Why:

- Ids are UUIDv7 (D11), so their order is the order of creation, and the primary key serves the query.
- A page asked for by position shifts when an order is placed meanwhile, and one order is then seen twice or not at all. A page asked for by id does not.
- The answer is a plain array. A page with fewer orders than `limit` is the last.

The couriers and the users are listed whole. There are a handful of each.

### D45. The API gives a plan its budget and its seed when it names none (in place)

- A request for a plan may leave out the budget (D31). It then gets `API_BUDGET_DEFAULT_MS`, ten seconds a route by default.
- A budget that is given may be at most `API_BUDGET_MAX_MS` of time, five minutes by default, and at most `API_BUDGET_MAX_ITERATIONS` moves, ten thousand million by default. Each limit holds on its own. More is a `422`.
- A request may leave out the seed. It then gets one from the random bytes of a UUID, and the plan says which, so the plan can be run again with it.

Why:

- A route job holds a worker for the whole of its budget (D18). A budget typed with three noughts too many would hold every worker for days.
- The limits are settings and not constants, because how long a number of moves takes depends on the machine the solvers run on.
- A plan that asked for no seed should still be repeatable.

### D46. Demo orders come from the API, from a seed (in place)

`POST /api/demo/orders`, for admins, stores up to 2000 pending orders for a date, spread evenly over a disc round the depot. Its radius is 5 km by default and 30 km at most. About one in five is a pickup, and each has a demand of 1 to 3. They belong to the admin who asked. They are stored in one transaction.

Every random choice comes from a ChaCha8 generator seeded with the request's seed, as in `routeforge-tsp` (D5). The same seed gives the same orders round the same depot.

Why:

- A demonstration needs a few hundred orders, and a tool to make them would be a sixth crate. In the API it is one more endpoint.
- With a seed, the scaling run of Phase 6 plans the same day with one solver and with four, and anybody can repeat a demonstration.
- The distance from the depot is the radius times the square root of a random number. Without the root the orders would crowd round the depot, since a ring's share of the disc grows with its radius.
- Beyond some 30 km the flat map the plans are clustered on is no longer a good one (D29).

### D47. The API ends at once when it cannot start (in place)

When it starts, the API connects to the database, applies the migrations (D32), makes the first admin if one is asked for (D40), and only then listens. If the database does not answer, or a setting is wrong, the process ends with the reason.

Why: a solver waits for its database for as long as it takes (D37), because a worker that waits harms nobody. An API that listened without a database would accept connections and answer every one with an error. A process that ends says more, and whatever started it can start it again. In compose that is `depends_on` with the health of the database (D54).

Good to know, found in Phase 6: "does not answer" takes five seconds. sqlx tries a connection that is refused again and again until the pool's acquire timeout has passed, and only then does the start fail, with "pool timed out while waiting for an open connection". A database that is a moment late is therefore waited for, and one that is not there costs five seconds.

While it migrates, sqlx holds a lock on the database. Two APIs that start together apply each migration once.

The pool has ten connections for requests, and one more that only listens (D50). A request that gets none within five seconds is answered `500`.

How the API stops is D51.

### D48. A change to a plan is announced where it is made (in place)

Beside `routeforge_jobs`, which wakes the workers (D26), the queue notifies two channels for whoever shows a plan to a person:

- **`routeforge_plans`** says that a plan has changed, or one of its jobs has. Its payload is the plan's id. Every function of `db::queue` that changes either notifies it: `enqueue`, `claim`, the commit of a completion, `fail`, `release`, and what ends a plan or discards it. It does so in the statement of the change, or in its transaction.
- **`routeforge_progress`** says how far the search for a route has got (D49). Its payload is a `RouteProgress` as JSON, sent by `queue::announce_progress` and stored nowhere.

What has changed is not said. Whoever hears of a plan reads it again.

Why:

- A notification is delivered when its transaction commits. Sent with the change, it is heard when the change can be read, and never for one that was rolled back.
- With nothing but the id in it, the notifications of one transaction are equal, and PostgreSQL folds equal ones into one. A `plan` job that completes enqueues a route job for each courier, moves its plan on and ends itself, and all of that is one notification. The id is turned into text by PostgreSQL in every statement that notifies, so the payloads are equal to the letter.
- A hint cannot be out of step. The read gives the plan as it is, however many hints came or were lost, and nothing has to fit into a payload, which PostgreSQL limits to 7999 bytes.
- The workers keep a channel to themselves because every worker wakes for what is said on it. On theirs, each claim and each stored route would send all of them to look at a queue they have just looked at.
- Every change goes through one module, so there is one place to keep right. `plans::start_routing` and `plans::route_finished` announce nothing themselves: they change a plan only inside a completion, and its commit announces it.

A claim is still one statement (D22). Its update moved into a CTE so that the statement can notify as well.

Tests: a watcher hears of every change once, at the commit and not before, and nothing of a rollback or a heartbeat; twenty route jobs, their plan and its job are one notification; a plan that fails, a job buried by an idle worker and a discard are heard; and none of it reaches the workers' channel.

### D49. A search hands out its best route, and its worker announces it (in place)

Decided with the project owner on 2026-10-11: a route is to be seen improving on the map while it is searched for, and not only by a number.

- **In `routeforge-tsp`** the progress callback of a `Control` is handed the best tour beside the two numbers. It looks on and nothing more, so D5 holds: a search takes the same course with a callback as without one. A search reports once more as it returns, so its last report is of the tour it returns.
- **In the solver** a report carries the route as the positions of its stops in the job's list of orders. The list is made anew only when the route got shorter, which is the only way the best route changes. A test in `routeforge-tsp` holds the search to that.
- **Announcing.** Beside the search and the heartbeat a third future is polled, the announcer (D34). Whenever the search has said something new, and at most every `SOLVER_PROGRESS_MS`, 250 by default, it sends the latest report through `queue::announce_progress`.
- **The announcement** names the plan, the job and the attempt, and carries the moves, the best length and the route. The route is left out when the payload would reach the 8000 bytes at which PostgreSQL refuses a notification, which is beyond some 1780 stops. The numbers are sent all the same.

Why:

- The search is where the route is. Without the tour in the callback a route could only be shown once it was stored, and the map would stand still for the whole of the budget.
- Positions and not the ids of the orders: a position is a number of up to four digits where an id is 36 characters. A route of 300 stops is 1.1 kB as positions, and as ids it would not fit.
- Nothing stores what is announced. The next announcement replaces it a quarter of a second later, and writing it to the job would be a new version of the row four times a second for every route. What a job has stored is what its heartbeat says, as before (D34). A page that is opened in the middle of a search shows that until the next announcement comes.
- The attempt is in it because an announcement is not fenced (D6). A worker that has lost its job goes on announcing until its next heartbeat tells it so, and a watcher that knows the attempt of the job can tell the two apart.
- An announcement that does not get through is logged at `debug` and forgotten. Nobody waits for it, and the next one says more.

A worker can now make three calls at one moment: its heartbeat, an announcement, and marking itself as seen. So a solver process takes one connection that listens and three for each worker.

What the callback costs: nothing that can be measured. The benchmarks of the old code and the new were run back to back, twice. 2-opt at 50 stops took 11.97 and 12.25 µs before and 12.15 and 12.18 after. Annealing 100 000 moves at 200 stops took 1.98 and 2.00 ms before and 2.01 and 1.99 after.

What there is to see, measured with four solvers in containers on the development machine:

- Routes of about a hundred stops, as 300 demo orders give, are at the length they end with by the second announcement, a quarter of a second in. From the first tour to a local optimum takes the search a few milliseconds, and annealing then finds little or nothing. The line appears, jumps once, and stands.
- Routes of about 350 stops, from 1200 orders, went from some 85 km to some 68 km in that first quarter of a second, and then lost between half a percent and two and a half percent more over ten seconds, a few edges at a time.

So for a line that is seen to move, a day has to give its routes several hundred stops.

### D50. One connection listens, and the event streams read (in place)

The API has one connection that listens on the two channels of D48. What it hears goes into a `tokio::sync::broadcast` channel, and every open stream holds a receiver of it. Two endpoints stream, as `text/event-stream`:

| Endpoint | Events |
|---|---|
| `GET /api/plans/{id}/events` | `plan`: the plan. `jobs`: its route jobs, each a `RouteJob`. `progress`: a `RouteProgress`. |
| `GET /api/system/stats/events` | `stats`: what `GET /api/system/stats` gives. |

A stream opens with everything it shows. After that:

- It reads again when it is given a reason: a notification for its plan, or for any plan in the case of the stats. It sends what differs from what it last sent, and nothing if nothing does.
- It reads at most every 200 ms, so a burst of changes is one read.
- It reads without a reason too, every ten seconds for a plan and every two for the stats, as a worker polls (D33). A notification can be lost, and a worker comes and falls silent without one (D27).
- When the listener has had to reconnect, and when a stream has fallen more than 1024 signals behind, the stream is told that it may have missed something, and reads.
- Progress is passed on as it comes, with no read.
- A comment line every 15 seconds keeps a quiet stream from being taken for a dead one.

A stream ends when its reader has gone, when the token it was opened with expires, when the API is told to stop (D51), and when a read fails.

Why:

- One connection for all streams: what is announced means the same to every stream, and a connection for each would let a dozen open pages take the whole pool.
- The stream reads, and the notification only says when. So a stream can be opened at any moment and shows the plan as it is, a lost notification costs ten seconds at the worst, and nothing has to be replayed. A page whose stream broke opens it again and is up to date.
- A token is checked when a request arrives (D40), and a stream answers for as long as it is read. Ended with its token, a stream gives nobody more than the token did. A token is accepted through the second it expires in, so the stream ends as the next second begins: whoever opens it again is answered `401`, and the page shows the login.
- No task stands behind a stream. It is a value that the response owns, made with `futures_util::stream::unfold` and polled when the server wants the next piece. When the response is dropped, because the reader has gone or the server stops, the stream and its receiver go with it. This is the rule of D34 kept in the API: nothing has to be stopped, so nothing can be left running. It also bounds what a slow reader costs. A stream that is not read is not polled, takes nothing from the channel, falls behind, and reads again when it is read.
- The first read is made before the response starts. So a plan that does not exist is a `404` with a problem for a body, and not a stream that breaks off.
- Server-sent events are written by hand: an event is two lines of text. `actix-web-lab` has a type for them and would bring a crate of experiments with it. `futures-util`, for `unfold`, is the one direct dependency this adds, and actix-web has it in the build anyway.
- The page reads a stream with `fetch`. `EventSource` cannot send an `Authorization` header, and a token in the query string would end up in logs.

Good to know:

- The line that says a request was handled is logged when its response starts (D42). A stream logs a second line when it ends, in the span of its request, with the reason and how long it was open.
- The schemas of `RouteJob` and `RouteProgress` are named for the document by hand. No response has them as its body, so utoipa would not find them (D43).
- The stats hold when each worker was last seen, which changes with every heartbeat of a worker. So with a solver running, the stats stream sends about every two seconds.
- A stream reads its plan and then the plan's jobs, one after the other and not in one snapshot. A job can finish between the two, so for a moment a stream may show a job as done that its plan has not counted yet. The next read, a fifth of a second later, puts that right.

Tests: the hub passes on what the database announces, passes over what it cannot read, and says that it may have missed something after its connection was ended under it; a plan's stream opens with the plan and its jobs and follows a stand-in for the solver from queued to done to discarded; it passes on the searches of its own plan and of no other; it reads again when it may have missed something and when it fell behind, and now and then without being told; it sends nothing twice; it ends with its token, to the second, and when the hub is closed; and the stats stream does the same for the queue.

### D51. The API stops at once with streams open (in place)

Told to stop, actix-web lets the requests it has finish first, and an event stream never finishes. So the API listens for Ctrl-C and SIGTERM itself, as the solver does, and hands the server that as its `shutdown_signal`. Before the server hears of it the hub is closed, which ends every stream. The requests in hand then get eight seconds.

Why: `docker stop` sends SIGTERM and kills after ten seconds. Without this the API would be killed every time a page was open, in the middle of whatever else it was doing.

It also makes Ctrl-C a graceful stop. Left to itself, actix-server stops at once on an interrupt and gracefully only on SIGTERM.

Tried on the development machine with a page open and both its streams running: the API had stopped a third of a second after Ctrl-C. Its log said that it was told to stop, that each stream ended because the API was stopping, and that it had stopped.

Tried the other way as well, in a build that left the hub open: the API took 8.4 seconds to stop, which is all the time it gives the requests in hand.

Not looked into: in those eight seconds the stream that was still open could not read. After five seconds the pool refused it a connection. By then the workers with nothing to do had stopped, and the connections of the pool had been opened on their threads, which may be why. If it is, a request that is still running when the API is told to stop can fail for want of a connection. With the streams ended first nothing is left running that long, but a slow request could be.

### D52. The page is one file, inside the binary (in place)

`web/index.html` is the whole page: its style, its script, and no build step. It loads Leaflet from `web/vendor/leaflet`. Both are compiled into the API with `include_bytes!`, through a table in `page.rs`, and served at `/` and under `/vendor/`.

- **Inside the binary**, as Swagger UI is (D43). The API is one file wherever it runs, the image of Phase 6 needs nothing copied beside it, and what is served is what was built. The price is a rebuild to see a change to the page. The files are sent with `Cache-Control: no-cache`.
- **The table** is written out, a row for each file. A test walks `web/` and fails for a file that is not in it.
- **Leaflet 1.9.4**, the release npm tags `latest`; 2.0 is an alpha. It was taken from the registry's tarball, which was checked against the integrity the registry gives for it, and the SHA-256 of every file is in the commit that added them. `.gitattributes` keeps `web/vendor` byte for byte: `leaflet.css` and `LICENSE` come with CRLF line ends, which git would otherwise turn into LF.
- **Nothing from another host but map tiles** (D8). A test reads the page and fails if it names any other address. The tiles are OpenStreetMap's and a switch turns them off. Without them the map is paper with a grid, and everything else is drawn on it.
- **The login** is kept in `sessionStorage`, so it survives a reload and ends with the tab. Any `401` clears it and shows the login.
- **Text from the API goes into the page as text**, never as HTML. An address or the name of a courier is typed by somebody.
- **On the map** the stops are dots on a canvas and the routes are lines in SVG above it. Leaflet draws a canvas in the order things were added to it. On one canvas a route would be hidden under dots that were drawn again after it, as they are when a plan takes its orders.
- **A client** who logs in sees the depot and their own orders, and no planning. That is what the API lets a client read (D41).

What the page can do beyond the list in the plan was decided by the project owner on 2026-10-11:

- **Discard** a plan that is done, beside Cancel.
- **Budget and seed** for a plan: seconds or millions of moves for each route, and a seed. Left empty, the API gives its own (D45).
- **Set up a demo city**, for an admin, on a database with no depot or no courier: a depot in Kyiv and four vans, through the endpoints an admin would use by hand. The vans are sized for the number of orders in the field beside the button. A demo order has a demand of 1 to 3 (D46), so a day asks for twice its number, and each van takes a quarter of that and a tenth more. So the day that is generated next gives every van a route, whether it has 300 orders or 2000.

What the page does not do: it draws every route from the depot as it is now, so a plan made for an earlier depot (D21) would be drawn from the wrong place; and it learns of an order that somebody else placed only when Refresh is pressed, since nothing announces an order.

The page has no tests of its own. The API's tests check that it is served and what it loads. What it does was tried in a browser: `docs/plan.md` says how.

### D53. The plans of a day are listed whole (in place)

`GET /api/plans?service_date=` gives the plans of one day, newest first. The day has to be said.

Why:

- The page has to find the plan of the day it shows. Whoever asked for a plan has its id from the answer, and nobody else has it.
- A day has one plan in progress at most, and beside it the ones that were done, discarded, cancelled or failed before. That is a handful, so the list is not paged (D44).
- Without a day the list would be every plan there ever was.

There is no index on `plans.service_date`. A table of some hundreds of plans is read whole in no time. Should there ever be many thousands, the index is one migration.

### D54. The stack is one command, and needs no settings (in place)

`docker compose up --build --scale solver=4`, on a clone that has no `.env`, brings up four services:

| Service | Waits for | Health check |
|---|---|---|
| `db` | nothing | `pg_isready`, over TCP |
| `api` | `db` to be healthy | the binary asks its own `/readyz` (D55) |
| `solver` | `db` to be healthy | none |
| `bot` | `api` to be healthy | none |

- **Every setting has a value to fall back on.** For most that is the default of the binary. Three have none there, and fall back to values written into `docker-compose.yml`: the key that signs the tokens, and the address and password of the first admin, `admin@example.com` and `routeforge`. Decided by the project owner on 2026-10-11. The binary still has no default for the key (D40).
- **The ports are open to this machine alone.** The API is published on `127.0.0.1:8080`, as the database is on `127.0.0.1:5432`.
- **No start order beyond the table.** A solver waits for the database, and for the schema by itself (D32), so the API and the solvers start side by side.
- **No restart policy.** A service that ends stays ended.
- **The database's health is asked over TCP**, with `pg_isready -h 127.0.0.1`.

Why:

- The claim is "from a fresh clone". A step before the command, a file to copy or a key to think of, is a step that can be got wrong by whoever is looking at the project for the first time.
- Values that are in the repository protect nothing, so nothing may depend on them: the ports are on the loopback. Whoever opens them to a network sets their own values in `.env`, and `.env.example` says so beside each.
- The other way to get a key was to make a random one at each start. Every login would then end with every restart, and the admin's password would still be one that everybody can read.
- A service that ends has said why (D47), and one that is started again by itself says it over and over. `docker compose up` once more is the restart.
- Nothing waits for a solver, and which solvers are alive is what the registry says (D27) and the page shows. A check of its own would be a query every few seconds from every instance, to tell Docker what the system knows already.

Why TCP for the database: the postgres image sets a new database up with a server that listens on the socket alone, and only then starts the one that listens on the network. Asked over the socket, `pg_isready` says yes to the first. Measured on the development machine, on a new volume: the socket said yes and the port was closed for 0.14 seconds, and for 0.16 seconds more nothing listened at all.

What that costs is less than it sounds, and was measured too. Ten starts on a new volume went well with the old check, and ten with the new one. An API that is let go inside that window finds the port closed, and sqlx tries again for five seconds (D47), by which time the database is there. It bites when setting the database up takes longer than that after the first yes. With a script that keeps the set-up going for twelve seconds, as a slow disk or a script that loads data would, the old check let the API start, and it ended with "pool timed out while waiting for an open connection", and `docker compose up` with "dependency failed to start". With the new check the same stack was up after twenty seconds. So the check is right now, and no longer depends on a retry that is not ours.

A test in the API holds the compose file to this. It reads the `api` service out of `docker-compose.yml`, takes every `${VARIABLE:-fallback}` for its fallback, and starts the API's settings from what it finds: the key is long enough, the admin is an account the API would make, the API listens on every address of its container, and the port is published to `127.0.0.1` alone. A variable without a fallback fails it.

Good to know:

- **About twenty solvers.** A solver holds up to four connections to the database (D49) and the API eleven (D47), and PostgreSQL gives out a hundred. The demo scripts refuse to start more than twenty (D57).
- **`--build` needs the network, every time.** A build asks the registry for the Dockerfile frontend its first line names and for its base images, even when every layer is cached, and one that has dependencies to compile asks crates.io as well. Seen twice while the phase was tried: for a moment a name could not be resolved inside Docker, and `up --build` failed, the first time with nothing to build at all. The next attempt went through both times. `docker compose up` without `--build` starts the images that are there.
- **A build from nothing** took a minute and a half on the development machine: every dependency and the three services, with the base images there already.
- **On Windows, from PowerShell.** Compose 5 refuses to build when the directory it is started in is spelled with a lower-case drive letter, which is how `cmd` and Git Bash have it in a terminal of VS Code. Nothing in the repository can change that. `docs/plan.md` has the message and three ways out among its traps.
- **A second clone is the same project.** The compose file names the project `routeforge`, so two copies of the repository on one machine share their containers and their volume. `docker compose -p another-name`, or `COMPOSE_PROJECT_NAME`, keeps them apart, together with other ports in `POSTGRES_PORT` and `API_PORT`.
- **Timings**, on the development machine: the whole stack with four solvers was up 9 seconds after `docker compose up` on a new volume, most of which is waiting for the database's health check and then for the API's. `docker compose down` took 2.8 seconds.

### D55. The API answers for its own health (in place)

`routeforge-api healthcheck` serves nothing. It asks the API that listens on `API_BIND` for `/readyz`, and ends with success if the answer is `200` and with a reason on standard error if it is anything else. That is the health check of the `api` service: `test: ["CMD", "routeforge-api", "healthcheck"]`, every two seconds.

- It is written with the standard library alone: a `TcpStream`, a request of four lines with `Connection: close`, and the first line of the answer.
- An address that means every address, as `0.0.0.0:8080` in the container does, is asked on the loopback.
- Connecting, sending and the answer may take two seconds each. Docker gives a check three, and ends one that takes longer.
- A health check that was answered with success is logged at `debug`, and every other request at `info` as before (D42).

Why:

- The runtime image holds the binary and the C library (D38). `curl` would be a package and its libraries, OpenSSL among them, for one request. A line of shell that writes to `/dev/tcp` would tie the check to bash being in the image, and could not be tested.
- `/readyz` and not `/healthz`: what waits for the API to be healthy, the bot and whoever runs `docker compose up --wait`, wants an API that can work. That is one with a database that answers and has the schema.
- A check every two seconds is 43 200 lines a day that say nothing. A check that fails is worth a line: it says that the stack is not well, and why.
- Every two seconds for good, and not often while starting and seldom afterwards: that is `start_interval`, which needs Docker Engine 25. One query every two seconds is nothing, and the check then reads the same as the database's.

`/healthz` counts as a health check for the log as well, although nothing in the stack asks it.

Tests: the probe against a stand-in that answers `200`, `503`, `404`, a line that is no HTTP, nothing, and against a port nobody listens on; the probe against the application itself over a socket, refused while the database has no schema and passed once it has; what the log holds after a check that went well, one that did not, and a request that is neither; and the binary itself, run with `healthcheck` against a stand-in, and with an argument it does not know.

The last two run the binary because nothing else comes in through `main`. Before they were there, two faults passed every test: the `healthcheck` argument not looked at, and an unknown argument taken for none. Either way the binary serves where it should ask, and the stack is never healthy.

### D56. The bot holds its place in the stack before it is built (in place)

Decided by the project owner on 2026-10-11.

Phase 6 gives the bot its target in the Dockerfile and its service in compose. The binary is still the stub of Phase 0, changed in one thing: it no longer prints its name and exits. It says that the bot is built in Phase 7, and waits to be told to stop.

Why:

- The description counts the bot among what one `docker compose up` brings, and Phase 6 is where that command is made.
- The image is where a dependency that needs a native library fails (D12). teloxide, and reqwest with rustls and ring, were compiled in the image for the first time here, and not in the week Phase 7 is due.
- What the stub does is what the bot does without its tokens (D8). Phase 7 puts a bot where the waiting is.

A service is the first process of its container, and the first process is not ended by a signal it does not listen for. So the stub listens for SIGTERM, as the solver and the API do (D35, D51). The whole stack, seven containers with four solvers, is down 2.8 seconds after `docker compose down`. A stub that slept would have made that ten.

What Phase 6 does not give it: tokens, settings, the logging of the other two services, or a line of teloxide.

### D57. The demonstration is a script (in place)

`scripts/demo.ps1` and `scripts/demo.sh` show the two things the project claims about its workers, on the stack as compose runs it:

- **Scaling.** The same day is planned with one solver and then with four, with the same seed and the same budget of moves, and discarded in between (D30).
- **Failure.** A plan with a budget of time. Two seconds into its searches a solver that holds a route is killed with `docker kill`. The script waits for another worker to hold that route's job, and for the plan to be done.

How:

- **Through the API alone.** The scripts log in, ask for plans, read them and the stats. Nothing is read from the database behind the API's back.
- **Where the API is, and who the admin is, they ask the stack:** `docker compose port api 8080`, and the environment of the api container. So a `.env` with other values needs no telling.
- **What a day needs is made if it is missing:** a depot and four vans, as the page's demo city (D52), and orders. The orders are as many as the couriers carry with a tenth to spare, the page's sum turned round. Then every courier gets a route, and every solver has one to search for.
- **The day is tomorrow.** The page opens on today, and on its day a script cancels a plan in progress and discards a done one.
- **The time of a plan is the plan's own:** `finished_at` less `created_at`, both from the database's clock (D6). How often the script looks does not enter into it.
- **The routes of every scaling run are compared with those of the first:** whose route it is, how long, and which stops in which order. Other routes end the script as a failure, because the same seed and the same moves give the same routes (D5).
- **The solver to kill is found in the stats.** A worker registers under its host name (D27), which under Docker is the id of its container, as much of it as `docker kill` needs.
- **After each run the script checks what the system is judged by** (D20): every order the plan took is on exactly one route.
- **Each plan is asked for with a request id of the script's making,** which it prints. `docker compose logs api solver` has it in the lines of the API and of every solver that worked on the plan (D42).

Measured on the development machine, with 16 logical processors and Docker Desktop on WSL2. A new database, 300 orders, four vans, 200 million moves a route:

| Solvers | Seconds | Speed-up | Routes | Kilometres |
|---|---|---|---|---|
| 1 | 15.2 | 1.0 | 4 | 123.60 |
| 2 | 8.1 | 1.9 | 4 | 123.60 |
| 4 | 4.5 | 3.4 | 4 | 123.60 |
| 8 | 4.6 | 3.3 | 4 | 123.60 |

The routes were the same in every run, and a second round gave 15.1, 7.9, 4.5 and 4.4 seconds.

- Four solvers are 3.4 times as fast as one and not four times, because the routes are not equally long: 87, 86, 78 and 49 stops. A plan is done when its longest search is. One solver makes the four searches in turn, and four make them side by side.
- Eight solvers are no faster than four. The day has four routes.
- The same run made while the machine was still clearing away other containers took 28.6 seconds with one solver. The numbers want a machine that is doing nothing else.

The failure run, twice, with four solvers and ten seconds a route: the route was taken over 27.8 and 28.3 seconds after the kill, and the plan was done 40.2 and 41.0 seconds after it was asked for, with 300 stops on four routes and every order on exactly one. The solver that took the route over logged the job with `attempt=2`. A lease runs for thirty seconds from the last heartbeat (D33), which came up to five seconds before the kill, and a worker with nothing to do looks at the queue every five. `SOLVER_LEASE_SECS` and `SOLVER_HEARTBEAT_SECS` set in the shell make it quicker: compose passes them on, and the scripts change no setting themselves.

Why:

- A script and not a test: it needs Docker, takes a minute and a half, and kills a container. What it shows is in the tests already, in small: three workers drain a plan, and a worker that is aborted is replaced when its lease has run out (D34). The script shows it on the real thing, to a person.
- A budget of moves for the scaling run: with a budget of time every route takes what it is given, so the speed-up would be arithmetic. Moves are work, and the same work gives the same routes, which is what lets the script say that four solvers found what one found.
- A budget of time for the failure run: how long a search goes on is then known, and two seconds in is the middle of it on any machine.
- Two scripts, because the project is developed on Windows and runs anywhere. The PowerShell one runs in Windows PowerShell 5.1 and in PowerShell 7. The other is POSIX sh, and needs `curl` and `jq`: reading JSON with `sed` would have been a second program. It was run under dash, on Ubuntu, with jq 1.8.1, and printed what the PowerShell one prints.

Two things the scripts guard against, both met while they were written:

- **How many solvers.** From inside PowerShell, `-Solvers 1,2,4,8` is a list. Started any other way, as `pwsh -File scripts/demo.ps1 -Solvers 1,2,4,8`, the script is handed one text, and PowerShell reads that text as the number 1248. Compose was asked for 1248 solvers, and had created every one of those containers and started some four hundred before it was stopped. The counts are now taken as text and read by the script, and anything but 1 to 20 is refused before compose is asked (D54).
- **No `--build`.** The scripts start the stack with `docker compose up`, which builds an image that is missing and leaves one that is there. With `--build` they would need the registry every time (D54), and the demo is to work without a network (D8).

### D58. The bot takes buttons and commands, and free text waits for a key (planned)

Decided by the project owner on 2026-10-12.

The bot of Phase 7 asks no model anything. A person shares a location, and the bot asks with buttons for what an order needs beside it: a delivery or a pickup, how many parcels, and which day. It shows what it is about to place, and places it when that is confirmed (D9). Whatever else a client or an operator may do is a command.

The approved description calls the bot LLM-powered and names the Claude API. That part is not given up. It is below the cut line: free text is an optional item of Phase 8, built only when there is an Anthropic API key to try it with. `docs/description.md` stays as it was approved, and until the item is built the README says that the bot takes no free text.

Why:

- The Claude API wants a key from an account of its own, with credit on it, paid for by the token. A Claude subscription does not include one. Phase 7 does not wait for it.
- An order says four things: where, which kind, how much and which day. A location and three buttons say them, the same way every time, which is what a presentation wants.
- The bot then leans on no service but Telegram that can be slow, down or out of credit on the day. Whoever clones the repository needs one token, from @BotFather, to see all of it.
- Nothing is built twice. What a button does and what a tool of a model would do is the same request to the API, so free text goes on top of the bot of Phase 7.
- Code for the Claude API that was never run against the Claude API is not shipped. A scripted model shows that the loop is right, and not that the requests are what the API takes. So that part is not written ahead of a key.

What it costs: "deliver two parcels here tomorrow" is a few taps and not a sentence, and for as long as the item of Phase 8 is not built the bot is less than the description says of it.
