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
2. A solver claims the `plan` job and groups the date's orders into one cluster per courier. In the transaction that completes that job, it inserts one `route` job per cluster.
3. Solvers claim the `route` jobs. Each one optimizes the stop order for a single courier and writes the route in the transaction that completes its job.
4. When the last `route` job completes, the plan is marked done.

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

### D1. One order is one stop, relative to a depot (planned)

An order is either a delivery from the depot to a client or a pickup from a client back to the depot. Couriers start and end at the depot.

Why: this is the capacitated vehicle routing problem, which the approved algorithms solve directly. The alternative, point-to-point orders with their own pickup and drop-off address, needs "pickup before drop-off" ordering that sweep clustering and plain 2-opt do not respect.

### D2. A plan is one `plan` job that fans out into `route` jobs (planned)

Why: optimizing one courier's route is the expensive part, and the routes of a plan are independent of each other. With one job per route, adding solver instances shortens a plan. With one job per whole plan, extra instances would help only when several plans were queued.

Cost: the plan must be marked done exactly once when its last route job finishes, including when two finish at the same moment. A counter on the plan row, incremented in each completing transaction, serializes them.

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

### D6. Jobs are delivered at least once; completion is fenced (planned)

A worker holds a job under a lease and renews it with a heartbeat. If the worker dies, the lease expires and another worker takes the job. Results are written in the same transaction that marks the job done, and only if the worker still holds the lease.

Why: with a lease, a slow worker and its replacement can both finish the same job. Fenced completion turns the second one into a no-op, so results are stored once. Lease times are computed with the database clock, so clock differences between workers do not matter.

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

Why: the queue needs `FOR UPDATE SKIP LOCKED` and `LISTEN/NOTIFY`, which 17 has, and nothing in the design needs a newer server. Identifiers will be UUIDv7 generated in the application, so the schema does not depend on a server-side generator.

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
