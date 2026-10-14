# RouteForge: code review

| | |
|---|---|
| Date | 2026-10-14 |
| Commit | `7ae9a7e` on `dev` (Phase 10 merged) |
| Scope | Architecture, code quality, security, performance and scalability, reliability and operations, tests, documentation |
| Method | Reading the code and the documents, and running the project's own checks ([appendix A](#appendix-a-check-results)) |
| Reviewer | Claude (Opus 5.5), at the project owner's request |

Nothing in the code was changed for this review. File references are links to the line in question.

## 1. Summary

RouteForge is in very good shape for a capstone project. Its core holds up to an adversarial reading:

- the PostgreSQL job queue: claims with `SKIP LOCKED`, an attempt number as fencing token, leases on the database clock, results written in the transaction that completes the job, and one lock order;
- the solver, where the heartbeat, the search and the progress announcements live inside the job's own future, so nothing outlives a job that is dropped;
- the deterministic anytime search, which the budget, the deadline and the cancel flag can stop but never steer.

These parts are correct by construction where that is possible and tested where it is not, races and deadlocks included. The API gates every handler by the type of its first argument and is held to a hand-written permission matrix and to its committed OpenAPI document. The decision log (D1 to D80) explains nearly every choice a reviewer would question.

Format, clippy with warnings as errors, the bot built without the `db` feature and `cargo audit` all pass. All 557 tests pass. One of them failed once in seven runs, on a timing margin of 200 ms ([T1](#t1--low--a-probe-test-gives-an-answer-200-ms-to-arrive)).

**No finding is High.** These are worth acting on first:

| ID | Finding | Severity |
|---|---|---|
| [R1](#r1--medium--a-panic-in-a-jobs-async-code-removes-the-worker-for-good) | A panic in a job's async code ends that worker for good. When the last worker is gone, the solver exits with status 0, and compose does not restart it. | Medium |
| [S1](#s1--medium--the-login-limit-can-be-reset-by-flooding-the-limiter) | The per-address login limit can be reset by a burst of logins for about 10,000 made-up addresses. A full map evicts its oldest entry, which is the victim's. | Medium |
| [Q1](#q1--medium--the-page-is-the-largest-untested-component) | The page is about 1,800 lines of JavaScript with no test in the repository. The browser driver that tried it was not committed. | Medium |
| [P1](#p1--lowmedium--the-live-stats-count-the-whole-job-history) / [R2](#r2--lowmedium--nothing-is-ever-deleted) | Each open operator page counts every job ever stored, every two seconds. Nothing deletes old jobs. | Low/Medium |
| [A1](#a1--lowmedium--a-second-plan-of-a-day-gives-couriers-a-second-round) | A second plan for a day that already has a done plan gives couriers a second round. The documented model says a courier makes one round a day. | Low/Medium |

Two items of the approved description are still open, and the project already says so itself: the bot has no LLM part ([C1](#c1--medium--the-bot-is-not-llm-powered)), and it has never talked to the real Telegram ([C2](#c2--medium--the-bot-has-never-met-telegram)).

## 2. How the review was done

**Read in full:**

- `docs/description.md`, `docs/architecture.md`, the README and `CLAUDE.md`. From `docs/plan.md`: the phase table, Phase 10, the known traps, what is needed from the owner, and the verification steps.
- Every non-test source file of `routeforge-tsp`.
- The domain model (`model.rs`), and the validation in `LatLon`, `Polyline` and `IdempotencyKey`.
- In the domain crate: `db/mod`, `db/queue`, `db/orders` and `db/plans`, and the queries of `db/routes` and `db/workers`. All eight migrations.
- In the API: `app`, `auth`, `logins`, `stream`, `events`, `page`, `probe`, `main`, and the handlers of orders, plans, auth, system and demo.
- In the solver: `main`, `worker`, `route_job` and `plan_job`.
- The Dockerfile, the compose file, the CI workflow file and the ignore files.

**Read in part:**

- The bot: the outline of `chat`, the drafts and their dispatch, `telegram`'s run loop, and `main`.
- The page: its injection sinks, storage, CSP inputs and how it shows plans.
- Tests: sampled, and counted.

**Run:** the checks of `CLAUDE.md` ([appendix A](#appendix-a-check-results)).

**Not done:**

- The page was not tried in a browser.
- Nothing ran against Telegram or OSRM.
- No load test, and no run of `scripts/demo`.

Each finding was held against `docs/architecture.md`. Where a decision already accepts a trade-off, the finding names that decision and is graded Info or Low.

### Severity

| | |
|---|---|
| **High** | Wrong results, lost data or an exploitable weakness, now. |
| **Medium** | A real defect or risk that is worth fixing, or deciding on, soon. |
| **Low** | An improvement with a clear benefit. |
| **Info** | An observation. Often a trade-off a decision already accepts, noted so that it stays visible. |

## 3. Conformance to the approved description

| The description asks for | Status | Where |
|---|---|---|
| One workspace of five crates | Met | [Cargo.toml](../Cargo.toml), D3 |
| `api` in actix-web: orders, auth, enqueues solve jobs | Met | [routes/orders.rs](../crates/routeforge-api/src/routes/orders.rs), [plans::submit](../crates/routeforge-domain/src/db/plans.rs#L128-L138) |
| `solver` as several instances, no coordinator | Met | [queue::claim](../crates/routeforge-domain/src/db/queue.rs#L200-L267), D22, D33 |
| `bot`: an LLM-powered Telegram agent | **Partly** | Buttons, commands and addresses, but no language model ([C1](#c1--medium--the-bot-is-not-llm-powered)), and not tried against Telegram ([C2](#c2--medium--the-bot-has-never-met-telegram)) |
| One PostgreSQL for the domain data and the queue | Met | [0001](../crates/routeforge-domain/migrations/0001_domain.sql), [0002](../crates/routeforge-domain/migrations/0002_queue.sql) |
| `FOR UPDATE SKIP LOCKED` with `LISTEN/NOTIFY`, a lease and a heartbeat, no load balancer | Met | [queue.rs](../crates/routeforge-domain/src/db/queue.rs), D22 to D26 |
| `--scale solver=4` with no configuration change | Met | [docker-compose.yml](../docker-compose.yml), D38, D54 |
| A `DistanceProvider` trait, Haversine by default, OSRM optional, a demo that works offline | Met | `geo.rs`, `osrm.rs`, D8, D66, D67 |
| JWT with the roles client, operator and admin | Met | [auth.rs](../crates/routeforge-api/src/auth.rs), D40, D41 |
| REST with an OpenAPI schema and Swagger UI at `/docs` | Met | [app.rs](../crates/routeforge-api/src/app.rs), `openapi.json`, D43 |
| Job progress over SSE | Met | [stream.rs](../crates/routeforge-api/src/stream.rs), D49, D50 |
| `tracing` logs with a request id carried end to end | Met | `request_id.rs`, D36, D42 |
| One HTML page with Leaflet, no npm and no build step, showing routes, queue depth and worker load live | Met | [web/index.html](../web/index.html), D52 |
| The whole stack from one `docker compose up` | Met | D54 |
| The technologies listed, among them `spawn_blocking`, criterion and proptest | Met, except the Claude API | `route_job.rs`, `benches/`, the proptest suites |
| Sweep, then nearest neighbour, then 2-opt and Or-opt, then annealing. Anytime, cancellable, deterministic | Met | `routeforge-tsp`, D5, D15 to D18 |
| What the POC will not do: GPS tracking, live re-planning, payments, time windows | Respected | The vans are a simulation reckoned on the page, and say so (D74) |

### C1 · Medium · The bot is not LLM-powered

The description calls the bot "an LLM-powered Telegram agent" and names "teloxide + the Claude API" ([description.md:13](description.md#L13), [:22](description.md#L22)). The bot takes buttons, commands and, where a geocoder is set, addresses. The README says so plainly, and D58 gives the reasons. Phase 11 is planned but not decided, and needs an API key.

**Recommendation:** decide on Phase 11 before the project is presented, so that the gap is a decision and not a surprise. If it is built, the plan in `docs/plan.md` stands as written: tool calls go through the draft, and the confirming button stays the only way to place an order (D9).

### C2 · Medium · The bot has never met Telegram

Every bot test runs against stand-ins: a fake API, a wiremock server held to `openapi.json`, and a wiremock Telegram (D64). D64 names what a stand-in cannot show. A keyboard of the wrong shape, a label too long for a button, or a method that Telegram answers otherwise would all pass here and fail there. Phase 7 is "built; not tried against Telegram yet".

**Recommendation:** the owner has said that no token will be available. If that changes, it is the cheapest high-value check in the project: ten minutes with a token, and the steps are in "Needed from the project owner" in `docs/plan.md`. Until then, the bot is the largest surface of the system that has not been tried for real.

## 4. Architecture

### 4.1 What works well

- **The queue.** Each part of the queue does one job:
  - The claim is one statement, with `SKIP LOCKED` ([queue.rs:200-267](../crates/routeforge-domain/src/db/queue.rs#L200-L267)).
  - The attempt number is the fencing token. A worker's every later call matches on it.
  - Leases and retry times come from the database clock, and workers pass only spans of time.
  - Completion is a handle that owns the transaction ([`Completion`](../crates/routeforge-domain/src/db/queue.rs#L307-L370)), so results and "done" commit together.
  - Every operation that locks a plan and its jobs locks the plan first (D24). A test races completions, a final failure and a cancel in every starting order, and fails with a deadlock if the rule is taken out.

  This is the textbook at-least-once queue with fenced completion, done right.
- **Fan-out with a counter.** A plan counts its stored routes on its own row. A plan with no routes goes straight to `done` ([plans.rs:186-210](../crates/routeforge-domain/src/db/plans.rs#L186-L210)), so the "last route marks the plan done" rule has no hole.
- **Lifetimes tied to futures.** The heartbeat, the announcer and the search all belong to one job future. `Search` sets the cancel flag on `Drop` ([route_job.rs:374-443](../crates/routeforge-solver/src/route_job.rs#L374-L443)). A dropped, aborted or shut-down job cannot leave a lease or a thread behind (D34). The API's event streams follow the same rule: they are values the response owns, with no task behind them ([stream.rs:119-160](../crates/routeforge-api/src/stream.rs#L119-L160)).
- **Notifications as hints.** Every change notifies inside the transaction that makes it, so nobody hears of a change that was rolled back. Every listener also polls, so a lost notification costs time and never correctness (D26, D48, D50).
- **A search that can be reproduced.** `Control` charges the budget per row before scanning it, and polls the flag and the clock every 16,384 moves ([control.rs:115-151](../crates/routeforge-tsp/src/control.rs#L115-L151)). So a cut by budget depends on the input alone, and a cut by deadline is a prefix of the same search (D5, D15).
- **Boundaries that the build enforces.** `routeforge-tsp` has no I/O and no workspace dependencies. The bot builds without `db`, and that is checked. The API never links `routeforge-tsp`.
- **Schema ownership and versions.** The API migrates, and solvers wait for the schema (D32). A solver older than the schema still runs. The depot has versions, so a plan keeps the depot it was clustered round (D21).
- **One request id across services.** The id is checked at the door, stored on the plan's jobs and inherited by the route jobs, so one id finds a plan in the logs of the API and of every solver (D36, D42).

### 4.2 Findings

#### A1 · Low/Medium · A second plan of a day gives couriers a second round

**Where:**

- D20 says "a later plan for the same date takes the orders that are still pending and adds its routes to those that are there", and D30 confirms it.
- `plans_one_active_per_date` only limits *active* plans ([0001_domain.sql:83](../crates/routeforge-domain/migrations/0001_domain.sql#L83)).
- `routes_plan_courier_key` is unique per plan ([:125](../crates/routeforge-domain/migrations/0001_domain.sql#L125)).
- The plan job offers every active courier with its whole capacity ([plan_job.rs:38](../crates/routeforge-solver/src/plan_job.rs#L38), [:91](../crates/routeforge-solver/src/plan_job.rs#L91)).

**What happens:** after plan A is done, orders placed later for the same day stay pending. Plan B clusters them over all active couriers again, at full capacity. A courier can end up with a route in A and another in B for one day. The README says "a courier makes one round a day", and so does D65 ("A courier drives one round a day"). The page shows one plan of a day at a time ([index.html:1637-1654](../web/index.html#L1637-L1654)), so an operator does not see that the courier is already out.

**Why it matters:** the documented model and the behaviour disagree. A courier's capacity for the day is not respected across plans, and the simulated vans of the two plans would overlap.

**Recommendation:** decide, and record it as a decision. The options:

- **(a)** Refuse a plan for a day that has a done plan, with a `409` that says "discard it first". This is the smallest change and matches "one round a day".
- **(b)** Offer a later plan only the couriers that have no route that day.
- **(c)** Make it explicit that a later plan is a second wave, and have the page and the README say so.

Effort: S for (a) or (b).

#### A2 · Info · LISTEN/NOTIFY is the first scaling ceiling

**Where:** every function of [queue.rs](../crates/routeforge-domain/src/db/queue.rs) that changes a job notifies, and so does [orders::place](../crates/routeforge-domain/src/db/orders.rs#L120-L149). Search progress is sent through `announce_progress` ([queue.rs:506-529](../crates/routeforge-domain/src/db/queue.rs#L506-L529)), about four times a second for each running search ([route_job.rs:504-527](../crates/routeforge-solver/src/route_job.rs#L504-L527)).

**What happens:** in PostgreSQL, a transaction that has issued `NOTIFY` takes a database-wide lock when it commits (`PreCommit_Notify` in `async.c`), so that notifications enter the queue in commit order. Notifying transactions therefore commit one at a time.

**Why it matters:** not at demo scale, with a handful of solvers and tens of notifications a second. Under heavy write load it serialises exactly the hot paths, and claims and completions would queue behind progress traffic.

**Recommendation:** nothing now. If throughput ever matters, keep `NOTIFY` for coarse, rare events ("a plan changed", "jobs are waiting"). Then send progress less often, for example only when the best length falls and at most once a second, or move it to a path that does not commit. One sentence in D48 or D49 would record the ceiling.

#### A3 · Info · The API is a singleton by design

The login counts are in process memory (D68), and each API process has one listening connection and one broadcast hub (D50). Two API instances would each count logins separately, which would double an attacker's budget. The approved description scales the solvers, not the API, so this is accepted. It is noted for whoever adds a second API.

## 5. Code quality

### 5.1 What works well

- **Lints.** `unsafe` is forbidden. `unwrap` outside tests, `dbg!` and `todo!` are warnings, which `-D warnings` turns into errors. Clippy is clean today ([appendix A](#appendix-a-check-results)).
- **Types that cannot hold an invalid value.**
  - [`LatLon`](../crates/routeforge-domain/src/geo.rs) checks its range, also when it is deserialized.
  - [`RouteBudget`](../crates/routeforge-domain/src/model.rs#L245-L311) cannot be built without a limit, not even through serde.
  - `IdempotencyKey` checks its alphabet and length.
  - `Polyline` is decoded when it is made.
  - Every table has an id type of its own.
  - `Completion` is `#[must_use]`.
- **One text per enumeration.** `text_enum!` gives each value one text for JSON, the database, `Display` and `FromStr`, and a test round-trips every value through the schema's `CHECK` (D19).
- **Errors.** The library uses `thiserror`. `DbError::Unreadable` turns a row this build cannot read into an error, never a panic. The binaries use `anyhow`. `ApiError` becomes `application/problem+json`, and the cause of a `500` goes to the log only.
- **Integer metres and exact deltas (D4).** `Tour::check` debug-asserts that the tracked cost never drifts from the recomputed one.
- **Readable code.** Functions are small, and their names say what they do: `with_the_lease_kept`, `in_the_order_of_the_job`, `calling_order`. Comments explain why, and cite the decision that says so. There is next to no dead code.

### 5.2 Findings

#### Q1 · Medium · The page is the largest untested component

**Where:** [web/index.html:269-2071](../web/index.html#L269-L2071), one inline script of about 1,800 lines. D52 says "The page has no tests of its own", and D80 points to `docs/plan.md` for how it was tried.

**What happens:** since Phase 5 the page has grown real logic:

- the order form and its idempotency key;
- two event streams with reconnects;
- lists that keep their rows (D75);
- the van simulation and the load sheets.

It was tried by hand in a headless browser driven over the DevTools protocol, but that driver is not in the repository. So the next change to the page cannot repeat the session.

**Why it matters:** a regression on the page is found only by a person who repeats the session by hand. The CSP rule makes some breakage silent: one script and one style, named by hash (D69). A second `<style>` or a `style` attribute stops working, and says so only in the browser's console.

**Recommendation:**

1. Commit the DevTools driver under `scripts/` as a smoke test that a developer runs locally. It would log in, set up the demo city, place an order, plan the day, wait for the routes, and fail on any console error. It needs no npm if it uses only Node's built-in `fetch` and `WebSocket`, or PowerShell.
2. Optionally, move the script to `web/app.js`. `script-src 'self'` already allows that file without a hash, and editors can then type-check it with `// @ts-check` and JSDoc, still with no build step. This changes D52 and D69, so it is the owner's call.

#### Q2 · Low · Process plumbing is copied into three binaries

**Where:**

- `init_logging` and `told_to_stop` are identical in [api main.rs:153-204](../crates/routeforge-api/src/main.rs#L153-L204), [solver main.rs:82-132](../crates/routeforge-solver/src/main.rs#L82-L132) and [bot main.rs:116-170](../crates/routeforge-bot/src/main.rs#L116-L170).
- The back-off that doubles from one second to thirty is written again in the API's listener ([events.rs:30-33](../crates/routeforge-api/src/events.rs#L30-L33)), the solver and the bot.
- `causes()` is in both [bot api.rs:205](../crates/routeforge-bot/src/api.rs#L205) and [geocode.rs:270](../crates/routeforge-bot/src/geocode.rs#L270).

**Why it matters:** about 50 lines, three times over, have to stay in step by hand. The comments say as much ("The solver listens the same way").

**Recommendation:** with three binaries this is acceptable. If it grows, a `service` module in `routeforge-domain` behind a feature of its own (with `tokio` signals and `tracing-subscriber`) would serve all three and keep to five crates. The bot can enable that feature without `db`.

#### Q3 · Low · `chat.rs` carries the whole dialogue in one `impl`

**Where:** [crates/routeforge-bot/src/chat.rs](../crates/routeforge-bot/src/chat.rs) has 1,460 lines. `impl Chats` runs from line 442 to line 1236: drafting orders, withdrawals, operator commands, address search and the reply texts.

**Recommendation:** split by concern into submodules, each with an `impl Chats` block of its own: `chat/draft.rs`, `chat/orders.rs`, `chat/operator.rs` and `chat/search.rs`. This changes no behaviour and no test, and D61 still holds: no teloxide in `chat`.

#### Q4 · Low · The compiler pin is held by an unmaintained dependency of teloxide

**Where:** D10. Clippy prints a future-incompatibility warning for `proc-macro-error2 2.0.1`, and `cargo audit` reports RUSTSEC-2026-0173 (unmaintained). It comes in by `teloxide 0.17.0`, then `aquamarine 0.6.0`, then `proc-macro-error2` ([appendix A](#appendix-a-check-results)).

**Why it matters:** a later compiler will reject that crate. Until teloxide drops or updates `aquamarine`, the toolchain cannot move past the version that starts rejecting it.

**Recommendation:** watch teloxide's releases, and check whether a newer `aquamarine` without `proc-macro-error2` can be patched in. Once known, record in D10 the compiler version that breaks the build.

#### Q5 · Info · A mistyped `RUST_LOG` is passed over without a word

**Where:** [api main.rs:188-192](../crates/routeforge-api/src/main.rs#L188-L192), and the same lines in the solver and the bot.

**What happens:** a `RUST_LOG` that does not parse falls back to `info` silently, so a typo in a filter looks like a filter that has no effect.

**Recommendation:** log one warning once logging is up.

## 6. Security

### 6.1 What holds

| Area | How | Where |
|---|---|---|
| SQL injection | Every query is a compile-checked sqlx macro with bound parameters. No SQL is built at run time. | D28, `db/` |
| Authentication | Argon2id at 19 MiB, two passes, one lane. A login for an unknown address is checked against a decoy hash, so its timing says nothing. JWT HS256 with `exp` and `sub` required and no leeway. The bot's key is compared in constant time. | [auth.rs:43-161](../crates/routeforge-api/src/auth.rs#L43-L161), [:254-269](../crates/routeforge-api/src/auth.rs#L254-L269), D40 |
| Authorisation | The gate is the first argument of every handler (D41), and a test checks every endpoint against every role. A client gets the `404` of a missing order for somebody else's, and lists are narrowed to the caller. | [routes/orders.rs:342-347](../crates/routeforge-api/src/routes/orders.rs#L342-L347), `app/tests.rs` |
| Cost of a login | Four hashes at a time. A login that waits more than five seconds for its turn is answered `503`. | D68 |
| Error disclosure | Every error is `problem+json`. The cause of a `500` goes to the log, never the response. | D39 |
| Log hygiene | Request ids are validated (D42). The types that hold secrets have no `Debug`, or one that leaves the secret out (`NewUser`, `Credentials`). reqwest errors are stripped of their URL before they are logged (D72). | `request_id.rs`, `auth.rs`, `model.rs`, `geocode.rs` |
| Browser | CSP with `default-src 'none'`, the script and the style named by hash, and `frame-ancestors 'none'`. No HTML sinks: text goes in with `textContent` or `el()`, and tooltips get elements. `nosniff`, and static files from an allow-list. The token travels in `Authorization`, so there is no cookie and no CSRF. No CORS. | [page.rs:62-146](../crates/routeforge-api/src/page.rs#L62-L146), [index.html:388-391](../web/index.html#L388-L391) |
| Supply chain | `cargo audit` is clean, apart from one documented ignore (an RSA timing issue that HS256 never reaches) and one unmaintained crate (Q4). Builds are `--locked`. Leaflet is vendored, and the commit that added it records the SHA-256 of each file. | `.cargo/audit.toml`, D12, D52 |
| Deployment | Ports are published on `127.0.0.1` only. The API warns at every start that runs on the demo secrets (D70). Containers run as a user without privileges. | `docker-compose.yml`, `Dockerfile` |

### 6.2 Findings

#### S1 · Medium · The login limit can be reset by flooding the limiter

**Where:** [logins.rs:79-81](../crates/routeforge-api/src/logins.rs#L79-L81) and [:129-140](../crates/routeforge-api/src/logins.rs#L129-L140). The map holds at most 10,000 addresses ([:28](../crates/routeforge-api/src/logins.rs#L28)).

**What happens:**

1. `begin` counts a login before anything is looked up or hashed, and makes an entry for every new address.
2. When the map is full, a new address first removes the entries whose window has ended. If none has ended, it removes the entry that was begun first.
3. An attacker who has used up the five tries on a victim's address sends about 10,000 logins for made-up addresses within a few seconds. Each is counted at once, before it waits for a hashing turn.
4. The map fills, and the victim's entry, being the oldest, is the one evicted. The next five guesses at the victim's password are free.

Meanwhile the flood holds all four hashing turns, so everybody else's logins get `503` for up to five seconds.

**Why it matters:** D68 promises that "only so many logins are taken for one address within a minute". Against a client that can send about 10,000 requests in a burst, that becomes five guesses per burst. Guessing is still bounded by hashing throughput, but the limit no longer holds as stated.

**Recommendation:** when room has to be made, never evict an entry that has reached its limit. Evict the entry with the lowest count, newest first: the filler entries have a count of one. If every entry is at its limit, refuse the new address with `429` rather than forget an old one. Add a test that exhausts one address, then fills the map with 10,000 new ones, and finds the first still refused. Effort: S.

#### S2 · Low · The bot's key is a master key for Telegram-linked accounts

**Where:** [routes/auth.rs:137-152](../crates/routeforge-api/src/routes/auth.rs#L137-L152). D60's "What the key is worth" accepts this.

**What happens:** `POST /api/auth/telegram` with the key returns a token for whichever Telegram id the caller names, in that account's role, admin included. Under compose's fallback key (D54), anybody who reaches the port can do that.

**Why it matters:** once any admin account has a Telegram id, the bot's key is an admin credential. And while it is the demo key, it is a public one, although only reachable from this machine.

**Recommendation:** cap the role of a bot-issued token at `operator`. The bot only calls client and operator endpoints (orders, plans, the depot: [bot api.rs:216-271](../crates/routeforge-bot/src/api.rs#L216-L271)), so the cap costs it nothing. The key then mints no admin token, whichever account it names. Effort: S.

#### S3 · Low · Nothing bounds what one account holds open

**Where:** [stream.rs:119-160](../crates/routeforge-api/src/stream.rs#L119-L160) keeps no count per user, and [routes/auth.rs:37-51](../crates/routeforge-api/src/routes/auth.rs#L37-L51) lets anyone open an account. Both are among the README's known limits.

**What happens:** anybody who reaches the port can open an account, and then any number of event streams. Each stream holds a broadcast receiver, and reads the database every 2 to 10 seconds and on every plan change. All of them read through a pool of ten connections, which waits five seconds before it gives up.

**Why it matters:** each stream is cheap, but they all read at once when a plan changes. A few thousand streams turn every plan change into thousands of queries through ten connections. The API's other requests queue behind them, and one that waits five seconds for a connection fails with `500`.

**Recommendation:** cap the streams per user, at eight for example. A counter in `AppState` goes up in `stream::open` and down in the `Drop` of the stream (where `Ending` already logs that it is over); beyond the cap, the answer is `429`.

#### S4 · Info · The demo secrets are in the repository

This is accepted in D54 and D70. Ports are on the loopback, and the API warns at every start that runs on these values. Nothing needs to be done as long as the README's instruction holds: set your own values in `.env` before opening a port to anything else.

## 7. Performance and scalability

#### P1 · Low/Medium · The live stats count the whole job history

**Where:**

- [queue.rs:627-645](../crates/routeforge-domain/src/db/queue.rs#L627-L645) runs `SELECT status, count(*) FROM jobs GROUP BY status` with no `WHERE`.
- It runs on every stats read ([system.rs:82-84](../crates/routeforge-api/src/routes/system.rs#L82-L84)).
- The stats stream reads again on every plan change ([system.rs:103-139](../crates/routeforge-api/src/routes/system.rs#L103-L139)), and every two seconds without a reason ([stream.rs:72](../crates/routeforge-api/src/stream.rs#L72)).

**What happens:** finished jobs are never deleted ([R2](#r2--lowmedium--nothing-is-ever-deleted)), and every stats read scans all of them. Every operator's page keeps a stats stream open.

**Why it matters:** the cost grows with the history, not with the load. It is harmless today. With a few hundred thousand jobs, it is a scan of all of them for each open page, every two seconds.

**Recommendation:** count only the unfinished jobs for the live view (`WHERE status IN ('queued', 'running')`, which the partial index `jobs_claimable_idx` serves). Take the totals of finished jobs from somewhere cheap, or leave them out of the stream. Pair this with R2.

#### P2 · Low · Catch-all filters on the order queries that run most

**Where:** [orders.rs:189-214](../crates/routeforge-domain/src/db/orders.rs#L189-L214) filters with `($1::uuid IS NULL OR client_id = $1) …` and ends in `ORDER BY id LIMIT $5`. It serves `GET /api/orders`, and every read of a client's stream ([routes/orders.rs:253-277](../crates/routeforge-api/src/routes/orders.rs#L253-L277)).

**What happens:** after five executions of a prepared statement on a connection, PostgreSQL may switch to a generic plan. A generic plan cannot rely on `OR`-ed parameters to pick the client index or the day index. With `ORDER BY id LIMIT` it tends to walk the primary key.

**Recommendation:** add an index on `(client_id, service_date)` (migration 0009), and give the client-and-day read of the D77 stream a query of its own. Compare `EXPLAIN (ANALYZE)` before and after, on some 10⁵ orders.

#### P3 · Low · `stops_of_day` counts each route's calls once per row

**Where:** [routes.rs:124-146](../crates/routeforge-domain/src/db/routes.rs#L124-L146).

**What happens:** each row runs `(SELECT count(*) FROM route_stops WHERE route_id = …)`. For staff on a day of 2,000 orders, that is 2,000 range scans of some 500 entries each.

**Recommendation:** count once per route, with a `GROUP BY route_id` in a CTE that is joined in, or with `count(*) OVER (PARTITION BY route_id)` in a subquery that runs before the client filter.

#### P4 · Low · Demo orders are inserted one statement at a time

**Where:** [demo.rs:59-63](../crates/routeforge-api/src/routes/demo.rs#L59-L63) makes up to 2,000 `orders::create` round trips in one transaction.

**Recommendation:** insert them in bulk with `UNNEST`, with one `pg_notify` for the day. This only matters for how long setting up a demo takes.

#### P5 · Info · The search, at a thousand stops (accepted in D18)

- **Or-opt is quadratic.** It tries every position for every segment, O(n²) per pass, with no neighbour lists or don't-look bits. At a thousand stops, local search alone takes about 25 million moves (D15), and Or-opt 0.55 s (README). Neighbour lists of the K nearest stops are the standard next step. On large routes they would leave more of the budget to annealing.
- **Two bounds checks per lookup.** `DistanceMatrix::get` asserts `to < nodes` on every lookup in the inner loops ([matrix.rs:108-116](../crates/routeforge-tsp/src/matrix.rs#L108-L116)), on top of the slice's own bounds check. The assert is needed for correctness with a flat layout. Storing the matrix as rows would give the same safety with one check per index. Measure with `cargo bench -p routeforge-tsp` before changing anything.

## 8. Reliability and operations

### 8.1 What works well

- **Graceful stops.** All three binaries hear SIGTERM. The solver gives its job back without using up an attempt (D35), the API ends its streams before it drains (D51), and the bot acknowledges its updates before it exits (D62).
- **Health checks.** The API checks its own health through its binary, with no curl in the image (D55). The database is asked over TCP, which avoids the false "ready" of the postgres image's set-up phase (D54).
- **Waiting for what is not there.** A solver waits for its database with back-off, and then for the schema (D32, D37). The API fails fast with the reason (D47).
- **Images.** Multi-stage builds with cargo-chef, one builder for three targets, a runtime image with nothing extra in it, and a user without privileges (D38).

### 8.2 Findings

#### R1 · Medium · A panic in a job's async code removes the worker for good

**Where:**

- `settle` polls `execute` in its own future ([worker.rs:203-207](../crates/routeforge-solver/src/worker.rs#L203-L207)).
- `run` only logs a worker that ends with a panic, and starts no other ([worker.rs:76-81](../crates/routeforge-solver/src/worker.rs#L76-L81)).
- `main` returns `Ok(())` once `run` does ([solver main.rs:72-76](../crates/routeforge-solver/src/main.rs#L72-L76)).
- The `solver` service has no `restart:` (D54).

**What happens:** a panic on a blocking thread is caught and reported as a failed attempt ([route_job.rs:430](../crates/routeforge-solver/src/route_job.rs#L430), [plan_job.rs:42-44](../crates/routeforge-solver/src/plan_job.rs#L42-L44); D37). A panic anywhere else in a job takes another path. It could come from the code that measures the route, or from any future change. It unwinds through `settle` and `work`, and ends the worker's task:

- the job is neither failed nor released, so it waits out its lease;
- nothing starts a new worker;
- with the default `SOLVER_CONCURRENCY=1`, the process then ends with status 0, which tells Docker and whoever looks that it stopped as it should.

**Why it matters:** a job that panics in its async part on some input takes a solver down with each attempt. Three attempts can take three of four solvers down before an idle worker buries the job, and the plan waits out three leases first. Nothing in the stack says why the solvers are gone.

**Recommendation:**

1. Catch a panic per job and report it as a failed attempt. The smallest change wraps `execute` in `futures_util::FutureExt::catch_unwind` with `AssertUnwindSafe`. Another way runs the job in a task of its own that is aborted when its handle is dropped. Either way the heartbeat and the search stay inside the job's future, so D34 holds. The panic message goes to `queue::fail`.
2. In `run`, treat a worker that ends without being told to stop as a fault: start a new one, or make the process exit non-zero. Exiting non-zero would pair well with `restart: on-failure` for the solver, which changes D54 and is the owner's call.
3. Add a test with a `DistanceProvider` whose `table` panics: the attempt is reported as failed, and the worker takes the next job.

Effort: S to M.

#### R2 · Low/Medium · Nothing is ever deleted

**Where:** the code has no `DELETE FROM jobs`, `workers` or `plans`. Each plan adds one job, and one more per courier. Each solver start registers a new worker id ([worker.rs:114](../crates/routeforge-solver/src/worker.rs#L114)).

**What happens:** the history grows without a bound, and P1's scan grows with it. The workers table gains a row for every worker of every start.

**Recommendation:** a retention rule, for example for the jobs of plans that finished more than N days ago, and for workers not seen for a day. An idle worker could apply it on its way out of `claim`, as it buries exhausted jobs, which keeps "no process of its own" (D23). Or document it as accepted for the POC.

## 9. Tests

### 9.1 Results

`cargo test --workspace --locked --no-fail-fast`, against the compose database:

| Crate | Tests | Result | Time |
|---|---|---|---|
| `routeforge-tsp` | 100 unit, 2 TSPLIB, 2 doc | Pass | 17.9 s + 2.6 s |
| `routeforge-domain` | 146 unit, 1 doc | Pass | 9.7 s |
| `routeforge-api` | 143 unit, 2 binary | Pass | 13.4 s + 1.3 s |
| `routeforge-solver` | 81 unit | Pass | 22.0 s |
| `routeforge-bot` | 74 unit, 6 binary | Pass | 2.3 s + 1.4 s |
| **All** | **557** | **Pass** | **79.9 s** with compiling |

The first run of the suite stopped on one failure in the API ([T1](#t1--low--a-probe-test-gives-an-answer-200-ms-to-arrive)). It passed in the full run above and in five more runs of the API's tests.

### 9.2 What works well

- **Volume.** 557 tests: 104 in `routeforge-tsp`, 147 in the domain crate, 145 in the API, 81 in the solver and 80 in the bot. Beside them, proptest suites in the algorithms and in `geo.rs`.
- **Properties, not examples, for the algorithms:**
  - every tour visits every stop once, and its cost bookkeeping is exact;
  - the same seed and budget give the same tour;
  - a larger budget never gives a longer tour;
  - small instances come out optimal, checked against an exhaustive search;
  - every tour that is reported fits its courier's loads.
- **Real database, real races.** `#[sqlx::test]` gives each test a database of its own. The tests make the races happen: completions at the same moment (D2), cancels against completions in every order (D24), accounts made concurrently (D59), and placements with the same key at once (D76).
- **Contracts held by tests:**
  - the API serves exactly the committed `openapi.json` (D43);
  - the bot's HTTP client is answered only where that document says (D64);
  - a hand-written permission matrix covers every endpoint and role (D41);
  - the compose fallbacks are held to what the binaries start with (D54).
- **Binaries run through `main`.** The API's health check and the bot are each run as a process (D55, D64).
- **Fault passes.** Each phase puts faults into the code on purpose and checks that a test catches each one: 60 of them in Phase 10, per `docs/plan.md`. In effect this is mutation testing by hand.

### 9.3 Gaps

- The page ([Q1](#q1--medium--the-page-is-the-largest-untested-component)), and the real Telegram ([C2](#c2--medium--the-bot-has-never-met-telegram)).
- No test runs the solver's binary, as tests do for the API's and the bot's, so its exit status is untested ([R1](#r1--medium--a-panic-in-a-jobs-async-code-removes-the-worker-for-good)).
- No test reports a panic that happens outside the blocking search ([R1](#r1--medium--a-panic-in-a-jobs-async-code-removes-the-worker-for-good)).
- The fault passes are done by hand. `cargo mutants` is in `.gitignore` already, and on `routeforge-tsp`, whose tests take seconds, it could repeat that work after every change to the search.

### 9.4 Findings

#### T1 · Low · A probe test gives an answer 200 ms to arrive

**Where:** [probe.rs:106-131](../crates/routeforge-api/src/probe.rs#L106-L131). The probe is called with `BRIEFLY`, 200 ms ([:80](../crates/routeforge-api/src/probe.rs#L80)), as its limit for connecting, writing and reading. [`an_api_that_listens_on_every_address_is_asked_on_the_loopback`](../crates/routeforge-api/src/probe.rs#L133-L147) does the same.

**What happened:** in the first run of the suite, `it_asks_whether_the_api_is_ready_and_takes_only_a_yes` failed at line 129:

```
"220 200 ESMTP\r\n": 127.0.0.1:56444 did not answer
```

The stand-in server is a thread that accepts, reads the request a byte at a time, and replies. "Did not answer" means the probe's read timed out, so the likeliest reading is that the thread did not get to reply within 200 ms. 143 tests were running beside it, on a binary that had just been built. The test passed in the next six runs.

**Why it matters:** a test that fails now and then teaches people to rerun on red, which hides the day it fails for a reason.

**Recommendation:** use a generous limit, such as `WITHIN` (2 s), wherever the stand-in does answer. Keep `BRIEFLY` for `it_does_not_wait_long_for_an_answer_that_does_not_come`, the one test whose point is the short wait. Effort: S.

## 10. Documentation

**Strengths:**

- The decision log is the project's best asset. Each decision says what was chosen, why, what it costs, and which tests hold it, and the code comments cite it by number.
- The README's "Known limits" is unusually honest.
- `CLAUDE.md`'s "Rules that are easy to break" and `plan.md`'s "Known traps" turn hard-won lessons into checklists.

**Observations (Info):**

- **The same facts in several places.** Benchmark tables, demo timings and limits appear in the README, in `architecture.md` and in `plan.md`. Each copy can drift from the others. Keep a figure in one place and link to it from the rest.
- **A long log.** `architecture.md` has 1,636 lines and grows with every phase. An index at the top, with the number, title and status of each decision ("in place", "replaced by D65"), would help a newcomer find the decisions that still hold.

## 11. What to do, in order

| # | ID | What | Severity | Effort | Who decides |
|---|---|---|---|---|---|
| 1 | R1 | Catch panics per job; replace a lost worker or exit non-zero; add a test | Medium | S–M | — |
| 2 | S1 | Evict filler, not exhausted entries, from the login limiter; add a test | Medium | S | — |
| 3 | A1 | Decide what a second plan of a day means, and enforce it | Low/Medium | S | Owner |
| 4 | Q1 | Commit the browser driver as a local smoke test of the page | Medium | M | — |
| 5 | P1, R2 | Count only live jobs for the stats; retain history for N days | Low/Medium | S–M | Owner (retention) |
| 6 | S2 | Cap bot-issued tokens at `operator` | Low | S | — |
| 7 | S3 | Cap event streams per user | Low | S | — |
| 8 | T1 | Give the probe tests a generous limit where an answer comes | Low | S | — |
| 9 | P2–P4 | Index and query shapes for orders and stops; bulk demo insert | Low | S each | — |
| 10 | Q2, Q3 | Shared service plumbing; split `chat.rs` | Low | S–M | — |
| 11 | Q4 | Watch teloxide for the `proc-macro-error2` fix | Low | — | — |
| 12 | C1 | Decide on Phase 11, the LLM part of the bot | Medium | L | Owner |
| 13 | C2 | Try the bot against Telegram when a token exists | Medium | S | Owner |
| — | A2, A3, P5, S4, Q5, docs | Record, no action now | Info | — | — |

## Appendix A. Check results

Run on the development machine (Windows 11, Rust 1.99.0), against the compose `db` service, which was already up and healthy.

| Check | Command | Result |
|---|---|---|
| Format | `cargo fmt --all --check` | Pass (0.5 s) |
| Lints | `cargo clippy --workspace --all-targets --locked -- -D warnings` | Pass (4.8 s, incremental). One future-incompatibility warning for `proc-macro-error2 v2.0.1` (Q4) |
| Bot without `db` | `cargo check -p routeforge-bot --locked` | Pass (1.7 s) |
| Advisories | `cargo audit` (0.22.1; 1,310 advisories, 439 crates) | Pass. One allowed warning: RUSTSEC-2026-0173, `proc-macro-error2` unmaintained, through `aquamarine` and then `teloxide` (Q4) |
| Tests, first run | `cargo test --workspace --locked` | Stopped at the API: 142 passed, 1 failed (T1). The other crates did not run (37.1 s) |
| Tests, full run | `cargo test --workspace --locked --no-fail-fast` | Pass: 557 tests (79.9 s with compiling); see [9.1](#91-results) |
| T1, again | `cargo test -p routeforge-api --bin routeforge-api --locked`, five times | Pass five times: 143 passed each time (12.9 to 14.1 s) |

## Appendix B. What was read

- **Documents:** `docs/description.md`, `docs/architecture.md` (D1 to D80), `docs/plan.md` (the phase table, Phase 10, the traps, what is needed from the owner, the verification steps), `README.md`, `CLAUDE.md`.
- **`routeforge-tsp`:** `lib.rs`, `control.rs`, `matrix.rs`, `tour.rs`, `moves.rs`, `problem.rs`, `construct.rs`, `local_search.rs`, `anneal.rs`, `loads.rs`, `sweep.rs`.
- **`routeforge-domain`:** `lib.rs`, `model.rs`, the `RouteJob` of `job.rs`, the validation of `LatLon` and `Polyline` in `geo.rs` and of `IdempotencyKey` in `ids.rs`, `db/mod.rs`, `db/queue.rs`, `db/orders.rs`, `db/plans.rs`, the queries of `db/routes.rs` and `db/workers.rs`, and migrations 0001 to 0008.
- **`routeforge-api`:** `main.rs`, `app.rs`, `auth.rs`, `logins.rs`, `stream.rs`, `events.rs`, `page.rs`, `probe.rs`, `error.rs` (in part), `validate.rs` (in part), and `routes/orders.rs`, `plans.rs`, `auth.rs`, `system.rs` and `demo.rs`.
- **`routeforge-solver`:** `main.rs`, `worker.rs`, `route_job.rs`, `plan_job.rs`.
- **`routeforge-bot`:** `main.rs`, `telegram.rs` (the run loop and handlers), `chat.rs` (its outline, the drafts and their dispatch), `api.rs` (the requests).
- **The page:** `web/index.html`, for its injection sinks, storage, the input of the CSP hashes, and how it shows plans.
- **Build and run:** `Cargo.toml`, `Dockerfile`, `docker-compose.yml`, `.github/workflows/ci.yml`, `.gitignore`, `.dockerignore`, `.cargo/audit.toml`, `clippy.toml`.

## What became of it

Phase 12 acted on this review on the same day; `docs/plan.md` has it in full. The review above is left as it was written.

| ID | What was done | Decision |
|---|---|---|
| R1 | A job is polled through `catch_unwind`, and a panic is a failed attempt. A lost worker ends its solver with an error. | D82 |
| S1 | The count that makes room is one with the fewest tries, and never one at its limit. | D68 |
| Q1 | `scripts/page-check.mjs` tries the page in a browser. | D86 |
| P1, R2 | The stats count the jobs that ended within the last day, by an index. A starting solver forgets workers not seen for a day. | D84 |
| A1 | A day whose plan is done is refused another plan until that plan is discarded. | D83 |
| S2 | The bot's token gives an operator's rights at the most. | D60 |
| S3 | One person holds 16 event streams at the most, `API_STREAMS_PER_USER`. | D85 |
| T1 | The probe's tests give an answer two seconds. | none |
| P2, P3, P4 | A query of its own and an index for a client's orders of a day, a route counted once, demo orders in one statement. | D77, D78, D46 |
| Q2, Q5 | `routeforge_domain::service`: logging that names an unreadable `RUST_LOG`, the stop signal, `Backoff`. | D81 |
| Q3 | The bot's dialogue in five files. | D61 |
| A2 | The ceiling is recorded. | D49 |
| Docs | The decision log has an index. | none |
| C1, C2, Q4 | Not mended in code: Phase 11, a Telegram token, and a release of teloxide. | D58, D64, D10 |
