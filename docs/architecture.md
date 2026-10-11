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

A plan that is cancelled, or that loses a job for good, undoes itself: its routes are deleted and its orders are pending again (D20).

## Crates

The workspace has exactly five crates, as the [approved description](description.md) requires.

| Crate | Role | Workspace crates it may use |
|---|---|---|
| `routeforge-tsp` | Clustering and route optimization. Pure computation. | none |
| `routeforge-domain` | Domain types, DTOs and geo helpers. With the `db` feature: persistence and the job queue. | none |
| `routeforge-api` | HTTP API, auth, OpenAPI, the map page. | `domain` with `db` |
| `routeforge-solver` | Queue worker. | `domain` with `db`, `tsp` |
| `routeforge-bot` | Telegram bot. | `domain` without `db` |

## Decisions

### D1. One order is one stop, relative to a depot (in place)

An order is either a delivery from the depot to a client or a pickup from a client back to the depot. Couriers start and end at the depot.

Why: this is the capacitated vehicle routing problem, which the approved algorithms solve directly. The alternative, point-to-point orders with their own pickup and drop-off address, needs "pickup before drop-off" ordering that sweep clustering and plain 2-opt do not respect.

The schema has had it since Phase 2: an order has one kind, `delivery` or `pickup`, and one position.

### D2. A plan is one `plan` job that fans out into `route` jobs (in place in the queue; the solver that fans out is Phase 3)

Why: optimizing one courier's route is the expensive part, and the routes of a plan are independent of each other. With one job per route, adding solver instances shortens a plan. With one job per whole plan, extra instances would help only when several plans were queued.

Cost: the plan must be marked done exactly once when its last route job finishes, including when two finish at the same moment. A counter on the plan row, incremented in each completing transaction, serializes them.

The counter is `plans::route_finished`. Two tests complete route jobs at the same moment, once with two transactions that are made to overlap and once with eight at a time. In both, exactly one completion sees the plan become done.

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

### D8. The demo works without the internet (planned)

Distances default to Haversine. The Leaflet library is vendored into the repository. Map tiles are optional: without them the routes still draw. The bot idles when its tokens are missing.

### D9. The bot has no authority of its own (planned)

The bot calls the api with a short-lived token of the Telegram user it is talking to. Actions with side effects are confirmed with a button, and the confirmation is handled in code.

Why: whatever the language model decides, the api applies that user's role, so the model cannot do more than the user could. The confirmation step keeps a misread message from creating or cancelling anything.

### D10. Rust 1.99.0, pinned (in place)

`rust-toolchain.toml` pins the compiler for developers, CI and the Docker image.

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

Consequence for Phase 3: a route job with four or more stops takes the whole time the solver gives it. The time budget per route is therefore what sets the length of a plan.

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

### D27. The worker registry is not part of the queue (in place)

`workers` has a row per solver process: its id, its host name, when it started and when it was last seen. `jobs.claimed_by` holds a worker's id and is not a foreign key to that table.

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

### D30. A plan that is done can be discarded, and its date planned again (planned)

Decided by the project owner on 2026-10-11. Phase 4 builds it.

Discarding undoes a plan that is done, the way a plan that fails or is cancelled undoes itself (D20): its routes are deleted and its orders are pending again. The plan row stays, with the status `discarded`. The operator then plans the date again, and the new plan takes every pending order of the date. Only a plan that is done can be discarded. One that is still in progress is cancelled instead.

A date whose plan is not discarded is planned as before: a later plan takes the orders that are still pending.

Why:

- Without it a plan could never be corrected. After a courier is added, or after a run that was given too little time, the date could not be planned afresh, and an order that a plan holds could never be withdrawn.
- The verification steps in `docs/plan.md` plan the same orders twice: a second time with the same seed, and once with one solver and once with four. The second run needs the orders back.
- Keeping the old routes until the new plan is done would leave no moment without routes. It would also put an order on two routes at once, an old one and a new one, which is what the unique constraint of D20 rules out. The price of a discard is that the date has no routes until the new plan is done.
- Discarding and planning stay two steps, so each does one thing. A discard made by mistake costs the routes and nothing else: the orders are all there, pending.

What it needs:

- A `discarded` value for `plans.status`. The two `CHECK`s that list the statuses, `plans_status_check` and `plans_finished_check`, are in a migration that is merged, so this is a new migration.
- `queue::discard_plan` beside `queue::cancel_plan`. The undo exists as `end_plan` in `db::queue`, where it is limited to plans that are queued or routing. Like every operation on a plan, it locks the plan row first (D24).
