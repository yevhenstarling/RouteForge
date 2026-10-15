# RouteForge architecture and decision log

RouteForge turns a day's parcel orders into courier routes. This file records how the system is put together and why it is built that way.

Decisions are numbered so that code comments and commits can refer to them. Each one says whether it is **in place** or **planned**; [plan.md](plan.md) says which phase builds the planned ones. When a phase makes a new decision or changes an old one, it is recorded here in the same commit.

## System at a glance

Three services and one database:

- **api** accepts orders, handles authentication and puts work on the queue.
- **solver** runs as several identical instances. Each one takes jobs from the queue and computes routes.
- **bot** is a Telegram bot that takes orders in a chat and places them through the api.
- **PostgreSQL** holds both the domain data and the job queue.

There is no load balancer and no message broker. A free solver takes the next job from the queue itself.

How a plan is made:

1. An operator asks the api to plan a service date. In one transaction the api inserts the plan and one `plan` job.
2. A solver claims the `plan` job and groups the date's pending orders into one cluster per courier, each within what its courier can carry out and can carry back (D65). In the transaction that completes that job, it marks those orders as planned and inserts one `route` job per cluster.
3. Solvers claim the `route` jobs. Each one optimizes the stop order for a single courier, so that the courier is never overloaded on the way, and writes the route in the transaction that completes its job. The distances it works on are great circles, or roads where the solver was given an OSRM server to ask (D66). A route by road is stored with the way it takes along the streets, which the solver asks the same server for (D73).
4. When the last `route` job completes, the plan is marked done.

A plan that is cancelled, or that loses a job for good, undoes itself: its routes are deleted and its orders are pending again (D20). A plan that is done can be discarded, which undoes it the same way, so that its date can be planned again (D30). A date is planned again only so: while a plan of it is done, another is refused (D83).

How a plan is watched:

1. Every change to a plan or to one of its jobs is announced in the database, by the function of the queue that makes it and in the transaction that makes it (D48). A solver that is searching for a route also announces, four times a second, how far it has got and which route is the best so far (D49).
2. One connection of the api listens. What it hears reaches every open event stream, and a stream that hears of its plan reads the plan again and sends what has changed (D50).
3. The page, which the api serves at `/`, reads the stream and draws: a dashed line for a route that is still searched for, a solid one when it is stored (D52). A stored route that has a way is drawn along it (D73). Beside each route the page charts the length its search announced, and it opens a stored route as a sheet of its stops (D75).
4. When the plan is done, the page sends a van along each route. That is a simulation of the page's own: nothing tracks a courier (D74).

How an order is placed by chat:

1. A person shares a location with the bot. Where the bot was given a geocoding service, they may write an address instead, and pick the place among those the service found (D72). The bot asks with buttons for what an order needs beside the place: a delivery or a pickup, how many parcels, which day. It shows what it is about to place (D58, D61).
2. On the confirming button the bot shows the api its key and says which Telegram user it is talking to. The api answers with a short-lived token of that user's account, which it makes if this is the first it hears of them (D59, D60).
3. With that token the bot places the order: as that person, with their rights and no others (D9). It sends a key with it, so that an order whose answer was lost can be asked for again and is placed once (D76).
4. The order announces its day in the database. The api passes the word on, and a page that shows that day reads its orders again, so the order is on the map (D63).

How an order is placed on the page:

1. Somebody opens an account on the page, which is a client's, or logs in. A click on the map begins an order there, for the day that is shown, and a button places it, with a key as the bot's has (D76, D80).
2. An operator's page shows the order at once, as it shows one that was placed by chat (D63).
3. The client's page lists their orders of the day, and has a stream of its own. The stream says when something has become of one of them, and nothing of anybody else's (D77).
4. When an operator plans the day, the client's page shows the order as planned, and, once its route is stored, which courier has it and which stop of the route it is (D78). That is all a client is told of a route.

How it is run:

1. `docker compose up --build --scale solver=4`, on a clone with no settings, brings up the database, the api, four solvers and the bot (D54).
2. The api waits for the database to be healthy and applies the schema. The solvers wait for the schema (D32). The bot waits for the api, and without a token from Telegram it idles (D62).
3. `scripts/demo` shows the two claims about the workers on that stack: more solvers finish a plan sooner, and a solver that dies loses no route (D57).
4. Two things ask a service outside these four, and are off until somebody turns them on: distances by road, from an OSRM server that the stack runs behind a profile (D67), and the bot's search for an address (D72). On a clone the stack asks nobody but the tile server of the map, and Telegram if the bot has a token (D8).

## Crates

The workspace has exactly five crates, as the [approved description](description.md) requires.

| Crate | Role | Workspace crates it may use |
|---|---|---|
| `routeforge-tsp` | Clustering and route optimization. Pure computation. | none |
| `routeforge-domain` | Domain types, DTOs and geo helpers. With the `db` feature: persistence and the job queue. With the `openapi` feature: the schemas of the DTOs. With the `service` feature: the logging, the stop signal and the pause before a retry that the three services share (D81). | none |
| `routeforge-api` | HTTP API, auth, OpenAPI, the map page. | `domain` with `db`, `openapi` and `service` |
| `routeforge-solver` | Queue worker. Asks an OSRM server for distances by road and for the way a route takes, if it is given one. | `domain` with `db` and `service`, `tsp` |
| `routeforge-bot` | Telegram bot. Asks a geocoding service where an address is, if it is given one. | `domain` with `service`, without `db` |

## Decisions

Every decision with its number, what it holds, and whether it holds now. A decision that was replaced says by which.

| | Decision | Status |
|---|---|---|
| [D1](#d1-one-order-is-one-stop-relative-to-a-depot-in-place) | One order is one stop, relative to a depot | in place |
| [D2](#d2-a-plan-is-one-plan-job-that-fans-out-into-route-jobs-in-place) | A plan is one `plan` job that fans out into `route` jobs | in place |
| [D3](#d3-persistence-lives-in-routeforge-domain-behind-the-db-feature-in-place) | Persistence lives in `routeforge-domain`, behind the `db` feature | in place |
| [D4](#d4-costs-are-integer-metres-in-place) | Costs are integer metres | in place |
| [D5](#d5-the-search-is-deterministic-a-deadline-only-cuts-it-short-in-place) | The search is deterministic; a deadline only cuts it short | in place |
| [D6](#d6-jobs-are-delivered-at-least-once-completion-is-fenced-in-place) | Jobs are delivered at least once; completion is fenced | in place |
| [D7](#d7-capacity-first-version-all-demands-on-a-route-share-the-limit-replaced-by-d65) | Capacity, first version: all demands on a route share the limit | replaced by D65 |
| [D8](#d8-the-demo-works-without-the-internet-in-place) | The demo works without the internet | in place |
| [D9](#d9-the-bot-has-no-authority-of-its-own-in-place) | The bot has no authority of its own | in place |
| [D10](#d10-rust-1990-pinned-in-place) | Rust 1.99.0, pinned | in place |
| [D11](#d11-postgresql-17-in-place) | PostgreSQL 17 | in place |
| [D12](#d12-one-tls-stack-no-native-crypto-libraries-in-place) | One TLS stack, no native crypto libraries | in place |
| [D13](#d13-lint-baseline-in-place) | Lint baseline | in place |
| [D14](#d14-node-0-is-the-depot-and-the-distance-matrix-is-symmetric-in-place) | Node 0 is the depot, and the distance matrix is symmetric | in place |
| [D15](#d15-one-control-for-all-stages-one-iteration-is-one-candidate-move-in-place) | One control for all stages; one iteration is one candidate move | in place |
| [D16](#d16-sweep-largest-courier-first-then-repair-in-place) | Sweep: largest courier first, then repair | in place |
| [D17](#d17-the-annealing-schedule-in-place) | The annealing schedule | in place |
| [D18](#d18-annealing-runs-until-it-is-stopped-in-place) | Annealing runs until it is stopped | in place |
| [D19](#d19-schema-conventions-in-place) | Schema conventions | in place |
| [D20](#d20-an-order-is-on-one-route-at-most-and-a-plan-that-ends-early-undoes-itself-in-place) | An order is on one route at most, and a plan that ends early undoes itself | in place |
| [D21](#d21-the-depot-has-versions-in-place) | The depot has versions | in place |
| [D22](#d22-the-job-row-and-the-claim-in-place) | The job row and the claim | in place |
| [D23](#d23-retries-final-failure-and-cancelling-in-place) | Retries, final failure and cancelling | in place |
| [D24](#d24-lock-order-the-plan-row-first-in-place) | Lock order: the plan row first | in place |
| [D25](#d25-completing-a-job-is-a-handle-not-a-closure-in-place) | Completing a job is a handle, not a closure | in place |
| [D26](#d26-a-notification-is-a-hint-in-place) | A notification is a hint | in place |
| [D27](#d27-the-worker-registry-is-not-part-of-the-queue-in-place) | The worker registry is not part of the queue | in place |
| [D28](#d28-query-metadata-is-committed-and-a-build-needs-no-database-in-place) | Query metadata is committed, and a build needs no database | in place |
| [D29](#d29-distances-and-the-flat-map-in-place) | Distances and the flat map | in place |
| [D30](#d30-a-plan-that-is-done-can-be-discarded-and-its-date-planned-again-in-place) | A plan that is done can be discarded, and its date planned again | in place |
| [D31](#d31-how-long-a-route-is-searched-for-is-part-of-the-plan-in-place) | How long a route is searched for is part of the plan | in place |
| [D32](#d32-the-api-owns-the-schema-and-a-solver-waits-for-it-in-place) | The API owns the schema, and a solver waits for it | in place |
| [D33](#d33-the-worker-loop-in-place) | The worker loop | in place |
| [D34](#d34-a-lease-lives-exactly-as-long-as-the-jobs-future-in-place) | A lease lives exactly as long as the job's future | in place |
| [D35](#d35-a-worker-that-is-told-to-stop-drops-its-job-and-gives-it-back-in-place) | A worker that is told to stop drops its job and gives it back | in place |
| [D36](#d36-the-plan-job-is-one-transaction-in-place) | The `plan` job is one transaction | in place |
| [D37](#d37-what-counts-as-a-failed-attempt-in-place) | What counts as a failed attempt | in place |
| [D38](#d38-one-dockerfile-a-target-for-each-service-in-place) | One Dockerfile, a target for each service | in place |
| [D39](#d39-the-http-api-one-prefix-plain-json-one-shape-of-error-in-place) | The HTTP API: one prefix, plain JSON, one shape of error | in place |
| [D40](#d40-passwords-and-tokens-in-place) | Passwords and tokens | in place |
| [D41](#d41-who-may-call-what-is-a-table-and-the-tests-hold-the-api-to-it-in-place) | Who may call what is a table, and the tests hold the API to it | in place |
| [D42](#d42-one-id-follows-a-request-into-the-logs-of-the-solvers-in-place) | One id follows a request into the logs of the solvers | in place |
| [D43](#d43-the-openapi-document-comes-from-the-types-and-the-handlers-in-place) | The OpenAPI document comes from the types and the handlers | in place |
| [D44](#d44-a-list-is-read-in-pages-by-the-id-of-the-last-item-in-place) | A list is read in pages, by the id of the last item | in place |
| [D45](#d45-the-api-gives-a-plan-its-budget-and-its-seed-when-it-names-none-in-place) | The API gives a plan its budget and its seed when it names none | in place |
| [D46](#d46-demo-orders-come-from-the-api-from-a-seed-in-place) | Demo orders come from the API, from a seed | in place |
| [D47](#d47-the-api-ends-at-once-when-it-cannot-start-in-place) | The API ends at once when it cannot start | in place |
| [D48](#d48-a-change-to-a-plan-is-announced-where-it-is-made-in-place) | A change to a plan is announced where it is made | in place |
| [D49](#d49-a-search-hands-out-its-best-route-and-its-worker-announces-it-in-place) | A search hands out its best route, and its worker announces it | in place |
| [D50](#d50-one-connection-listens-and-the-event-streams-read-in-place) | One connection listens, and the event streams read | in place |
| [D51](#d51-the-api-stops-at-once-with-streams-open-in-place) | The API stops at once with streams open | in place |
| [D52](#d52-the-page-is-one-file-inside-the-binary-in-place) | The page is one file, inside the binary | in place |
| [D53](#d53-the-plans-of-a-day-are-listed-whole-in-place) | The plans of a day are listed whole | in place |
| [D54](#d54-the-stack-is-one-command-and-needs-no-settings-in-place) | The stack is one command, and needs no settings | in place |
| [D55](#d55-the-api-answers-for-its-own-health-in-place) | The API answers for its own health | in place |
| [D56](#d56-the-bot-holds-its-place-in-the-stack-before-it-is-built-in-place) | The bot holds its place in the stack before it is built | in place |
| [D57](#d57-the-demonstration-is-a-script-in-place) | The demonstration is a script | in place |
| [D58](#d58-the-bot-takes-buttons-and-commands-and-free-text-waits-for-a-key-in-place) | The bot takes buttons and commands, and free text waits for a key | in place |
| [D59](#d59-a-telegram-user-gets-an-account-of-their-own-the-first-time-the-bot-hears-of-them-in-place) | A Telegram user gets an account of their own, the first time the bot hears of them | in place |
| [D60](#d60-the-bot-shows-a-key-and-is-given-a-token-of-the-person-in-place) | The bot shows a key, and is given a token of the person | in place |
| [D61](#d61-the-bot-is-a-dialogue-that-knows-nothing-of-telegram-behind-a-part-that-decides-nothing-in-place) | The bot is a dialogue that knows nothing of Telegram, behind a part that decides nothing | in place |
| [D62](#d62-how-the-bot-starts-waits-and-stops-in-place) | How the bot starts, waits and stops | in place |
| [D63](#d63-an-order-announces-itself-in-place) | An order announces itself | in place |
| [D64](#d64-the-bot-is-tried-against-stand-ins-and-its-requests-are-held-to-the-apis-document-in-place) | The bot is tried against stand-ins, and its requests are held to the API's document | in place |
| [D65](#d65-what-a-courier-has-on-board-is-followed-along-the-route-in-place) | What a courier has on board is followed along the route | in place |
| [D66](#d66-distances-by-road-come-from-an-osrm-server-and-a-route-says-what-it-was-measured-along-in-place) | Distances by road come from an OSRM server, and a route says what it was measured along | in place |
| [D67](#d67-osrm-is-a-service-of-the-stack-behind-a-profile-on-roads-that-are-fetched-once-in-place) | OSRM is a service of the stack behind a profile, on roads that are fetched once | in place |
| [D68](#d68-a-login-is-counted-and-passwords-are-hashed-a-few-at-a-time-in-place) | A login is counted, and passwords are hashed a few at a time | in place |
| [D69](#d69-the-page-is-served-with-a-content-security-policy-in-place) | The page is served with a content security policy | in place |
| [D70](#d70-the-api-says-when-it-runs-on-the-demonstrations-values-in-place) | The API says when it runs on the demonstration's values | in place |
| [D71](#d71-a-depot-can-be-read-by-its-id-and-a-plan-is-drawn-from-its-own-in-place) | A depot can be read by its id, and a plan is drawn from its own | in place |
| [D72](#d72-the-bot-looks-an-address-up-at-a-service-it-is-given-in-place) | The bot looks an address up, at a service it is given | in place |
| [D73](#d73-a-route-by-road-keeps-the-way-it-takes-and-the-map-draws-it-in-place) | A route by road keeps the way it takes, and the map draws it | in place |
| [D74](#d74-the-vans-on-the-map-are-a-simulation-and-each-page-reckons-its-own-in-place) | The vans on the map are a simulation, and each page reckons its own | in place |
| [D75](#d75-the-chart-of-a-search-and-the-route-sheet-are-the-pages-in-place) | The chart of a search and the route sheet are the page's | in place |
| [D76](#d76-a-key-makes-placing-an-order-safe-to-repeat-in-place) | A key makes placing an order safe to repeat | in place |
| [D77](#d77-a-stream-says-when-something-has-become-of-ones-own-orders-in-place) | A stream says when something has become of one's own orders | in place |
| [D78](#d78-whoever-placed-an-order-is-told-where-it-is-on-its-route-in-place) | Whoever placed an order is told where it is on its route | in place |
| [D79](#d79-an-operators-page-hears-of-a-plan-that-was-asked-for-elsewhere-in-place) | An operator's page hears of a plan that was asked for elsewhere | in place |
| [D80](#d80-the-page-takes-an-account-and-an-order-and-lists-a-clients-own-in-place) | The page takes an account and an order, and lists a client's own | in place |
| [D81](#d81-the-services-share-their-plumbing-through-a-feature-of-the-domain-crate-in-place) | The services share their plumbing through a feature of the domain crate | in place |
| [D82](#d82-a-job-that-panics-fails-its-attempt-and-a-solver-that-loses-a-worker-ends-in-place) | A job that panics fails its attempt, and a solver that loses a worker ends | in place |
| [D83](#d83-a-date-has-one-plan-that-stands-in-place) | A date has one plan that stands | in place |
| [D84](#d84-the-live-view-counts-the-jobs-of-the-last-day-and-forgets-the-workers-of-long-ago-in-place) | The live view counts the jobs of the last day, and forgets the workers of long ago | in place |
| [D85](#d85-one-person-holds-only-so-many-event-streams-open-in-place) | One person holds only so many event streams open | in place |
| [D86](#d86-a-script-tries-the-page-in-a-browser-in-place) | A script tries the page in a browser | in place |
| [D87](#d87-demo-orders-lie-on-the-streets-of-kyiv-in-place) | Demo orders lie on the streets of Kyiv | in place |

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

Since Phase 12 a third feature, `service`, holds what the three services share beside their types. The bot takes that one, and neither of the others (D81).

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

### D7. Capacity, first version: all demands on a route share the limit (replaced by D65)

A route was feasible when the sum of its demands, pickups and deliveries together, was within the courier's capacity.

Why: it never overloads a courier and needs no knowledge of the stop order. It is stricter than necessary, because a courier who delivers before picking up could carry more in total.

Phases 1 to 7 ran on this rule. Since Phase 8 the load is followed along the route (D65). A route that the old rule allowed is still searched for exactly as it was: no order of its stops can overload its courier, so the search has nothing to look out for.

### D8. The demo works without the internet (in place)

Distances default to Haversine. The Leaflet library is vendored into the repository. Map tiles are optional: without them the routes still draw. The bot idles when its Telegram token is missing.

Haversine has been the default since Phase 3. The page and its Leaflet are in the binary of the API, and a test fails if the page names any host but the tile server (D52). The bot is the one part that is nothing without the internet: Telegram is where its chats are. Without a token it says so and idles, and with one and no network it waits for Telegram and goes on trying (D62). Nothing else in the stack waits for the bot.

Two things of Phase 8 ask another service, and both are off until somebody turns them on. Distances by road come from an OSRM server, which is a service of the stack behind a profile and runs on this machine, on roads that were fetched once (D67). Where an address is comes from a geocoding service that the bot is given by name (D72). On a clone neither is there: the solvers measure along great circles, and the bot sends what people write to nobody. A solver that was given a server and gets no answer falls back on great circles, route by route (D66).

Building is not part of this. A build asks the registry for its base images, and the first one fetches every crate. Once the images are there, `docker compose up` without `--build` starts them and asks nobody (D54).

### D9. The bot has no authority of its own (in place)

The bot calls the api with a short-lived token of the Telegram user it is talking to. Actions with side effects are confirmed with a button, and the confirmation is handled in code.

Why: whatever the bot is made to do, the api applies that user's role, so the bot cannot do more than the user could. That holds for a fault in the bot, for what somebody sends in the place of what a button carries, and for the language model that may one day read free text (D58). The confirmation step keeps a slip of the finger, or a misread message, from creating or cancelling anything.

How it is built: the token is D60, and the confirmation is D61. The bot looks at a person's role in two places, to list the commands that are theirs and to say at once that planning is an operator's. Neither is a gate. Take both out and a client is refused all the same, by the api, a press later.

### D10. Rust 1.99.0, pinned (in place)

`rust-toolchain.toml` pins the compiler for developers and CI. The Docker image is built with the same version, which the Dockerfile repeats in the tag of its base image (D38).

Why: sqlx 0.9 needs Rust 1.94 or newer, and one compiler everywhere removes differences between machines.

Check before raising the pin: teloxide 0.17 depends, through `aquamarine`, on `proc-macro-error2` 2.0.1. Rust 1.99 compiles it with a future-incompatibility warning (rust-lang/rust#127909), and a later compiler will reject it. The crate is also marked unmaintained (RUSTSEC-2026-0173). Clippy prints the warning on every run, and `cargo audit` the advisory, as an allowed warning. So the compiler stays where it is until teloxide no longer needs that crate: watch its releases.

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

Consequence for road distances, which are not symmetric: the solver makes the table symmetric before it builds the matrix, and measures the finished route along the directed table itself. Built in Phase 8, and how is D66.

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

Since Phase 8 a courier has room in two ways, for what it carries out and for what it carries back, and a stop fits when it fits the one it asks for (D65). The walk, the repair and the guarantees are as they were. "Largest first" goes by the larger of a stop's two demands.

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

A plan that is done keeps its orders. Until Phase 12 a later plan for the same date took the orders that were still pending and added its routes to those that were there. Since then there is no later plan while a plan that is done stands: it is discarded first, and the date planned afresh (D83). Getting the orders of a done plan back is D30.

### D21. The depot has versions (in place)

Setting the depot inserts a row, and no row is ever updated. The newest row is the current depot. A plan records the id of the depot it was made for.

Why: the clusters and the routes of one plan are computed by different jobs, possibly minutes apart. With one row that is updated in place, moving the depot in between would give routes that start somewhere other than the place the clusters were formed round, and every old plan would be redrawn from the new place. An id on the plan rules out both, and needs no lock.

Until Phase 8 only the newest row could be read through the API, so the page drew an old plan from the new place all the same. A depot can be read by its id now, and the page draws a plan from the depot it names (D71).

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

Until Phase 12 a date whose plan was not discarded was planned as before: a later plan took the orders that were still pending. Since then such a date is refused another plan until its plan is discarded (D83).

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
- A panic in the search is such an error. Since Phase 12 so is a panic anywhere in a job, and a worker that is lost all the same ends its solver (D82).
- A job this build cannot read is logged and the next one claimed. The queue has counted that attempt already (D22).
- Trouble with the database outside a job is not held against any job. A claim that fails is tried again after the poll interval, a heartbeat at the next heartbeat, and a connection at startup after a pause that doubles up to 30 seconds.
- A failure that cannot be reported is left alone. The lease runs out, and the queue treats the job as it treats that of a worker that died.

Why: the queue already bounds the damage of a job that takes its worker down with it. Reporting the reason makes the same outcome quicker and readable. And a database that is away for a minute says nothing about any job, so it must not use up the attempts of the jobs that happened to be running.

### D38. One Dockerfile, a target for each service (in place)

The Dockerfile at the root builds every service, and each service is a target: `docker build --target solver .`. Phase 3 made the `solver` target, and Phase 6 the targets `api` and `bot`.

- **cargo-chef.** The dependencies are built as a layer of their own, from the manifests and the lock file alone. A change to the workspace's crates then rebuilds in seconds. cargo-chef is pinned, like everything else.
- **Base images.** The Rust image of the pinned compiler to build, Debian slim to run. `rust-toolchain.toml` is kept out of the build context: the tag of the base image is the pin (D10), and with the file there rustup would also fetch the clippy and rustfmt it names.
- **What the build needs.** No database, because queries are checked against `.sqlx/` (D28), and nothing native that the base image does not have (D12). The solver's binary links to the C library and nothing else, so the runtime image needs neither OpenSSL nor CA certificates. The bot is the one service that speaks TLS, to Telegram, and the roots it trusts are compiled into it (D62). It runs as a user without privileges.
- **Compose.** The `solver` service has no container name and publishes no port, which is what lets `--scale solver=N` work. It is given the database's address on the compose network. The solver's other settings are passed on when they are set in `.env` and left to their defaults when they are not.

Why one file: the services are binaries of one workspace and share almost all their dependencies. With one builder stage they are compiled once, however many targets are built.

The builder stage builds the three packages with one `cargo build`. Cargo then gives a crate they share the features of all three, so in the image the bot is linked against a domain crate that was compiled with `db` and `openapi`, as it is by `cargo test --workspace` on any machine. What the bot's own manifest asks for is unchanged, and `cargo check -p routeforge-bot` in CI is what holds it to that (D3).

The binaries are 16 MB for the API, which has the page and Swagger UI in it, 6 MB for the solver and 12 MB for the bot, which has teloxide's types for all of Telegram's Bot API in it, and rustls. Each links to the C library, `libm` and `libgcc`, and to nothing else. The rest of an image is Debian: the images are 140, 126 and 134 MB.

CI builds the `solver` target in a job of its own, so a dependency that needs a native library fails there and not on the day of the demo. The builder stage is shared, so that job compiles all three services. The targets `api` and `bot` themselves, and the stack, are built and started by hand: `docker compose build`.

### D39. The HTTP API: one prefix, plain JSON, one shape of error (in place)

Everything the API serves as data is under `/api`. Beside it are `/healthz` and `/readyz`, the OpenAPI document at `/api/openapi.json` and Swagger UI at `/docs/`. The page is at the root, and what it loads under `/vendor/` (D52).

Three endpoints answer with `text/event-stream` and go on answering: `/api/plans/{id}/events`, `/api/system/stats/events` (D50) and, since Phase 10, `/api/orders/events` (D77). A caller they refuse is answered as by any other endpoint, with a problem.

- A thing is read with `GET`, made with `POST`, which answers `201`, replaced with `PUT` and changed in part with `PATCH`. What changes the state of a thing and is no edit of it is a `POST` to a path of its own: `/api/orders/{id}/cancel`, `/api/plans/{id}/cancel`, `/api/plans/{id}/discard`.
- An order that is asked for again with the key it was placed with is answered `200`, with the order, and not `201`: nothing was made this time (D76).
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
- A token the bot is given for a Telegram user is of the same key and the same shape, and is read by the same check. It is good for `BOT_JWT_TTL_SECS`, a minute by default (D60).
- The role is in the token, and nothing is looked up to check a request.
- An account is opened as a client's, with an address and a password or by the bot for a Telegram user (D59). Since Phase 10 the page has a button for the first (D80). An admin makes operators and admins of them, and cannot change their own role.
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

Since Phase 8 a login is counted before anything is looked up or hashed, and only so many passwords are hashed at a time (D68).

### D41. Who may call what is a table, and the tests hold the API to it (in place)

| | Client | Operator | Admin |
|---|---|---|---|
| Open an account, log in, health, the document, the page | without a token | | |
| Log in a Telegram user | no: the bot alone, by its key (D60) | no | no |
| Orders: place, list, read, withdraw | their own | all | all |
| Orders: where they are on their routes (D78) | their own | all | all |
| Orders: watch (D77) | their own | their own | their own |
| The depot: read | yes | yes | yes |
| The depot: set | no | no | yes |
| Couriers | no | yes | yes |
| Plans: ask, list, read, watch, their routes, cancel, discard | no | yes | yes |
| The stats, read and watched | no | yes | yes |
| Users, roles, demo orders | no | no | yes |

A handler says who may call it by the type of its first argument: `AuthUser` for anybody with a token, `Staff` for an operator or an admin, `Admin`, and `Bot` for the Telegram bot, which shows a key and no token (D60). actix-web takes the arguments of a handler in their order, so with this one first the gate is checked before the body is read.

One test has the table, written out by hand as a list of every endpoint with the least role it needs. It sends every request as nobody and as each role, and expects `401`, `403` or anything else, as the table says. Where the bot's key is asked for, every one of them is answered `401`, an admin included: a token is not a key that falls short, it is no key. Another test reads the endpoints out of the OpenAPI document and compares them with the list.

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

`POST /api/demo/orders`, for admins, stores up to 2000 pending orders for a date, within a radius of the depot. The radius is 10 km by default, 5 km before Phase 13, and 30 km at most. Since Phase 13 each order is at a point of a street of Kyiv (D87). Round a depot with no street within the radius, they are spread evenly over the disc, as all of them were before. About one in five is a pickup, and each has a demand of 1 to 3. They belong to the admin who asked. They are stored in one statement, `orders::create_many`, since Phase 12, and in one transaction of an insert each before that.

Every random choice comes from a ChaCha8 generator seeded with the request's seed, as in `routeforge-tsp` (D5). The same seed gives the same orders round the same depot, from the same streets.

Why:

- A demonstration needs a few hundred orders, and a tool to make them would be a sixth crate. In the API it is one more endpoint.
- With a seed, the scaling run of Phase 6 plans the same day with one solver and with four, and anybody can repeat a demonstration.
- On the disc, the distance from the depot is the radius times the square root of a random number. Without the root the orders would crowd round the depot, since a ring's share of the disc grows with its radius.
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

An order that no plan has taken is nobody's to announce here. Since Phase 7 it announces itself, on a channel of its own (D63).

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

A ceiling, noted in the review of Phase 12: PostgreSQL commits the transactions that notified one at a time, under a lock of the whole database, so that the notifications arrive in the order of the commits. Every order, claim and completion notifies, and so does every search, four times a second. At the scale of the demonstration that is nothing. Under a heavy load of writes it is the first thing to give, and the progress of the searches is then the first thing to send less often, or another way.

What there is to see, measured with four solvers in containers on the development machine:

- Routes of about a hundred stops, as 300 demo orders give, are at the length they end with by the second announcement, a quarter of a second in. From the first tour to a local optimum takes the search a few milliseconds, and annealing then finds little or nothing. The line appears, jumps once, and stands.
- Routes of about 350 stops, from 1200 orders, went from some 85 km to some 68 km in that first quarter of a second, and then lost between half a percent and two and a half percent more over ten seconds, a few edges at a time.

So for a line that is seen to move, a day has to give its routes several hundred stops.

### D50. One connection listens, and the event streams read (in place)

The API has one connection that listens on the two channels of D48, and since Phase 7 on the one of D63. What it hears goes into a `tokio::sync::broadcast` channel, and every open stream holds a receiver of it. Two endpoints stream, as `text/event-stream`:

| Endpoint | Events |
|---|---|
| `GET /api/plans/{id}/events` | `plan`: the plan. `jobs`: its route jobs, each a `RouteJob`. `progress`: a `RouteProgress`. |
| `GET /api/system/stats/events` | `stats`: what `GET /api/system/stats` gives. `orders`: an `OrdersChanged`, the day an order was placed or withdrawn for (D63). `plans`: a `PlanChanged`, the plan that has changed (D79). |
| `GET /api/orders/events?service_date=` | `orders`: an `OrdersChanged`, when something has become of an order of the caller's own for that day (D77). Since Phase 10. |

A stream opens with everything it shows. After that:

- It reads again when it is given a reason: a notification for its plan, or for any plan in the case of the stats. It sends what differs from what it last sent, and nothing if nothing does.
- It reads at most every 200 ms, so a burst of changes is one read.
- It reads without a reason too, every ten seconds for a plan, as for one's own orders, and every two for the stats, as a worker polls (D33). A notification can be lost, and a worker comes and falls silent without one (D27).
- When the listener has had to reconnect, and when a stream has fallen more than 1024 signals behind, the stream is told that it may have missed something, and reads.
- Progress is passed on as it comes, with no read. So is the word that an order was placed or withdrawn, by the stats stream. That a plan has changed is passed on by the stats stream as well, and is a reason for it to read (D79). The stream of one's own orders passes nothing on: it reads, and speaks if what it read has changed (D77).
- A comment line every 15 seconds keeps a quiet stream from being taken for a dead one.

A stream ends when its reader has gone, when the token it was opened with expires, when the API is told to stop (D51), and when a read fails. Since Phase 12 one person holds only so many streams open at once (D85).

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
- **Text from the API goes into the page as text**, never as HTML. An address or the name of a courier is typed by somebody. Since Phase 8 the browser is told as much: the page is served with a policy that lets it run its own script and no other (D69).
- **On the map** the stops are dots on a canvas and the routes are lines in SVG above it. Leaflet draws a canvas in the order things were added to it. On one canvas a route would be hidden under dots that were drawn again after it, as they are when a plan takes its orders.
- **A client** who logs in sees the depot and their own orders, and no planning. That is what the API lets a client read (D41). Until Phase 10 a client could only look (D80).

What the page can do beyond the list in the plan was decided by the project owner on 2026-10-11:

- **Discard** a plan that is done, beside Cancel.
- **Budget and seed** for a plan: seconds or millions of moves for each route, and a seed. Left empty, the API gives its own (D45).
- **Set up a demo city**, for an admin, on a database with no depot or no courier: a depot in Kyiv and four vans, through the endpoints an admin would use by hand. The vans are sized for the number of orders in the field beside the button. A demo order has a demand of 1 to 3 (D46), so a day asks for twice its number. Four in five of its orders are deliveries, and it is the deliveries a van has to have room for, since what is picked up comes on board when they are off it (D65). Each van takes a quarter of them and a tenth more. So the day that is generated next gives every van a route, whether it has 300 orders or 2000.

What Phase 8 added to it:

- **Beside the length of a route, what it was measured along:** `by road` or `as the crow flies` (D66). The lines stayed straight, from stop to stop. Drawing them along the streets was offered beside it and not taken, on 2026-10-12. Phase 9 draws them so (D73).
- **Beside the stops of a route, the most its courier has on board** along it, and how much the courier carries: `at most 131 of 132 on board` (D65).
- **A plan is drawn from its own depot** (D71). When the depot has been moved since the plan was made, the plan's depot is on the map as well, as a grey dashed ring.

What Phase 9 added to it:

- **A stored route is drawn along the streets it takes,** where it was measured by road and the routing service gave its way (D73). A search under way is a dashed straight line, as before.
- **Vans that drive the routes of a plan that is done.** They are a simulation, which the page says beside them (D74).
- **A small chart in the row of a route,** of its length while it was searched for (D75).
- **A route sheet:** the stops of a route in calling order, with what is on board after each. A click on a route's line opens it, and a row of it shows its stop on the map (D75).

What Phase 10 added to it, all of it in D80:

- **An account opened on the page,** by a second button of the login.
- **An order placed by a click on the map,** with a key that makes it safe to ask for again (D76). For whoever is logged in.
- **A client's own orders as a list,** each with what has become of it, a button that withdraws a pending one, and for a planned one the courier and the stop (D78).
- **A stream for a client's page** (D77), and for an operator's the plans that were asked for elsewhere (D79).

So a client no longer only looks. What a client is shown is still what the API lets a client read (D41), which since Phase 10 includes where their own orders are on their routes.

What the page learns only from Refresh. Until Phase 7 that was an order that somebody else had placed. An operator's page hears of one since then (D63), and a client's of their own since Phase 10 (D77). A plan that was asked for elsewhere shows on an operator's page since Phase 10 as well (D79). What is left is the depot and the couriers: a page that was open when an admin moved the one or added the other shows it after Refresh.

The page has no tests of its own. The API's tests check that it is served and what it loads. What it does was tried in a browser: `docs/plan.md` says how. Since Phase 12 a script does that, run by hand: `scripts/page-check.mjs` (D86).

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

- **Every setting has a value to fall back on.** For most that is the default of the binary. Four have none there, and fall back to values written into `docker-compose.yml`: the key that signs the tokens, the address and password of the first admin, `admin@example.com` and `routeforge`, and since Phase 7 the key the bot shows (D60). Decided by the project owner on 2026-10-11. The binary still has no default for either key (D40).
- **One setting has nothing to fall back on, and needs nothing:** the bot's token from Telegram. Without it the bot idles (D62).
- **The ports are open to this machine alone.** The API is published on `127.0.0.1:8080`, as the database is on `127.0.0.1:5432`.
- **No start order beyond the table.** A solver waits for the database, and for the schema by itself (D32), so the API and the solvers start side by side.
- **No restart policy.** A service that ends stays ended.
- **The database's health is asked over TCP**, with `pg_isready -h 127.0.0.1`.

Why:

- The claim is "from a fresh clone". A step before the command, a file to copy or a key to think of, is a step that can be got wrong by whoever is looking at the project for the first time.
- Values that are in the repository protect nothing, so nothing may depend on them: the ports are on the loopback. Whoever opens them to a network sets their own values in `.env`, and `.env.example` says so beside each. Since Phase 8 the API says so too, in its log, when it starts on one of them (D70).
- The other way to get a key was to make a random one at each start. Every login would then end with every restart, and the admin's password would still be one that everybody can read.
- A service that ends has said why (D47), and one that is started again by itself says it over and over. `docker compose up` once more is the restart.
- Nothing waits for a solver, and which solvers are alive is what the registry says (D27) and the page shows. A check of its own would be a query every few seconds from every instance, to tell Docker what the system knows already.

Why TCP for the database: the postgres image sets a new database up with a server that listens on the socket alone, and only then starts the one that listens on the network. Asked over the socket, `pg_isready` says yes to the first. Measured on the development machine, on a new volume: the socket said yes and the port was closed for 0.14 seconds, and for 0.16 seconds more nothing listened at all.

What that costs is less than it sounds, and was measured too. Ten starts on a new volume went well with the old check, and ten with the new one. An API that is let go inside that window finds the port closed, and sqlx tries again for five seconds (D47), by which time the database is there. It bites when setting the database up takes longer than that after the first yes. With a script that keeps the set-up going for twelve seconds, as a slow disk or a script that loads data would, the old check let the API start, and it ended with "pool timed out while waiting for an open connection", and `docker compose up` with "dependency failed to start". With the new check the same stack was up after twenty seconds. So the check is right now, and no longer depends on a retry that is not ours.

A test in the API holds the compose file to this. It reads the `api` service out of `docker-compose.yml`, takes every `${VARIABLE:-fallback}` for its fallback, and starts the API's settings from what it finds: the key is long enough, the admin is an account the API would make, the API listens on every address of its container, and the port is published to `127.0.0.1` alone. A variable without a fallback fails it.

A test in the bot does the same for the `bot` service. On what a clone has, the bot idles. Given a token and nothing else, it has the address of the API on the compose network and the very key the `api` service falls back to. And the token itself has no fallback.

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

Phase 7 gave it all of those. The service is the bot itself now, and what the stub did is what the bot does without a token (D62).

### D57. The demonstration is a script (in place)

`scripts/demo.ps1` and `scripts/demo.sh` show the two things the project claims about its workers, on the stack as compose runs it:

- **Scaling.** The same day is planned with one solver and then with four, with the same seed and the same budget of moves, and discarded in between (D30).
- **Failure.** A plan with a budget of time. Two seconds into its searches a solver that holds a route is killed with `docker kill`. The script waits for another worker to hold that route's job, and for the plan to be done.

How:

- **Through the API alone.** The scripts log in, ask for plans, read them and the stats. Nothing is read from the database behind the API's back.
- **Where the API is, and who the admin is, they ask the stack:** `docker compose port api 8080`, and the environment of the api container. So a `.env` with other values needs no telling.
- **What a day needs is made if it is missing:** a depot and four vans, as the page's demo city (D52), and orders. The orders are as many as the couriers can deliver with a tenth to spare, the page's sum turned round (D65). Then every courier gets a route, and every solver has one to search for.
- **The day is tomorrow.** The page opens on today, and on its day a script cancels a plan in progress and discards a done one.
- **The time of a plan is the plan's own:** `finished_at` less `created_at`, both from the database's clock (D6). How often the script looks does not enter into it.
- **The routes of every scaling run are compared with those of the first:** whose route it is, how long, and which stops in which order. Other routes end the script as a failure, because the same seed and the same moves give the same routes (D5).
- **The solver to kill is found in the stats.** A worker registers under its host name (D27), which under Docker is the id of its container, as much of it as `docker kill` needs.
- **After each run the script checks what the system is judged by** (D20): every order the plan took is on exactly one route.
- **Each plan is asked for with a request id of the script's making,** which it prints. `docker compose logs api solver` has it in the lines of the API and of every solver that worked on the plan (D42).

Measured on the development machine, with 16 logical processors and Docker Desktop on WSL2, in Phase 13, on the day of D87: orders on the streets within 10 km of the depot. A new database, 300 orders, four vans of 132, 200 million moves a route:

| Solvers | Seconds | Speed-up | Routes | Kilometres |
|---|---|---|---|---|
| 1 | 15.4 | 1.0 | 4 | 226.70 |
| 2 | 8.2 | 1.9 | 4 | 226.70 |
| 4 | 4.5 | 3.4 | 4 | 226.70 |
| 8 | 4.6 | 3.3 | 4 | 226.70 |

The routes were the same in every run, and a second round gave 15.1, 8.3, 4.5 and 4.6 seconds.

- Four solvers are 3.4 times as fast as one, and not four times. The routes have 82, 85, 84 and 49 stops, but each is searched for the same number of moves, and the one of 49 stops took 3.7 seconds where the others took 3.8 to 3.9. So that is not the reason. A search that ran alone took 3.8 seconds, each of two side by side took 4.1, and each of four took 4.5: the processor gives one busy core more than it gives each of four.
- Eight solvers are no faster than four. The day has four routes.
- The numbers want a machine that is doing nothing else. In Phase 6 the same run, made while the machine was still clearing away other containers, took nearly twice as long with one solver.

The failure run, with four solvers and ten seconds a route: the route was taken over 27.9 seconds after the kill, and the plan was done 40.4 seconds after it was asked for, with 300 stops on four routes and every order on exactly one. The solver that took the route over logs the job with `attempt=2`. A lease runs for thirty seconds from the last heartbeat (D33), which came up to five seconds before the kill, and a worker with nothing to do looks at the queue every five. `SOLVER_LEASE_SECS` and `SOLVER_HEARTBEAT_SECS` set in the shell make it quicker: compose passes them on, and the scripts change no setting themselves.

Before Phase 13 the day was of random points within 5 km of the depot (D46). At the end of Phase 8 it took 15.5, 8.3, 4.5 and 4.8 seconds, for routes of 73, 73, 81 and 73 stops and 126.13 km, and the failure run was done after 40.5 seconds. The numbers of Phase 6 were of the same orders on vans of 165, under the rule of D7: 15.2, 8.1, 4.5 and 4.6 seconds, for routes of 87, 86, 78 and 49 stops and 123.60 km. On vans of 132 the sweep cut that day into four routes that were more alike (D65).

With an OSRM server in the stack (D67) the scripts run as they are. The same day by road was 476.17 km, the same routes with one solver and with four, in 14.8 and 4.3 seconds. The day of Phase 8 was 475.92 km by road, in 16.0 and 5.3 seconds.

Why:

- A script and not a test: it needs Docker, takes a minute and a half, and kills a container. What it shows is in the tests already, in small: three workers drain a plan, and a worker that is aborted is replaced when its lease has run out (D34). The script shows it on the real thing, to a person.
- A budget of moves for the scaling run: with a budget of time every route takes what it is given, so the speed-up would be arithmetic. Moves are work, and the same work gives the same routes, which is what lets the script say that four solvers found what one found.
- A budget of time for the failure run: how long a search goes on is then known, and two seconds in is the middle of it on any machine.
- Two scripts, because the project is developed on Windows and runs anywhere. The PowerShell one runs in Windows PowerShell 5.1 and in PowerShell 7. The other is POSIX sh, and needs `curl` and `jq`: reading JSON with `sed` would have been a second program. It was run under dash, on Ubuntu, with jq 1.8.1, and printed what the PowerShell one prints.

Two things the scripts guard against, both met while they were written:

- **How many solvers.** From inside PowerShell, `-Solvers 1,2,4,8` is a list. Started any other way, as `pwsh -File scripts/demo.ps1 -Solvers 1,2,4,8`, the script is handed one text, and PowerShell reads that text as the number 1248. Compose was asked for 1248 solvers, and had created every one of those containers and started some four hundred before it was stopped. The counts are now taken as text and read by the script, and anything but 1 to 20 is refused before compose is asked (D54).
- **No `--build`.** The scripts start the stack with `docker compose up`, which builds an image that is missing and leaves one that is there. With `--build` they would need the registry every time (D54), and the demo is to work without a network (D8).

### D58. The bot takes buttons and commands, and free text waits for a key (in place)

Decided by the project owner on 2026-10-12.

The bot of Phase 7 asks no model anything. A person shares a location, and the bot asks with buttons for what an order needs beside it: a delivery or a pickup, how many parcels, and which day. It shows what it is about to place, and places it when that is confirmed (D9). Whatever else a client or an operator may do is a command. How that is built is D61.

The approved description calls the bot LLM-powered and names the Claude API. That part is not given up. It is below the cut line: free text is an optional item of Phase 8, built only when there is an Anthropic API key to try it with. `docs/description.md` stays as it was approved, and until the item is built the README says that the bot takes no free text.

Why:

- The Claude API wants a key from an account of its own, with credit on it, paid for by the token. A Claude subscription does not include one. Phase 7 does not wait for it.
- An order says four things: where, which kind, how much and which day. A location and three buttons say them, the same way every time, which is what a presentation wants.
- The bot then leans on no service but Telegram that can be slow, down or out of credit on the day. Whoever clones the repository needs one token, from @BotFather, to see all of it.
- Nothing is built twice. What a button does and what a tool of a model would do is the same request to the API, so free text goes on top of the bot of Phase 7.
- Code for the Claude API that was never run against the Claude API is not shipped. A scripted model shows that the loop is right, and not that the requests are what the API takes. So that part is not written ahead of a key.

What it costs: "deliver two parcels here tomorrow" is a few taps and not a sentence, and for as long as the item of Phase 8 is not built the bot is less than the description says of it.

Phase 8 ended with no key, and the item was not built. The bot does read words since then, for one thing: an address, which it looks up and asks about with buttons (D72). No model is in that, and nothing is done on the strength of words.

On 2026-10-13 the project owner made the item a phase of its own, Phase 11 in `docs/plan.md`, without having decided whether it will be built. What holds for it is what holds here: it is not begun without a key.

### D59. A Telegram user gets an account of their own, the first time the bot hears of them (in place)

Decided with the project owner on 2026-10-12.

Migration `0005_telegram.sql` adds `users.telegram_id`, the id Telegram gives a user, unique and positive. The first time the bot names a Telegram user the API has not heard of, the API makes them an account: a client's, with that id, no address and no password. From then on that account is who they are. `users.email` and `users.password_hash` may be empty for it, under two checks: an account has an address exactly when it has a password, and every account has an address or a Telegram id. In Rust a `User` has an optional `email` and an optional `telegram_id`.

An admin makes such an account an operator's as any other, with `PUT /api/users/{id}/role`. The bot tells a person the id of their account when it greets them, and whenever it refuses them what is an operator's.

Why:

- Whoever opens the chat can place an order, with nothing to do first. That is the "Done when" of the phase, and what a presentation wants.
- The other way was to tie a Telegram user to an account that is there already, by a code that the page hands out and the bot takes. That is a table for the codes, two endpoints and a control on the page, and nobody could order by chat who had not first opened an account through the API. It was offered, and not taken.
- No address is made up for such an account. An address is what a user picks: `4242@telegram` could be registered by anybody before the Telegram user of that id ever writes, and would then block their account, or be it. With no address there is nothing to squat on.
- Nobody logs in to such an account. A login looks an account up by its address, and this one has none, nor a password to check. The first check keeps it so: no address without a password, and no password without an address.
- The second check rules out an account that nobody could reach.

How an account is made once: the bot's first two requests for a newcomer can arrive at the same moment. `users::find_or_create_by_telegram` reads first, since everybody but a newcomer is there already, and then inserts with `ON CONFLICT (telegram_id) DO UPDATE`. That update changes nothing and returns the row. Whoever loses the race waits for the winner and is given the winner's account. With `DO NOTHING` the loser would be given no row at all: the winner's is not in the snapshot of its statement.

What is not built: an account with an address cannot be reached from Telegram. The schema lets an account have both, for the day that is built.

Tests: an account is made once and is the same afterwards, with the role an admin gave it; eight at once make one; the loser of the race, picked by hand, is given the winner's; each of the two checks, and the uniqueness, refuse what they are there for.

### D60. The bot shows a key, and is given a token of the person (in place)

`POST /api/auth/telegram` is the bot's, and the bot's alone. It shows its key as `Authorization: Bearer <key>` and says which Telegram user it is talking to. The answer is a `Session`, as a login gives one: a token of that user's account (D59), the account, and how long the token is good for.

- **The gate** is a fourth extractor, `Bot`, and comes first like the other three (D41). Whatever is not the key is answered `401`: no header, another key, and the token of a user, an admin's included. The key is compared in constant time, with `subtle`.
- **The API has no default for the key.** Started without `BOT_SERVICE_KEY` it takes no bot, whatever it is shown, and says so in the `401`. A key has to be at least 32 bytes long.
- **Compose falls back to a demo key,** the same for the `api` and the `bot` service (D54).
- **The token is short-lived:** `BOT_JWT_TTL_SECS`, a minute by default. Otherwise it is a login's token (D40).
- **The bot keeps no token.** It asks for one whenever a step needs the API, uses it for that step, and lets it go.

Why:

- This is what makes D9 hold. With the person's token the API answers the bot as it answers them. The bot needs no rule of its own about who may do what, and a fault in it cannot open what the person's role keeps shut.
- A token for each step: the role is in the token (D40), so a token that was kept would hold on to the role it was made with. Asked for anew, a role an admin has changed applies to the next thing its owner asks. There is also nothing to renew, and nothing to throw away when an account changes. The price is one request more for each step that needs the API. It is a lookup and a signature, and no password is hashed for it.
- A minute is longer than any step takes. It bounds what a token is worth that was read off a wire.
- No default in the binary, as for the signing key (D40): an API that somebody starts by hand has no way in that they did not mean to open. The fallback in compose is D54's. With the ports on the loopback it gives away nothing that the demo signing key beside it does not, and a clone then needs one thing to have a bot, a token from @BotFather (D58).
- As a bearer and not in a header of its own: the answer to a missing key is then the `401` with `WWW-Authenticate: Bearer` that the API has anyway, and credentials are read in one place. In the OpenAPI document it is a second scheme, `service_key`, so Swagger UI asks for it by name.
- `401` and not `403` for a user's token: a `403` says that the caller is known and may not. A token is not known here at all.

What the key is worth: a token of any account that Telegram reaches. That is whatever account is asked for by its Telegram id, an operator's included. Until Phase 12 an admin's was included as well. Since then the token gives an operator's rights at the most, which are all the bot uses: orders, plans and the depot. The session still names the account as it is, so the bot tells an admin that they are one. Outside a demonstration the key is a secret of the rank of the signing key all the same. Neither the API nor the bot can log it: both hold it in a type without `Debug`.

The request is logged like any other (D42). Its `user_id` is the account the bot asked a token for.

Tests: the key gets a token that places an order as a new client, and opens nothing that is an operator's; every wrong key, an admin's token and no key at all are answered `401`, and leave no account behind; the gate comes before the body; an API without a key takes no bot; the token is good for a minute; a role an admin gives is in the next token and not in the one from before; three requests at once for a newcomer are one account; the log names the account, and neither the key nor the token.

### D61. The bot is a dialogue that knows nothing of Telegram, behind a part that decides nothing (in place)

`routeforge-bot` has three parts:

| Module | |
|---|---|
| `api` | What the bot asks of the API: a trait, `Api`, and `HttpApi`, which asks over HTTP. |
| `chat` | What the bot says and does. What came from a chat goes in, an `Event`. A `Reply` comes out: a text, and the buttons under it. Since Phase 12 in five files: `chat.rs` with the types, how an event is handed on and who is asking, and `chat/draft.rs`, `search.rs`, `orders.rs` and `operator.rs`. |
| `telegram` | Updates of Telegram's become events, and replies become messages. Nothing is decided there. |

How an order is placed:

- It begins with a place: a location that was shared, a venue, whose address goes into the order, or `/order 50.45 30.52`.
- The bot asks for the kind, for the number of parcels, 1 to 5, and for the day: today or one of the two days after it. One message carries the question, and changes in its place from one question to the next.
- When nothing is missing, the message shows the order and has a button that places it. The order is placed by the press of that button, and by nothing before it (D9).

What holds for everything:

- **An order that is being put together is in memory,** one for each chat. Its buttons name it by the message that began it. A button of another draft, of another chat, or of a bot that has been started again is told that the order is no longer open. At most 1024 drafts are kept, and the oldest goes first.
- **What a button means is a type,** `Press`, which writes what the button carries and reads it back. Telegram lets a button carry 64 bytes. A button that is not a draft's says everything itself, an order's id or a day, so it works after the bot has been started again.
- **What changes something is confirmed:** placing an order, withdrawing one, asking for a plan. The first press asks, and only the second does.
- **A draft is taken out before the API is asked,** and given back when the API would not have the order. So a second press that is already on its way finds nothing to place.
- **The buttons for a day say the day they mean,** as `Tomorrow, Tue 13 Oct`. The days are those of the bot's clock (D62). A day that has passed since it was offered is refused, and asked for again.
- **What the API says is what the person is told.** Its `detail` is written for a person (D39), and the bot makes a sentence of it. A failure that is the API's own is said in the bot's words. An order that got no answer is not said to have failed: it may have been stored, and the bot says that it cannot tell. Since Phase 10 it says so only until the next press: the order goes out with a key, the draft is kept with it, and pressing again places the order if it was not placed and is answered with it either way (D76).
- **The commands are a table in `chat`:** what each is called, and what Telegram's menu says of it. `/orders` lists the person's orders from today on, to look at or to withdraw. `/pending`, `/plan` and `/status` are an operator's, and each asks for the day with buttons.
- **Every request carries `tg-<update id>` as its request id** (D42). So what a chat asked for is found in the API's log, and a plan that was asked for by chat in the log of every solver that worked on it.
- **Private chats only.** In a group, everybody's presses would go into one person's order.
- **Text goes out as text.** No message is given a parse mode, so an address somebody typed is never read as markup.

Why:

- The dialogue is where the bot can go wrong: an order placed twice, or before it was confirmed, or by the wrong person. With no Telegram in it, every step of it is a function of what came in and of what the API answered. A test says what a person sends, and presses the buttons the bot made, by their labels.
- A draft in memory is what `docs/plan.md` asks for, and all that is lost with it is an order that was not finished. Everything that was finished is in the API.
- The id of the message that began a draft is unique in its chat, and is there without the bot having to keep count from one start to the next.
- The commands are read in `chat` and not by teloxide's derive. One table then serves the reading, the help and the menu, and a test sends `/order 50.45 30.52` as a person does. The workspace no longer asks teloxide for its `macros` feature.
- Typed coordinates were added by the project owner's choice on 2026-10-12. Not every client of Telegram can share a location, and they let the bot be tried without a phone.

What it does not do: understand sentences (D58); or show more than ten orders at once. Until Phase 10 it could not tell an order that was not placed from one that was placed and lost on the way back. That took a key that makes the request safe to repeat, which it sends now (D76). An address in words became a way to begin an order in Phase 8, for a bot that was given a service to ask where it is (D72).

Tests: the order of the phase, press by press, with nothing asked of the API before the last one; dropping it at each step; buttons that are out of date, another chat's, or made up; days that have passed; every command for a client and for an operator; a role that changes between two requests; the API away, refusing, failing, and silent after an order went out. How the three parts are tried together is D64.

### D62. How the bot starts, waits and stops (in place)

- **Without `TELEGRAM_BOT_TOKEN` the bot idles.** It says that it has no token and where one comes from, and waits to be told to stop, as the stub did (D56).
- **With a token it needs `BOT_SERVICE_KEY`,** the key the API has (D60). Without it, or with an address that is none, it ends at once and names the setting, as the API does (D47).
- **It asks Telegram for its updates,** by long polling, ten seconds at a time. So Telegram needs no way to reach it. It asks for messages and for the presses of buttons, and for nothing else.
- **A Telegram that cannot be reached is tried again,** after a pause that doubles from one second to thirty, as a solver waits for its database (D37).
- **A token that Telegram refuses ends the process,** with the reason and a status that says so.
- **Told to stop, it tells Telegram which updates it has had** and ends, within eight seconds at the most.
- **"Today" is the day of its clock,** in the zone its machine is set to. Compose passes `TZ` on, and a container without it is on UTC.

Why:

- teloxide's `dispatch()` panics when its first request to Telegram fails. That would be a panic for a network that is not up yet, and for a mistyped token alike. The bot asks Telegram who it is itself, and tells the two apart: one is waited out, and the other is for a person to mend.
- A refused token is not waited out. Nothing the bot does will make it good, and a process that ends has said why (D47).
- teloxide's handler of Ctrl-C hears Ctrl-C and no SIGTERM, and SIGTERM is what `docker stop` sends. The first process of a container is not ended by a signal it does not listen for (D35, D51, D56). So the bot listens for both, as the API and the solver do, and stops teloxide's dispatcher through its shutdown token. The workspace no longer asks teloxide for its `ctrlc_handler` feature.
- An update that is not acknowledged is sent again. Stopped without that, the bot would answer the last message twice: once now, and once when it is started again.
- TLS is rustls with the roots it trusts compiled in: teloxide's `rustls` feature is reqwest's `rustls-tls`, which brings `webpki-roots`. So the image still has no certificates of its own, and nothing native is built (D12, D38).
- The client that asks Telegram is built by the bot, with the settings teloxide gives its own. Built inside teloxide, a failure to build it would be a panic.

Neither the token nor the key reaches a log. The settings that are logged hold neither, and the type that holds them has no `Debug`. teloxide's `Bot` has one that shows the token, so the bot never formats it, and teloxide takes the token out of the errors it reports. An address that carries a password is refused as a setting, and is not repeated.

Good to know:

- **One bot for a token.** Two that ask for updates with the same token are refused by turns, with a `409`.
- **teloxide waits ten seconds over a `5xx` of Telegram's** before it hands the error on.
- **teloxide writes the name of a method as `GetMe`** where Telegram's documentation writes `getMe`. Telegram takes either, and whatever stands in for it has to as well.

Tried in containers, in a project of its own: the stack was up nine seconds after `docker compose up`, with the bot idling. The idle bot had stopped 0.4 seconds after it was told to. With `TZ=Europe/Kyiv` it said that its day was `2026-10-12 +03:00`. With a token that was made up, it reached `api.telegram.org` over TLS from an image that has no CA certificates, was told that there is no such bot, and ended with status 1 and the reason, 2.8 seconds after it was started.

### D63. An order announces itself (in place)

Decided with the project owner on 2026-10-12: an order that is placed by chat is to be seen on the map as it is placed.

- **In the database.** `orders::create` and `orders::cancel` notify a channel of their own, `routeforge_orders`, in the statement of the change. The payload is the day of the order, written as `2026-10-12`, and nothing else.
- **In the API.** The one connection that listens (D50) takes this channel as well. The stats stream passes the word on as an `orders` event, an `OrdersChanged` with the day. The stream of a plan ignores it.
- **On the page.** A page that shows that day reads its orders again. It also reads them when its stats stream opens again after a break, and every thirty seconds while the stream is open.

Why:

- Sent with the change, the word is heard when the order can be read, and never for one that was rolled back (D48). A withdrawal that changed nothing says nothing.
- The day alone: the notifications of one transaction for one day are then equal, and PostgreSQL folds them into one (D26). The two thousand orders of a demo day are one word. The day is also all a page needs, to know whether the word is for it.
- `to_char` writes the day, and not a cast to text, which follows the `DateStyle` of the session that placed the order.
- The stats stream, because it is the one stream an operator's page always has open. A stream of its own would be one more connection for every page, and one more endpoint.
- The event names the day and no order, so it tells an operator nothing they may not read anyway. Whoever hears it reads the orders, as whoever hears of a plan reads the plan (D48).
- A notification is a hint (D26), and nothing in the stream makes up for one that was lost: the stream holds no orders to compare with. So the page reads now and then without being told, as the streams do (D50).

What it does not do:

- What a plan does to an order, taking it or giving it back, is the plan's to announce, and the page reads the orders again when its plan changes.

What it did not do until Phase 10:

- A client's page had no stream (D41), and showed a new order after Refresh. It has one of its own now, which says when something has become of an order of the client's, and nothing of anybody else's (D77). A repeat of a request that places nothing announces nothing either (D76).
- A plan that was asked for elsewhere, by chat for one, showed on a page after Refresh. The stats stream names every plan that changes now, and a page that shows the day of a plan it did not know reads its plans again (D79).

Measured in a headless Edge, against an API on the host: an order that was placed through the API with a Telegram user's token was on the map 0.37 seconds later, and its withdrawal after 0.37 seconds. An order of another day caused no read. An order that was stored behind everybody's back, by an `INSERT` that announces nothing, was shown 26 seconds later.

Tests: an order is announced once, at the commit and not before it, and not when it was rolled back or when nothing changed; forty orders of two days in one transaction are two words; the day reads the same in a session that writes its dates as `12.10.2026`; none of it reaches the channels of the workers and of the plans; the stats stream passes on a placement, a withdrawal and a day of demo orders, each once; the stream of a plan stays quiet.

### D64. The bot is tried against stand-ins, and its requests are held to the API's document (in place)

What can be shown without Telegram is shown without it, so the bot's tests need no network and no token:

- **The dialogue against a fake of the API.** `FakeApi` implements `Api`. It notes what it is asked, and gives a new token at every login, so a test can see which login a request came of.
- **The requests against a server that knows the API's document.** A wiremock server answers each operation at the method and the path that `crates/routeforge-api/openapi.json` gives it, found by the id of the operation, and nowhere else. A request the API would not know is not answered there either.
- **The whole bot against a stand-in for Telegram.** A second wiremock server speaks the Bot API as far as the bot uses it. It hands out the updates a test gave it, from the offset on that it is asked for, as `getUpdates` does, and takes what the bot sends. The bot runs against it and against a stand-in for the API as it runs against the real ones: with teloxide's dispatcher, and over HTTP.
- **The binary itself,** run by a test: it idles without a token, names the setting it misses, and with its settings talks to the Telegram and the API they name.

Why:

- The bot may not link the API, and the API's tests cannot run the bot (D3). The document is the contract between the two (D43), and it is committed. With the bot's requests held to it, a path that changes in the API fails a test of the bot's.
- The part that turns updates into events can only be wrong about Telegram. Updates as Telegram sends them are the one thing that tries it, read by teloxide's own types.
- Nothing else comes in through `main`, which is what the tests of the API's binary are there for too (D55).

What a stand-in cannot show: that Telegram takes what the bot sends. A keyboard that is shaped wrongly, a text too long for a button, a method that Telegram answers otherwise than teloxide expects, would all pass here and fail there.

Found while it was written: teloxide asks for `GetMe` and not for `getMe` (D62). The first stand-in answered the second alone, and every test of the whole bot waited for an answer that never came.

### D65. What a courier has on board is followed along the route (in place)

Replaces D7. Decided with the project owner on 2026-10-12.

A courier leaves the depot with everything it is to deliver. A delivery takes its parcels off at its stop, and a pickup puts its parcels on at its own. What is on board has to be within the courier's capacity at every moment of the route. A van of twenty can deliver twenty parcels and bring twenty others back, as long as it does not pick up faster than it delivers.

- **Clustering.** A courier has room in two ways: for what it carries out, and for what it carries back. A stop fits when it fits the one it asks for (D16). That is all clustering has to know. The stops of a cluster can be driven in some order when the deliveries fit the courier together and the pickups do: with every delivery made before the first pickup, the load falls to nothing and then rises to what was picked up.
- **`Loads`.** What the stops of a route ask for, and what its courier carries, is a value of `routeforge-tsp`. It cannot be made for a route that no order of the stops could drive, so a search that is handed one always has a tour to start from. The matrix has its symmetry the same way (D14). A `Loads` also knows whether any order of its stops could overload the courier at all.
- **The search.** `optimize_route_within` is the search of D15 to D18 with one rule more: it never takes up a tour that overloads the courier.
  - Nearest neighbour goes to the nearest stop the courier can call at next. It always gets to the end: a delivery can be called at whenever, and when pickups alone are left, what is on board is what was picked up.
  - 2-opt and Or-opt take the best of the moves that shorten the tour and keep it within the capacity.
  - Annealing draws for a move as it did. A move it would take is dropped if the tour would overload the courier with it.
  - A move is tried for its length first, as before. Only one that passes is checked for the load: it is made on a copy of the tour, and the copy is walked once.
- **Where nothing could overload the courier, the search does not look.** When the deliveries and the pickups fit the courier together, every order of the stops does, and the search is the one without loads, move for move. A route that D7 allowed is such a route. `optimize_route` is that search, and the TSPLIB test and the benchmarks of Phase 1 run it as they did.
- **The solver** makes the `Loads` of a route job from its orders and its courier. Before it stores a route it checks the calling order once more, and a route that overloads its courier fails the attempt and is not stored. When it turns a route round for the roads (D66), it turns only one that can be driven that way round.
- **Nothing is stored for it.** What a courier has on board follows from the stops of a route in their order. The page reckons it for each route it shows, and writes the most beside the courier's capacity (D52).

Why:

- Under D7 a pickup took room from the deliveries although the two are never on board together in full. The same vans now take more. The demonstration's day of 300 orders, four in five of them deliveries, is carried by four vans of 132, where D7 needed four of 165.
- The two sums are exact, and not a safe estimate. A set of stops can be driven if and only if both fit: the courier leaves with all the deliveries, and comes back with all the pickups.
- The order of the stops is the search's to decide, so the search is what has to mind the load. A rule that held clustering to what any order could carry would be D7 again.
- Checked after the length, the load costs something only for a move that is about to be taken. Most are not.
- The solver's check before it stores is the one that counts. A route that overloads a van is the thing the system must not hand out, whatever went wrong before.

What it costs, with `cargo bench -p routeforge-tsp` on the development machine: the whole search for one route, ten million moves, took 194 ms without loads and 195 ms within loads that can overload the courier at 50 stops, and 166 ms and 169 ms at 300. That is less than two runs of the same search differ by.

What it does not do:

- The search never passes through a tour that overloads the courier. Where the loads are tight, a shorter tour that small moves reach only through such a tour may not be found.
- A parcel that is picked up goes to the depot. It cannot be delivered on the same route (D1).
- A courier drives one round a day. Coming back to unload and going out again is not planned. Since Phase 12 a second plan of a day cannot send it out again either (D83).

Tests: the sweep's properties under the two kinds of room, and deliveries and pickups not taking each other's; a tour within the loads at every stage, for random routes and loads; never longer than the best such tour that an exhaustive search finds, for up to nine stops; the same seed giving the same tour; with loads that cannot overload, the very tour of the search without loads; a square on which the shortest tour overloads the courier, 40 against 48. In the solver: a courier that leaves full delivers before it picks up; a day of mixed orders with no route over its courier's capacity; a route that overloads its courier not stored.

### D66. Distances by road come from an OSRM server, and a route says what it was measured along (in place)

A solver that is given `OSRM_URL` asks that server for the distances between the stops of each route. Without the setting the distances are great circles, as they have been since Phase 3 (D8, D29).

- **One request for a route.** The solver asks OSRM's table service for the distances from the depot and each stop to each other, in metres. It asks with a `POST`, so the positions travel in a body.
- **A route that gets no table is searched for on great circles.** That is so when the server does not answer within `OSRM_TIMEOUT_SECS`, twenty seconds by default, when it refuses, when what it answers cannot be read, and when it knows no way between two of the positions. The solver logs a warning with the reason and goes on.
- **A route says what its length was measured along.** `routes.distances` is `road` or `great_circle`, and the API gives it with the route. The page writes `by road` or `as the crow flies` beside the kilometres (D52).
- **The table is waited for with the heartbeat beside it** (D34). The lease is kept however long the server takes, and a plan that is cancelled meanwhile ends the wait within a heartbeat.
- **The search works on the mean of the two directions,** rounded up to the metre. By road the way from A to B is not the way back, and the search needs it to be (D14).
- **The route that is stored is measured as it is driven,** every leg in its direction. The search cannot tell a tour from the same tour the other way round, and by road one of the two is shorter. Both are measured, and the shorter is stored, unless the courier would be overloaded that way round (D65).

Why:

- A route on great circles is still a route. A job that failed for want of a table would be tried three times and then fail its plan (D23), so a service that is optional by the approved description could end a demonstration. With the fallback a plan is done whatever OSRM does, and the label keeps a route by great circles from being taken for one by road.
- The label is the route's and not the plan's, because the fallback is. A plan whose server went away in the middle has routes of both kinds, and says so route by route.
- The mean, and not the shorter or the longer of the two directions: a tour is then as long to the search as the mean of driving it one way round and the other. So what is short to the search is short on the road, whichever way it is driven in the end. Between great circles the two directions are the same and the mean is what they are, so nothing changed for a solver without OSRM.
- `POST`, because the positions of a route of three hundred stops make an address of several kilobytes, which a server may refuse for its length. In a body `annotations` is a list, where in an address it is a word. The server refuses the word there.
- Decided with the project owner on 2026-10-12: the page says what a route was measured along, and draws its lines straight. Lines along the streets would take the geometry of every leg from OSRM, and somewhere to keep it. That was turned round the day after: since Phase 9 a route by road keeps its way, and the page draws it (D73).

What it does not do:

- What a search announces while it runs is the length on the matrix it searches, the symmetric one (D49). The length that is stored is the directed one, so the last announcement and the stored route differ a little.
- OSRM moves every position onto the nearest road it has, and its distances are those of the road it was moved to. A demo order is on a street since Phase 13 (D87), and is moved a few metres at most. An order placed by a click or by the bot may be in a park or on the river as well as at a door.
- A route of more stops than the server was started for is refused by it, and searched for on great circles. The stack starts it for 2000 (D67).

Measured on the development machine, against the server of D67. A table of 50 positions took 0.05 seconds, one of 337 took 0.14, one of 1000 took 0.73, and one of 2001 took 2.56 and was an answer of 28 MB. One of 2002 was refused with `TooBig`.

What a day comes to, in the stack as compose runs it: the demonstration's 300 orders on four vans, seed 7, 200 million moves a route. Measured again in Phase 13, with the orders on the streets within 10 km of the depot (D87).

| | As the crow flies | By road |
|---|---|---|
| The routes that were found on great circles | 226.70 km | 571.61 km |
| The routes that were found by road | 257.87 km | 476.17 km |

- The routes found on great circles are 20 % longer to drive than those found by road. Each driven the shorter way round, which a search on great circles does not know of, they are 533.50 km, and 12 % longer.
- The lengths by road in the right-hand column were asked of the server again, leg by leg, by a script outside the stack. They came to 476.16 km for the 476.17 km that are stored.
- The four routes by road, driven the other way round, come to 521.78 km. Each was stored the shorter way.
- By road the day is two to two and a half times its length as the crow flies. The stops are some 750 metres apart. The Dnipro runs between them. Of the 300 orders and the depot, the server moved none more than 25 metres onto its roads: a demo order is on a street (D87).
- In Phase 8 the day was of random points within 5 km of the depot (D46), and the table was 126.13 and 603.61 km, then 182.05 and 475.92 km. The routes found on great circles were 27 % longer to drive. The stops were some four hundred metres apart, and 66 of the 304 positions were more than a hundred metres from a road, one of them 1.25 km.
- With the server stopped while the first of four routes was searched for, by one solver, the plan was done all the same: one route by road and three on great circles, each labelled. Each of the three waited nearly four seconds for the name of the server to fail.

Tests: the request as OSRM takes it, longitude first; the table read back; each way of failing, by its own name; a table that differs by direction searched on the mean; a route stored the shorter way round, and not turned round when that would overload the courier; a route on great circles stored as the search found it; a route with no table searched on great circles and labelled so; the lease kept while a table is waited for, and the wait ended by a cancel.

### D67. OSRM is a service of the stack behind a profile, on roads that are fetched once (in place)

- **A service with a profile.** `osrm` in `docker-compose.yml` has the profile `osrm`, so `docker compose up` leaves it out. `COMPOSE_PROFILES=osrm` and `OSRM_URL=http://osrm:5000` in `.env` put it in and point the solvers at it.
- **The image is OSRM's own,** `ghcr.io/project-osrm/osrm-backend:v26.10.0-debian`, by its tag. It is started as `osrm-routed --algorithm ch --max-table-size 2001 --max-viaroute-size 2002`.
- **The roads are prepared once, by a script.** `scripts/osrm-prepare`, as `.ps1` and as `.sh`, fetches the extract of Kyiv that BBBike publishes, 34 MB, into `osrm/`, and runs `osrm-extract` with the car profile and `osrm-contract` on it, with the tools of the same image. On the development machine that took 16 seconds, used half a gigabyte of memory at the most, and left 106 MB.
- **`osrm/` is nobody's but the machine's.** Git ignores it, and so does the build context of the images.
- **Nothing waits for it.** It has no health check, and the solvers do not depend on it. A solver that gets no answer falls back, route by route (D66), so the server may come up late, or not at all.

Why:

- A profile keeps the stack as it was for whoever does not ask for roads: one command, no download, and nothing that asks another host (D8, D54).
- The roads are not in the repository and not in an image. They are 34 MB that change every week and belong to OpenStreetMap, under its licence. A script that fetches them says where they are from.
- A city and not a country: the demonstration's orders are in Kyiv (D46), and a city is prepared in seconds on a laptop.
- `ch`, contraction hierarchies, answers a table quickest. Its price is that the whole map is prepared again for any change, which here is never.
- `--max-table-size 2001`: the server's default is a hundred positions, and a route has a position for each stop and one for the depot. The demonstration makes days of up to 2000 orders (D46), and no route has more stops than its day has orders.
- `--max-viaroute-size 2002`, since Phase 9: the way of a route is asked for from the depot through the stops and back to the depot, which is one position more than its table has (D73). The server's default is five hundred.
- The tag ends in `-debian` because that is what the release is published as. `v26.10.0` alone is no image.

Good to know:

- **The extract is a box round Kyiv,** from 30.28 to 30.86 degrees east and from 50.30 to 50.62 north. A position outside it is moved to the nearest road inside it.
- **Stopping it is quick:** the container was gone a second after it was told to stop.
- **A solver outside Docker** finds the server at `http://127.0.0.1:5000`. `OSRM_PORT` moves that port.

### D68. A login is counted, and passwords are hashed a few at a time (in place)

Decided with the project owner on 2026-10-12, as the first of four items of hardening that earlier phases left for Phase 8. The others are D69, D70 and D71.

- **Only so many logins are taken for one address within a minute:** five, unless `API_LOGIN_ATTEMPTS` says otherwise. The next is answered `429` with a `Retry-After`, before anything is looked up or hashed.
- **It counts by the address that was sent,** in lower case, whether it has an account or not, and right password or wrong. A login that succeeds clears the count. A login the API could not check, for a fault of its own, is not counted.
- **Passwords are hashed four at a time,** unless `API_HASHES_AT_ONCE` says otherwise. A login that gets no turn within five seconds is answered `503`, and is not counted either.
- **The counts are in memory,** for at most ten thousand addresses. When there is no room, the counts whose window is over go first, and then one of those with the fewest tries, the newest first. A count that has reached its limit never goes: until Phase 12 the oldest went, and a flood of logins for made-up addresses gave an address that had had its tries a fresh five. When every count is at its limit, a new address waits.

Why:

- Without a limit, whoever knows an address can try passwords for it as fast as the API hashes them.
- Counted before the lookup, a refusal costs no hash, and says nothing about which addresses have accounts: an address nobody has is refused after five tries as one somebody has (D40).
- The right password is counted as well. Otherwise the sixth try would still say whether it was right.
- A hash takes 19 MiB and some tens of milliseconds of a processor (D40), and a login needs no token. A hundred at once would be two gigabytes. Four at a time bound that, and the wait in front of them is bounded too.

What it costs: somebody who knows an address can keep its owner out, for as long as they keep trying. A limit by where the tries come from would not have that fault, and would have another: behind Docker's published port every caller comes from the same address. The counts are gone when the API starts again, and two APIs would count apart.

The bot's endpoint is not counted. It takes the bot's key and no password (D60).

Measured on a release build: five wrong logins were answered `401` in 37 to 137 ms each, and the sixth `429` in 2 ms, with `Retry-After: 60`.

Tests: the count, its window and its bound, by a clock the test holds; the sixth login refused before any hash, for an address nobody has as for one somebody has; the right password refused too, and taken when the time is over; a login that succeeds clearing the count; a login that waits too long for its turn answered `503` and not counted; both refusals in the OpenAPI document.

### D69. The page is served with a content security policy (in place)

`GET /` answers with a `Content-Security-Policy`:

```
default-src 'none'; script-src 'self' 'sha256-…'; style-src 'self' 'sha256-…';
img-src 'self' data: https://tile.openstreetmap.org; connect-src 'self';
base-uri 'none'; form-action 'none'; frame-ancestors 'none'
```

- **The two hashes are of the page's own script and style,** made once, when the page is first asked for, from the bytes that are served.
- **Everything else is from where the page is from:** Leaflet, its style and its images, and the API. The one other host is the tile server, for images alone (D8).
- **Only the page has it.** Swagger UI has inline scripts of its own, and is not ours to change (D43).

Why:

- The page puts text from the API into the document as text (D52). The policy is what holds if a change to the page ever forgets that: a script that was not built into the page does not run, wherever it came from.
- Hashes, and not a file of its own for the script: the page stays one file (D52), and the hash is of exactly what is served, since it is made from it.
- Made by the API and not written down: a hash in the source would have to be changed with every change to the page, and a stale one is a page that does nothing.

What it costs: a `style` attribute and a handler in an attribute are refused like any other inline code, so the page may have neither. It had none. A `<script>` or `<style>` element beside the first of its kind would be refused too: the policy names one of each.

Tried in a headless Edge: the page worked as before, with twenty tiles, four routes and its stream, and an empty console. Leaflet needed no exception. An inline script added afterwards, a script from another host, a handler in an attribute, `eval`, a request to another host and a `style` attribute were each refused.

Tests: the policy on `/`, and none on what the page loads; the hashes those of the page as it is on disk; no `unsafe-inline`, no `unsafe-eval`, no wildcard, and one other host; a page without a script getting the hash of nothing.

### D70. The API says when it runs on the demonstration's values (in place)

When the API starts with the signing key, the admin's password or the bot's key that `docker-compose.yml` falls back to, it logs a warning that names which, and says that they are in the repository for everybody to read.

Why: the stack starts on those values by design, with its ports open to the machine alone (D54). Whoever opens a port wider has to set their own, and until Phase 8 nothing said so to a person who did not read `.env.example`. A warning costs a demonstration nothing. Refusing to start would break the one command.

The values are constants of the API, and a test holds them to the compose file: it reads the `api` service, and each fallback there has to be the constant. So neither can change without the other.

### D71. A depot can be read by its id, and a plan is drawn from its own (in place)

`GET /api/depots/{id}` gives a depot by its id, to anyone who is logged in, as `GET /api/depot` gives the newest.

A plan names the depot it was made for (D21). The page reads that depot when it is no longer the current one, draws the plan's routes and its searches from there, and marks the place with a grey dashed ring.

Why an endpoint, and not the depot inside the plan: the plan says which depot already, and all that was missing was a way to read one that is no longer the newest. A depot is no secret from anybody who may read the current one, and its id is not guessable (D19).

Tried in a headless Edge: a plan was made, the depot moved two kilometres, and the plan shown again. Its routes started at the old place, which had its ring, and the new depot stood beside it.

### D72. The bot looks an address up, at a service it is given (in place)

Decided with the project owner on 2026-10-12.

A bot that is given `GEOCODER_URL` takes a place in words. It asks that service where the address is, shows up to three places with a button each, and the press of one begins an order there, which the buttons then finish as any other (D61).

- **Off until it is given a service.** On a clone the bot asks nobody but Telegram and the API (D8), and answers words as it did.
- **The service is one that speaks as Nominatim does,** the search of OpenStreetMap: `GET /search?q=…&format=jsonv2&limit=10`. Whoever names the public one takes on its usage policy, which `.env.example` links.
- **What the bot does for that policy:**
  - It says who it is, by its name, its version and the address of this repository.
  - All its chats together send one request a second at the most. A search that would wait more than five seconds for its turn is told to try again, and is not queued.
  - It asks only when a person has written an address.
  - It shows the places with `© OpenStreetMap contributors`.
- **What is looked up:** a text that is no command, and what follows `/order` when that is no pair of numbers. Not a text that begins with a slash, and not one longer than 200 letters.
- **Near the depot first.** The bot asks the API where the depot is, as the person (D9), and tells the service to prefer what lies within some 55 kilometres of it. When the API does not say, it searches without.
- **A search is in memory,** one for each chat, as a draft is (D61). Its buttons name it by the message that asked, and a press that names another search, or a place this one does not have, picks nothing and ends nothing. A new search takes its place, and `/cancel` drops it.
- **Nothing is begun or placed by words.** What a search finds is shown, and picked by a button. An order is still placed by the press of the confirming button and by nothing before it (D9).

Why:

- Off by default, because what a person writes goes to somebody else's service, and because the public one runs on donated servers and is for light use. Whoever turns it on has read what that takes. For more than a few people the answer is a Nominatim of one's own, which is the same setting.
- Behind a trait, `Geocoder`, as the API is behind `Api`: the dialogue is tried against a fake, step by step.
- Ten places are asked for and three are shown. One address is several things on OpenStreetMap's map, each of its buildings and each shop in them, and several come back under one name. The bot shows the first three that differ by name.
- The box round the depot is a preference and no limit. A street has its name in many towns, and the one in the depot's town is the likely one.
- The pace is the bot's and not each chat's: the policy counts an application, all its users together.

What a person wrote goes to the service and nowhere else. reqwest names the address it asked in its errors, and here that address ends in the words, so it is taken out before a failure is logged. No header says whose address it is.

What it does not do: understand a sentence (D58), complete an address while it is typed, which the policy forbids, or remember a place from one search to the next.

Tried against the public service, with eight requests in all, a second apart or more. `Хрещатик 22` near the depot gave the address, the post office in it and one more thing that is at it. Without the depot it gave the same address first. `Соборна 10` gave that address in three towns round Kyiv. A line of letters gave nothing. The first try showed that one address comes back three times under one name, which is why the bot now asks for ten.

Tests: the dialogue against a fake of the service and of the API: one place and several, none, a press that is out of date or made up, a new search in the place of the old, the service away and busy, the API not saying where the depot is, what is and is not sent, more places and longer names than a message takes, and the bound on what is kept. The requests against a server that answers as Nominatim does: the query, the name the bot gives, the pace, a search that would wait too long, places under one name, and the words kept out of what is said of a failure. The whole bot against stand-ins for Telegram, the API and the service (D64).

### D73. A route by road keeps the way it takes, and the map draws it (in place)

Turns round a choice of 2026-10-12, when the project owner took a label on the route over lines along the streets (D66). The lines were asked for on 2026-10-13, as part of Phase 9.

- **The solver asks for the way.** When the search for a route is over, and it is settled which way round the route is driven (D66), the solver asks the route service of the OSRM server for the way from the depot through the stops in that order and back. That is one request for a route, as the table is.
- **Only a route that was measured by road has a way.** A solver without an OSRM server asks nobody. A route that fell back on great circles asks for none either: its length is the straight lines from stop to stop, and so is its way.
- **A route that gets no way is stored without it.** A service that does not answer within `OSRM_TIMEOUT_SECS`, that refuses, or that answers what is no line is a warning in the solver's log and nothing more. The route is still `road`, and the page says that it is drawn from stop to stop.
- **The way is waited for with the heartbeat beside it** (D34), as the table is. The search is over by then and the route is not stored yet, so a lease that ran out there would have another worker search for the whole route again.
- **It is asked for with `continue_straight` off,** which makes it the way that was measured. More on that below.
- **It is stored with the route,** in `routes.path`, as text: an encoded polyline with six decimal places, latitude first, which is how OSRM writes it. A check of the schema lets only a route by road have one.
- **The API gives it with the route,** as `path`, and `null` where there is none.
- **The page draws a stored route along its way,** and a route without one from stop to stop. A search under way is a dashed straight line, as it was (D49).

Why:

- The solver and not the API: the solver is the service that asks OSRM and is given `OSRM_URL` (D66, D67). Asked for by the API when a page wants a line, the lines would be there only while the server is, the API would have its first service to ask, and a way would be asked for with every look at it.
- With the route and not in a table of its own: the way is of this route in this order. It goes when the route goes, with its plan (D20, D30).
- `continue_straight` off, because the table measures every leg by itself, whichever way the courier faces at its ends. For cars the route service makes a way go on from a stop in the direction it came to it, unless it is told otherwise, and that is another way and a longer one. Asked both ways for the same stops in the same order, against the server of D67:

  | | Legs longer than the table has them | The longest by | The whole way |
  |---|---|---|---|
  | 75 stops, the service's default | 26 of 76 | 1.3 km | 1.1 % longer |
  | 330 stops, the service's default | 153 of 331 | 2.2 km | 1.6 % longer |
  | Either, with `continue_straight` off | none | 0.1 m | the same to the metre |

- An encoded polyline and not a list of positions: a point of a way takes four letters so, and some forty as two numbers in JSON. Six decimal places are a tenth of a metre. The type that holds one, `Polyline`, decodes it when it is made, as `LatLon` checks its numbers. So what is stored and what is sent is a line, whatever the service answered.
- The whole way and not the outline the service gives unasked: the outline is made for a picture of the whole route, and cuts the corners that show when the map is looked at close up.
- A search under way is not drawn along the streets. It announces its best route four times a second (D49), and each announcement would be a request to the server.
- `list_for_plan` reads a row for each stop of a route. The way is selected with the first row of its route alone, so that a route of three hundred stops does not carry its way three hundred times.

What it takes, measured against the server of D67 on the development machine in Phase 9, on days of random points within 5 km of the depot (D46):

- **Time.** Next to nothing beside the table. By the server's own log, in the stack: for a planned day of 300 orders on four vans the four tables took 19 to 29 ms and the four ways 5 to 9 ms. For a day of 2000 orders, with routes of 347 to 556 stops, the tables took 94 to 216 ms and the ways 24 to 30 ms. A way through stops in no order at all is longer and takes longer: 71 ms through 330 of them, and 155 ms through 500.
- **Room.** A way has a point every 35 metres or so, and a point is four letters. For the day of 300 orders, which came to 505.55 km by road, the four ways had 2067 to 4848 points and were 8 to 19 thousand letters, and the answer of `GET /api/plans/{id}/routes` was 108 kB. For the day of 2000 orders the ways had 12 to 17 thousand points each, and the answer was 517 kB.
- **The limit.** Left to itself the server takes a way through 500 positions, and refuses a longer one with `TooBig`. The stack starts it with `--max-viaroute-size 2002`: a table has the depot and the stops, and a way comes back to the depot (D67). Three of the four routes of the day of 2000 orders had more than 500 stops, and each got its way.
- **The line against the length.** Measured from point to point along great circles, each of those eight ways came to within 0.21 % of the length its route is stored with, which is OSRM's own measure of the same way.

Tried in the stack, from a clone of the phase branch, and in a headless Edge:

- **The lines.** The stored routes of a day by road followed the streets, and crossed the Dnipro on its bridges. Close up, a stop that lies off the roads has a short spur to it and back.
- **A server that gives the distances and refuses the way,** started for a way through ten positions. The plan was done in 1.4 seconds. Its four routes were stored by road without a way, each solver logged a warning with the server's `TooBig`, and the page said `by road, drawn from stop to stop` of each.
- **The server stopped three seconds into a plan,** which one solver worked on with six seconds a route. It was gone a second later. The plan was done after 40.2 seconds: the first route by road, without its way, and three as the crow flies. The page said `219.79 km over 4 routes: 1 by road, 3 as the crow flies`. Each time the server was asked and was not there, the solver waited four seconds for its name to fail, as in Phase 8 (D66).

What it does not do:

- A way begins and ends on the road nearest the depot, and passes each stop on the road nearest to it (D66). The depot of the demonstration is 23 metres from its road.
- A way is asked for once. A route that was stored while the server was away stays straight until its day is planned again, and so does every route that was stored before this.
- Nothing says how far along its way a route has got to each stop. The length of the legs is not stored, only that of the whole.

Good to know:

- In the body of a `POST`, OSRM takes `steps`, `continue_straight` and `skip_waypoints` as booleans and refuses them as words, and takes `overview` as a word and refuses it as a boolean. `continue_straight` may also be the word `default`.
- The solver logs the length of the way beside the length of the route when it stores one, as `path_m` beside `distance_m`. Two that differ by more than a fraction of a percent are a line of another route.

Tests: the format's published example read and written; lines of random positions read back as they were written; text that is no line refused, in JSON as well; a route stored with its way beside routes without one, and each read with its own; the check that keeps a way off a route on great circles. The request as OSRM takes it, longitude first; each way of failing, by its name; the first route of an answer; fewer than two positions not asked about. A route by road stored with the way of the order it is driven in, on a ring that is driven one way and on the same ring driven the other, of which one has the search's tour turned round; a route stored by road without a way when the service gives none; a route on great circles asking for none; the lease kept while the way is waited for, and the wait ended by a cancel; the compose file starting the server for a way one position longer than its longest table; a route read through the API with its way.

### D74. The vans on the map are a simulation, and each page reckons its own (in place)

Decided with the project owner on 2026-10-13. Three other ways were offered and not taken: reckoning from the clock of the database, positions sent in through an endpoint by a script that plays the couriers, and a simulator inside the system that writes positions for the streams to pass on.

- **A van for each stored route of a plan that is done.** It drives along what the route is drawn with: the way along the streets where the route has one, and the straight legs where it has none (D73).
- **Each page moves its own.** Where a van is, is reckoned from the route as it is stored and from the time since the vans set out on that page. The API is asked nothing for it, nothing is stored, and no clock is compared with another.
- **They set out when the page sees the plan done.** For a page that watched, that is when the last route is stored. For a page that opens the plan later, or is told to read it again, it is when the plan is shown.
- **One speed for all,** at which the longest route of the plan takes a minute. A van with a shorter route is back sooner.
- **A button stops them, and sends them out again.** Where the browser is asked for less motion (`prefers-reduced-motion`) they do not set out by themselves, and the button is how they start.
- **They are called simulated** where they are shown, and in the README.

Why:

- The approved description has live GPS tracking of couriers among what the project will not do. A van that is moved for show does not change that, as long as nothing reports where a courier is and nothing keeps it. An endpoint that takes positions would be the first half of tracking, whoever sends them.
- Each page for itself, and not from the clock of the database, because it needs nothing new and has nothing to go wrong. Two pages that watch a plan hear that it is done within a fraction of a second of each other, and so their vans agree. What is given up is a page that is opened later: it starts the vans at the depot, where a shared clock would show them on their way, or back. For that the API would have to say how long ago the plan was done, as a span and not as a time (D6), and the plan's stream would have one event more.
- A minute for the longest route, and not a speed in kilometres an hour. By road the routes of the demonstration are four times as long as on great circles. At forty kilometres an hour and a minute for an hour, the one day would be over in under a minute and the other would take four.
- The time is read when a picture is drawn. A tab that was hidden draws none, and shows the vans where they have got to when it is looked at again.
- A van is a disc in a pane of its own above the depot, drawn by Leaflet in SVG like the lines. Nothing in it needs an exception from the policy of the page (D69).

What it does not do:

- A van does not wait at a stop, and nothing says which stops it has called at. A sheet that follows its van was offered on 2026-10-13 and not taken. It would need the length of every leg of a route, which is not stored (D73).
- Refresh, and picking the plan again, send the vans out again from the depot.
- A plan that is not done has no vans, and a discarded one loses them.

Tried in a headless Edge, on the host and in the stack. The vans were on the map 35 ms after the page showed its plan as done, with nobody pressing anything. At three moments of a drive by road, each of four vans was within 1.1 pixels of the line of its route, which Leaflet draws a pixel off at the most. The van with the shortest route, 53.60 km, was back first, while the one with 183.44 km had most of its way before it. After a minute all were back, and the page said so. The button stopped them and sent them out again, a sheet that was opened meanwhile stayed open, and a discard took them off the map. In a browser that was asked for less motion nothing set out until the button was pressed.

### D75. The chart of a search and the route sheet are the page's (in place)

Both are made of what the page has already, and the API was not changed for either.

- **The chart.** Each `progress` event of the plan's stream says how long the best route of a search is by then (D49). The page keeps those lengths for each route job, and draws them in the route's row as a small staircase: a length holds until the search announces a shorter one.
  - Across is the share of the plan's budget that is used up: of its time if it has one, and of its moves otherwise. So the line reaches the right edge as the search ends.
  - Up is the length, from the shortest so far to 2 % above it. What is longer runs off the top, and the row says the first length in words.
  - It is the length as the search measures it. By road that is a little off the length the route is stored with (D66), and the number beside the chart is the stored one.
- **The route sheet.** A stored route has a button that opens it as a list: the depot with what the van leaves with, each stop with its number, whether it is a delivery or a pickup, its address or its position, the parcels that come off or on, and what is on board after it, and the depot again with what the van brings back.
- **The map and the sheet point at each other.** A click on the line of a stored route opens its sheet, and the line of the open sheet is drawn thicker. A press on a row of the sheet moves the map to that stop and puts a ring round it. This is the one thing in Phase 9 beyond its list, taken by the project owner on 2026-10-13.
- **The list of routes is no longer written anew with every event.** A row is replaced only when what it says has changed.

Why:

- Nothing stores what a search announces (D49), and the chart does not change that. So a page that did not see a search has no chart of it, and a page that came in the middle has the rest. The lengths are kept while the page is logged in, so that Refresh and a look at another plan do not lose them.
- The scale of the chart. A route loses a fifth of its length in the first quarter of a second and a hundredth or two in all the seconds after (D49). On a scale from the first length to the last that is a cliff and a flat line. With 5 % above the shortest, the fall that goes on took the lowest third of the chart or less. With 2 % it takes up to four fifths.
- The same share for every route, and not each chart stretched to its own fall: so the charts of a plan can be compared, and a route that gained nothing shows a flat line and not a small gain made large.
- The loads of the sheet and the words `at most 131 of 132 on board` come from one function. The sheet is the sum written out (D65).
- The rows had to stay. While a plan is searched for, the list was written anew up to sixteen times a second, and a button that is replaced between the press of the mouse and its release is not pressed. The same would lose the place of whoever moves through the page by keyboard.
- The chart is SVG that the page makes with Leaflet's own function for it. So the page names no namespace, and the test that it names no host but the tile server holds as it is (D52).

What they do not do:

- The sheet has no distances. The length of a leg is not stored, only that of the whole route (D73).
- A search that was over before its first announcement has no chart: a chart takes two lengths.
- Neither is there for a client, who sees no plan (D41).

Tried in a headless Edge, against an API and solvers on the host. On a day of 300 orders on four vans, the chart of each route fell once, a quarter of a second in, and stood: one went from 146.97 km to 129.49 km. On a day of 1200 orders the charts went on falling for one to six of the ten seconds: one route went from 78.64 km to 63.87 km at once, and to 62.84 km in four steps after. A sheet of 86 stops left the depot with 131 on board and came back with 37, and the most in its column was the 131 its row names. A row brought the map to its stop, which was in the Dnipro: a demo order was a random point then (D46), and is on a street since Phase 13 (D87).

With two workers for four routes, the row of a route that was stored stayed the element it was while the two others were searched for, and its button opened its sheet while the plan was still at it. The rows of the searches were made anew, as they have to be.

In the stack, on the day of 300 orders, the first length the page heard of each search was already its last, and the four charts were level lines. A search announces its first tour within a millisecond or so of starting, and the page opens the stream of a plan after the plan was asked for: which of the two is first is chance. So a row names the first length only where the search has got below it.

### D76. A key makes placing an order safe to repeat (in place)

`POST /api/orders` takes a key of the caller's making, in the header `Idempotency-Key`. Sent again with the same key, the request places nothing, and is answered with the order that key placed.

- **The key** is 1 to 64 letters, digits, dots, underscores and hyphens: the alphabet of a request id (D42). It is a type of the domain crate, `IdempotencyKey`, with no feature, so the bot makes what the API takes.
- **It is optional.** A request without one is an order of its own every time, as it always was.
- **It is the caller's own.** `orders.idempotency_key` is unique together with `client_id` (migration 0008). Two clients who make up the same key have placed two orders.
- **The answers.**
  - `201` with the order, when this request placed it.
  - `200` with the order as it stands by then, pending or planned or withdrawn, when the key had placed it before.
  - `422`, naming the header, when the key has placed another order than the one this request asks for, and when what was sent is no key.
- **It is one statement still.** `orders::place` is the insert of `orders::create` with a conflict that sets the key to what it is already, which returns the row that has it. Whether that row is this request's own is read off its id, against the id that was sent. Only then is its day announced (D63).
- **A key is kept as long as its order.** Nothing ends it.

Who sends one:

- **The bot.** A draft has a key from the moment it is begun, and another whenever it is changed. When no answer comes, the draft stays, with its key, and the bot says that pressing again cannot place the order twice.
- **The page.** The key is of the order as the form asks for it. Asked for again unchanged, it goes out with the key it had (D80).

Why:

- A header, and not a field of the body: `OrderRequest` stays what an order is, and what makes a request safe to repeat is said of the request. `Idempotency-Key` is the name such a header commonly has.
- A conflict that updates, and not `DO NOTHING`: the trap of D59. `DO NOTHING` returns no row to the request that came second. The update takes the lock of the row, so a second request that arrives while the first is on its way waits for it. If the first commits, the second is given its row. If it is rolled back, the second places the order.
- By the id, and not by `xmax`: the application makes the ids (D11). So it knows its own row without asking after a column of PostgreSQL's bookkeeping.
- `200` for a repeat: whoever asks can tell that nothing was placed this time. The bot and the page take either for the order.
- `422` for a key that has placed another order: answered with that other order, whoever asked would take this one for placed. The bot and the page cannot come to it, since each changes the key with the order. It is for a caller with a fault.
- A text that is no key is refused, and not passed over: whoever sent it takes the request for one that is safe to repeat.
- For each client: a key somebody else has used would otherwise be a key that is taken, and to be told so is to be told something of somebody else's orders.
- The bot does not ask again by itself. One press is one request (D9, D61), and each request waits up to ten seconds for its answer: a person would wait half a minute in silence where a press settles it.
- Kept for good: there is no clock to compare and nothing to clean up. It costs a column and an entry in an index for each order that has one.

What it does not do:

- Make asking for a plan safe to repeat. A day has one plan in progress, so the second request is refused anyway (D30).
- Outlive the bot. A draft is in memory (D61). A bot that was stopped between two presses has lost the draft, and its key with it.
- Say that an order is the same as one without a key. Two requests without a key are two orders.

Tests: a key places once, and every repeat is answered with the same order, also when it was withdrawn or planned since; four requests at once with one key are one order, and one that waits for a request that is then rolled back places the order itself; the same key from another client is another order; a repeat announces nothing; the schema refuses an empty key and one of 65 letters; the API answers `201`, then `200` with the same body, and `422` for each of five ways an order can differ, and for each of five texts that are no key; the gate comes before the key; the bot sends its key in the header the API's document has for it (D64), with the same key on the second press whether the first request was lost on the way there or on the way back, and with another key for an order that was changed.

Tried in a headless Edge, in the stack: the answer to an order was failed in the browser after the API had sent it. The page said that the order may or may not be placed, and kept it in the form. Another page showed that it was placed. The second press was answered `200`, and the day had the order once. And by hand, with `curl`: `201`, then `200` with the same id, `422` for another order under that key, and one order.

### D77. A stream says when something has become of one's own orders (in place)

`GET /api/orders/events?service_date=` is a third event stream (D50), for anybody with a token. It sends an `orders` event, an `OrdersChanged`, when it opens and whenever something has become of an order the caller placed for that day: it was placed, here or elsewhere, or withdrawn, a plan has taken it, its route was stored, or a plan has given it back.

- **It says nothing of anybody else's orders.** No signal is passed on as it comes. An order of the day that was placed or withdrawn, any plan that has changed, and the word that something may have been missed are each a reason to read. The read is of the caller's own orders of the day and of where they are on their routes (D78). The event goes out only when that differs from what was read last.
- **The event is the one the stats stream has** (D63). It names the day and no order, and whoever hears it reads the orders of the day again.
- **It reads without a reason every ten seconds,** as the stream of a plan does. So a word that was lost costs ten seconds, and a page that reads this stream needs no reads of its own beside it.
- **It is the caller's own, whoever the caller is.** An operator who opens it hears of the orders they placed themselves. Everybody's are on the stats stream.

Why:

- A notification names a day, or a plan, and nobody (D48, D63). Passed on as it comes, it would tell every client who watches a day that somebody has placed an order for it. So the stream has to read, and has to keep what it read, to know whether what changed was the caller's.
- Any plan is a reason to read, and not only the plan of the day: a notification of a plan names the plan and not its day, and to look the day up would cost the read that looking at the orders costs.
- What it keeps is each order's id and status, and where the planned ones are. That is all that can change of an order in a way its client sees. Which plan holds an order is not among it: to its client the order is planned, whichever plan that is by.
- The same event as the stats stream, and not the orders themselves: one handler on the page serves both, and the read that follows is the one the page makes anyway, in pages (D44).
- A stream of its own, and not the stats stream opened to clients: that one shows the queue and the workers, which are an operator's (D41).

What it costs: every stream of this kind reads when any plan changes, at most five times a second each (D50). A read is two queries. Since Phase 12 the one of the orders is a statement of its own, `orders::of_client_on`, served by the index on client and day of migration 0009.

Good to know: a page that picks another day opens the stream of that day, and leaves the one it had. The API finds a stream without a reader when it next writes to it, and a quiet stream writes a comment every fifteen seconds (D50). Until then the stream that was left reads as the others do. Three that one page had left were still open when the API was stopped a little later, and ended with it.

What it does not do: tell a client of a plan. A client sees no plan (D41), and hears only what a plan did to an order of theirs.

Tests: the stream opens with the day; an order placed, withdrawn, taken by a plan, put on a route and given back by a discard are one event each, with silence between them; somebody else's order, placed, withdrawn, planned and put on a route, is silence, and so are an order of one's own of another day and the progress of a search; an operator hears of their own orders alone; an order that was stored behind everybody's back is found by the read that nobody asked for; the stream reads when it may have missed something; it ends with its token, to the second, and when the API is told to stop.

Measured in a headless Edge, in the stack, with a client's page and an admin's side by side. The admin pressed Plan with two seconds a route, and the client's page said where its order was 2.7 seconds later: first that a plan had taken it and its route was still searched for, then `Van 3 has it: stop 1 of 11`. While a third account placed an order for the same day, the client's page showed nothing new and asked the API nothing. With logins good for twenty seconds, the stream ended after 21.0 seconds and the page was at the login a second later. And the API had stopped 1.3 seconds after it was told to, with such a stream open.

### D78. Whoever placed an order is told where it is on its route (in place)

Decided with the project owner on 2026-10-13, as one of four things beyond the list of Phase 10.

`GET /api/orders/stops?service_date=` gives, for each order of the day that is on a stored route, an `OrderStop`: the order, the name of its courier, which call of the route it is, counted from 0, and how many calls the route has. A client gets those of their own orders, and an operator and an admin all of them, as with `GET /api/orders`.

- **An order is on a route from the moment the route is stored.** While the route is still searched for, the order is planned and has no stop.
- **The calls are counted over the whole route,** and not over the rows that are one client's.
- **One query serves the endpoint and the stream of D77.** Since Phase 12 it counts the calls of each route once, for the routes the orders are on, and not once for each order.

This changes what a client may read (D41). Until now a client was told nothing of a route. Now they are told the name of the courier that has their order, its place in the round, and the length of the round in calls. Of the route itself they are told nothing: not its other calls, its line or its length.

Why:

- A list for a day, and not `GET /api/orders/{id}/stop`: the page shows a day, so it is one request, and an order whose route is still searched for is simply not in the list. One by one, each such order would be a `404` in the browser's console.
- Not a field of `Order`: every query of an order would join two tables more, the solver's among them, which reads orders to route them and has no use for where they were.
- The length of the round is no secret of anybody's: it is a number of calls, and no call is named.
- No time: a route has none. The order of the calls is all a plan says (D1).

What it does not do: say where the courier is. Nothing does (D74).

Tests: an order is found on its route once the route is stored, and not before; two clients whose orders share two routes are each given their own, counted along the whole route; everybody's are given oldest order first; an order that is pending, of another day, or of somebody with no orders is on none; a plan that is discarded leaves none; a client and an operator through the API; the day has to be said.

### D79. An operator's page hears of a plan that was asked for elsewhere (in place)

Decided with the project owner on 2026-10-13, as one of four things beyond the list of Phase 10.

The stats stream sends a `plans` event, a `PlanChanged`, whenever a plan has changed, or one of its jobs has, or a plan is new. It carries the id of the plan and nothing else.

- **In the API** the stream passes the notification of D48 on, and reads the queue again as it did before. `Heard::ChangedWith` is that: an event to send on, and a reason to read.
- **On the page** an id that is neither the plan that is shown nor known to be of another day makes the page read the plans of its day again, once, however many words come in a third of a second. If the newest plan of the day is one the page did not know, it is shown, as the newest plan of a day is when the day is opened. An id that is not among the plans of the day is of another day, and is remembered as that until another day is shown.

Why:

- The id alone, as the notification has it (D48). The stream would have to read the plan to say its day, for every word of every plan, and most pages show another day.
- The stats stream, because it is the one stream an operator's page always has open (D63).
- Remembered as of another day: a plan is announced a dozen times while it is made, and each would otherwise be a read of the plans of a day it is not of. On a day that the demonstration plans elsewhere, a page read its plans once for it.

What it does not do: tell a client anything. And a page that shows an older plan of a day, picked by hand, reads the plans of the day again at each word of a newer one.

Measured in a headless Edge: a plan that a script asked for was shown on a page that nobody touched 0.33 seconds later against an API on the host, and 0.37 in the stack, with its routes being searched for.

Tests: the stream names each of two plans that somebody else asked for, and names them again as their jobs are worked off and as one is discarded, with the id and nothing else; an order is no plan; the queue is read again after the plan is named.

### D80. The page takes an account and an order, and lists a client's own (in place)

What Phase 10 gave the page. Two of these were on its list. The others were taken by the project owner on 2026-10-13: that the list and the map point at each other, and that an operator and an admin can place an order by a click as well.

- **An account.** The login has a second button. It opens an account with the address and the password of the form, through `POST /api/auth/register`, and logs in to it.
- **An order by a click on the map,** for whoever is logged in.
  - A click puts a marker there and opens a form: a delivery or a pickup, one to five parcels as the bot offers, and an address for people to read, if somebody likes. The order is for the day that is shown. Another click moves it.
  - A day that has passed takes none. That is the page's rule: the API takes any day.
  - Nothing is asked of the API before the button that places the order is pressed.
- **Placed with a key (D76).** The key is 128 random bits, and is of the order as the form asks for it. When no answer comes, the order stays in the form, and the page says that pressing again cannot place it twice. Pressed again unchanged, it goes out with the key it had. Changed, it is another order and gets another key.
- **What is clicked for itself keeps its click.** The stops, the depot, the lines of the routes and the vans are made with `bubblingMouseEvents: false`. So a click on a route's line opens its sheet, as before, and begins no order under it.
- **"Your orders", for a client:** a row for each order of the day, with what has become of it. A pending one has a button that withdraws it. A planned one says `Van 3 has it: stop 12 of 86`, once its route is stored (D78).
- **The list and the map point at each other.** A row shows its order on the map, with a ring round it. A click on an order marks its row.
- **The streams.** A client's page opens the stream of D77 for the day it shows, and opens it anew when another day is picked. An operator's page takes the `plans` event of D79 from the stats stream.

Why:

- The same form for a client and for an operator: the API lets anybody with a token place an order, as their own (D41).
- No list for an operator. An admin's orders are the demo orders (D46), two thousand of them on a large day. An operator has the routes and their sheets (D75).
- The rows of the list are kept, as those of the routes are (D75). A row is made anew only when what it says has changed, so a button in it is still there when the mouse is let go.
- `crypto.getRandomValues` and not `crypto.randomUUID`: a page has the second only where it was served over TLS or from the machine it is shown on.
- The key is of what the form asks for, and not of the form: an order that was changed after it went out is another order, and the API would refuse it under the old key (D76).
- The click on the map does not move the map. While Leaflet shows a move, it passes over the next one it is asked for (D75).

What it does not do:

- A client sees no route, no van and no sheet (D41).
- An operator has no way to withdraw an order on the page. The API has one.
- A place cannot be typed. The bot takes coordinates, and an address where it has a service to ask (D72).
- When an answer is lost, the order may show in the list of a client while the form still says that it may or may not be placed: the stream has said so. Pressing again says the same, and closes the form.

Found while it was tried: a click on the line of a route left a black box round the whole route. Leaflet listens for the focus of whatever has a tooltip. To the browser a line that is listened to like that is one a click gives the focus to, and it draws the outline of the focus as a box. That was so since Phase 9, which made the lines something to click. The page takes that outline off. Nothing on the map is reached by keyboard.

The page has no tests of its own (D52). How it was tried is in `docs/plan.md`. Since Phase 12 a script tries it in a browser (D86).

### D81. The services share their plumbing through a feature of the domain crate (in place)

Decided by the project owner on 2026-10-14, from the review of that day (`docs/review.md`, finding Q2).

`routeforge_domain::service`, behind a feature of its own, `service`, has what the API, the solver and the bot do alike and have nothing to do with their work:

- `init_logging`: lines for people, or JSON with `LOG_FORMAT=json`, filtered by `RUST_LOG`. A `RUST_LOG` that cannot be read is said to be so, once there is a log to say it in, and `info` is logged in its place (finding Q5).
- `told_to_stop`: Ctrl-C, and SIGTERM where there is one (D35, D51, D62).
- `Backoff`: the pause before the next try, a second at first and twice as long each time up to half a minute, started again by a success. The solver waits so for its database and its listener (D37), the API's listener for its connection (D50), and the bot for Telegram (D62).

All three enable the feature. The bot enables it without `db` and without `openapi`.

Why:

- Each binary had a copy of the first two, word for word, and four loops doubled a pause in four ways of their own. A change to one had to be made to the others by hand, and the comments said as much.
- In the domain crate, because the workspace has five crates and a helper goes into one of them. Behind a feature, because the domain crate is otherwise plain data, and tokio's signals and a log subscriber are no business of a type.
- Not into a crate of the bot's or of the API's: the three share it, and none of them may depend on another.

`tracing-subscriber` is a dev-dependency of the API and the solver, for their tests that read the log, and the bot no longer has it.

The bot's two ways of writing an error with its causes are one as well, `api::causes`. The geocoder takes the address out of the error before it hands it on, as before (D72).

Tests: what `RUST_LOG` gives when it is set, empty, missing and mistyped; a pause that doubles to its limit and starts again; the pauses the services wait.

### D82. A job that panics fails its attempt, and a solver that loses a worker ends (in place)

From the review of 2026-10-14 (finding R1).

- **A job's future is polled through `catch_unwind`** (`worker::guarded`). A panic anywhere in a job, and not only on the blocking thread of a search, is an error of the attempt, with what the panic said. It goes to `queue::fail` like any other error (D37).
- **The job is dropped where it stood,** its heartbeat and its search with it, as when the worker is told to stop (D34). It is not polled again after it panicked.
- **A worker that ends without being told to has been lost** to a fault outside its jobs. `worker::run` tells the other workers of the process to stop, they give their jobs back (D35), and the solver ends with the reason and a status that says something went wrong.

Why:

- Until Phase 12 a panic in a job's own code ended the worker's task. The job waited out its lease for the next worker to panic over, nothing took the worker's place, and a solver whose last worker had gone ended with status 0. A job that panicked on some input took a solver down with each of its attempts.
- A failed attempt is how the queue already bounds a job that cannot be done (D23). With the panic as its reason, the plan says why it failed.
- A process that has lost a worker is not what it was started as. Ending it says so, where going on would say nothing (D47). Starting another worker in its place could start one fault after another.

What it does not do: start the solver again. The stack still has no restart policy (D54). A solver that ends is ended, and `docker compose ps` shows it with its status.

Tests: a route job whose distances panic fails each attempt with the panic's words, and the one worker there is takes all three attempts and stops when it is told to; a worker that panics, or that stops by itself, ends the others and the process with the reason; workers that are told to stop end it well.

The decision in `main`, to end with the error `run` gives, is a line that no test runs: the solver's binary is not run by a test, and a worker cannot be made to be lost from outside.

### D83. A date has one plan that stands (in place)

Decided by the project owner on 2026-10-14, from the review of that day (finding A1). Changes D20 and D30.

A plan stands while it is in progress and once it is done. A date has one at a time: a plan for a date that has a plan that is done is refused, with a `409` that says to discard it first. Once discarded, the date is planned afresh, with every order of it (D30). A plan that failed or was cancelled has undone itself and stands no more (D20).

- **In the database** `plans::create` locks the plans of the date, in the order of their ids, and inserts the new one only if none of them is done. In one statement. The plan in progress is refused, as before, by the unique index of D20.
- **The lock** is what makes the rule hold when the last route of a plan is stored in the very moment another plan is asked for. Without it the request would see the plan as the plan in progress it was a moment before, wait for its row to be written, and then go in as a second plan beside a plan that is done.
- **On the page** Plan cannot be pressed for a day whose plan is done, and says why. The bot shows the API's words.

Why:

- A plan that is done gives its couriers their round of the day. A second plan of the same day was offered every active courier at its whole capacity, knowing nothing of the first, and so gave each of them a second round. The model says that a courier makes one round a day (D1, D65), and the page shows one plan of a day at a time, so an operator did not see it.
- Refusing is the smallest of the three ways that were offered. The others were to offer a later plan only the couriers that have no route that day, and to call a later plan a second wave.
- In the database and not in the handler, so that no way to a plan goes round it.

What it costs: an order that comes in after the day was planned is planned by discarding the plan and planning the day again, every order of it. The demonstration's scripts did that already (D57).

Tests: a done plan's date refused, by `create` and by `submit`, with nothing left behind; a cancelled plan's date planned again; a date planned once its plan is discarded; a plan asked for while the last route of the date's plan is stored waits for that plan and is refused; through the API, the `409` with its words, and the date planned after a discard.

### D84. The live view counts the jobs of the last day, and forgets the workers of long ago (in place)

Decided by the project owner on 2026-10-14, from the review of that day (findings P1 and R2).

- **The stats** count the jobs that are queued and running, and those that ended within the last day. `queue::depth` takes the span, and the API asks for a day. Until Phase 12 the three counts of jobs that ended were of every job there ever was.
- **An index on when a job ended**, migration `0009_indexes.sql`, serves the count, whatever the history.
- **A solver that starts takes the workers that were not seen for a day out of the registry** (`workers::forget`). The queue never reads that table (D27), so a row can go at any time.
- **Nothing else is deleted.** The jobs stay, for the plans they belong to.
- **The page** writes "In the last day" before the three counts.

Why:

- The stats are read by every open page of an operator every two seconds, and whenever a plan changes (D50). Counted whole, their cost grew with the history and not with the load.
- What an operator looks at is the day's work, and a count that only grows says little about it.
- Not by deleting old jobs: the page builds the rows of a plan from its jobs (D75), so an old plan would have opened with no route in its list.
- Each start of a solver registers its workers under new ids, so the registry gained rows that nothing would ever take away.

Tests: the depth counting the jobs that ended within the span, and the queued ones however long ago; through the API, a job that ended counted, and a day later not; a worker unseen for a day forgotten, by the registry and by a solver that starts.

### D85. One person holds only so many event streams open (in place)

From the review of 2026-10-14 (finding S3).

An event stream takes a place among those of its user when it opens, and gives it back when it is dropped, however it ends (D50). `API_STREAMS_PER_USER`, sixteen unless it is set, says how many places a user has. One more stream is answered `429`, with `Retry-After` as long as it takes a stream whose reader has gone to be noticed: the next comment finds it gone.

- The place is taken before anything is read, and a stream that is refused for a reason of its own holds none.
- It counts the streams of every kind together.
- The page needs no change. It opens a stream again after a pause that grows, whatever it was answered.

Why:

- Every stream reads the database when it opens, every few seconds, and whenever a plan changes, through the ten connections that every request shares (D47). Anybody who reaches the port can open an account (D80), and nothing bounded what an account held open. A few thousand streams would have had every other request wait for a connection, and fail after five seconds.
- Sixteen: a page holds two or three, and one it has left is let go only when the next comment finds its reader gone (D77), within fifteen seconds. So there is room for a few pages and for a page whose day is changed a few times.
- The place is a value the stream owns, given back in its `Drop`. So no way a stream ends can keep it, which is the rule of D34 and D50.

What it does not do: bound how many accounts are opened, or how many streams all of them hold together. Both are among the README's known limits.

Tests: the setting, its default and its bounds; a user's places taken, given back with a stream that is dropped, and kept apart from another user's; through the API, a stream over the limit refused with the reason and when to come back, streams of every kind counted together, and a stream refused for a plan that is not there holding no place.

### D86. A script tries the page in a browser (in place)

Decided by the project owner on 2026-10-14, from the review of that day (finding Q1). Changes D52.

`scripts/page-check.mjs` drives Edge or Chrome without a window, over the DevTools protocol, against an API that is running:

1. The admin logs in, sets up the demo city if there is none, gives a day its demo orders, and places an order by a click on the map.
2. The day is planned with a second a route. Each route has its row and its line, the streams are live, the jobs are counted of the last day (D84), and Plan cannot be pressed (D83). A route sheet opens and closes.
3. The plan is discarded, and Plan can be pressed again.
4. Somebody opens an account, places an order by a click, sees it wait for its day, and withdraws it.

All the while the browser has to hold nothing against the page: no error in its console, nothing refused by the policy (D69), no request that failed but the depot that a new database does not have, and no request to another host. The tiles are switched off for it, so that it asks the API alone.

- **The day** is a month from today unless it is named, out of the way of what is done on the page. The script clears it first, through the API, and leaves its plan discarded.
- **What it needs:** Node 22 or later, and no package, since Node has `fetch` and `WebSocket` of its own. And Edge or Chrome, found where they are installed or named in `BROWSER`.
- **Run by hand,** after a change to the page, against a stack that was built with it.

Why:

- The page had grown real logic, phase by phase: an order with its key, two streams, lists that keep their rows, the vans, the sheets. It was tried by hand in a headless browser, by scripts that were not kept, so the next change to the page could not repeat what was tried.
- A script of Node, and not one of PowerShell and one of sh as the other scripts are: neither has a way to speak the protocol without something added, and Node has. It runs on Windows and anywhere else alike.
- Through the page and not around it: what it checks is what a person sees. It uses the API only to clear the day it plans.

What it does not do: run by itself, or look at the pictures. It takes one when it fails, and says where it is.

Tried against an API and a solver of four workers on the host, on a new database: it passed in 6.4 seconds. With the rule of D83 taken out of the page, it failed where Plan could be pressed. Against the stack of a clone with no `.env`, it passed in 4.6 seconds.

### D87. Demo orders lie on the streets of Kyiv (in place)

Decided with the project owner on 2026-10-14, in Phase 13. Changes D46.

A demo order is at a point of a street, picked at random among the points of the streets within the radius of the depot. The radius is 10 km unless the request names another. Round a depot with no street within it, the orders are spread evenly over the disc, as D46 has them.

- **The streets** are in `crates/routeforge-api/data/kyiv-streets.bin`, which is compiled into the API. `scripts/streets.mjs` takes them out of the extract of OpenStreetMap that `scripts/osrm-prepare` fetches for the OSRM server (D67). It keeps the ways whose `highway` is `primary`, `secondary`, `tertiary`, `unclassified`, `residential` or `living_street`, and that are not a bridge, a tunnel, an area or private. Along each it puts a point every 25 metres, inside the rectangle the extract was cut to.
- **The file** is the south-west corner of that rectangle, as two little-endian `i32` in hundred-thousandths of a degree, and then each point, as two little-endian `u16` north and east of the corner in the same unit: a metre or so. The extract of 2026-10-12 gave 18 991 streets, 5018 km of them, as 202 297 points in 790 kB. The points are sorted, so the same extract gives the same file, byte for byte.
- **Where the streets are many, so are the orders.** Every point within the radius is as likely as another, so a part of the city with twice the streets gets twice the orders. Two orders may share a point.
- **Each request reads the file** and keeps the points within the radius: 67 718 of them within 10 km of the demonstration's depot, in some 8 ms in a release build. Nothing is kept between requests, which only an admin makes.
- **The radius** takes in both banks of the Dnipro, and keeps within the extract, which ends some 16 km south of the demonstration's depot and 24 km east of it. A larger radius is still taken, up to 30 km (D29), and its orders end where the extract does.

Why:

- A random point was in the Dnipro, in a forest or in a field as often as it was by a house. 66 of the 304 positions of the day of D66 were more than a hundred metres from a road, and one of them 1.25 km. The map of such a day is not a city's.
- In the repository, because the approved description has the demonstration need no network (D8). Moving each point onto a road through OSRM, as the solvers' tables do, works only where OSRM is turned on, which it is not on a clone (D67). It would also give the API a client of OSRM, which only the solver has.
- Not a shape of the river and the forests drawn by hand: it would be rough, and a point would still be in a smaller park or on a lake.
- A street, and not a street with a house by it. In a try, a quarter of the points within 10 km had no building of the extract within 50 metres. Among them were the main roads across the parks and the islands, but also wide avenues whose houses stand back from them, and whole quarters of private houses, of which OpenStreetMap has few.
- A script of Node, as `scripts/page-check.mjs` is (D86). Reading an extract takes zlib and a reader of protocol buffers, which Node has and PowerShell and sh do not. In Rust it would have been a sixth crate, or a dependency of the API for something the API never does.
- The file, and not the extract: the extract is 36 MB, and stays out of the repository (D67).

What it does not do:

- An order is on a street, not at a door. It may be on a main road where it crosses a park or an island, with no house beside it.
- Kyiv alone. A depot elsewhere gets the disc of D46. Another city would be another file, and the API reads one.
- A file made from a newer extract gives other orders for the same seed, and so other numbers in the README, which were measured with the file of 2026-10-12.

Measured in Phase 13, on the day of the demonstration's scripts, 300 orders with seed 7: all 300 were points of the file, on both banks of the river. As the crow flies the day is 226.70 km, where the day of 5 km was 126.13. By road it is 476.17 km, where that day was 475.92: the day is longer, and its detours are gone (D57, D66).

Tests: the file whole and in Kyiv; the points within a radius all there and no others, and none round Lviv; 2000 orders round Kyiv each on a street, within the radius and on every side of the depot; round Lviv, evenly over the disc. Taking the streets out of `scatter`, the radius out of the streets, and the disc away from a depot with no streets each made a test fail.
