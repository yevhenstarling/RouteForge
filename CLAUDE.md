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

CI runs `cargo audit` as well, in a job of its own. `.cargo/audit.toml` lists what it ignores, and why (D12). After a change to the dependencies, run it here first: `cargo install cargo-audit --version 0.22.1 --locked` once.

From Phase 3 on there are images too. After a change to the dependencies or to the `Dockerfile`, build them: `docker compose build`. Run that from PowerShell. From Git Bash or `cmd` it can end with `additional privileges requested: pass "--allow=fs.read=Dockerfile"`, which means that the shell's working directory is spelled with a lower-case drive letter; `BUILDX_BAKE_ENTITLEMENTS_FS=0` in the environment gets round it (see the traps in `docs/plan.md`).

## Running it

The whole stack, as somebody who has just cloned the repository gets it (D54):

```sh
docker compose up --build --scale solver=4
```

The page with the map is then at `http://127.0.0.1:8080/` and Swagger UI at `http://127.0.0.1:8080/docs/`. The admin is `admin@example.com` with the password `routeforge`, unless `.env` says otherwise. On a database with no depot and no courier, an admin gets a button on the page that sets up a demo city.

`scripts/demo.ps1` (or `.sh`, which needs `curl` and `jq`) makes the scaling run and the failure run on that stack (D57). It plans tomorrow, and on that day it cancels and discards the plans that are in its way.

The numbers in the README are made by `cargo run -p routeforge-tsp --release --example quality`, by `cargo bench -p routeforge-tsp` and by `scripts/demo.ps1 scaling -Solvers 1,2,4,8`, on a machine that is doing nothing else. After a change to the search, make them again.

The bot idles until `.env` has `TELEGRAM_BOT_TOKEN`, a token from @BotFather (D62). Nothing else has to be set for it. Whoever writes to it gets a client's account, and `/start` says the account's id, which an admin needs to make it an operator's (D59). It takes an address in words only where `.env` has `GEOCODER_URL`, a service that speaks as Nominatim does (D72).

Distances by road are off until they are turned on (D67). `scripts/osrm-prepare.ps1` (or `.sh`) fetches the roads of Kyiv into `osrm/` and prepares them, once. With `COMPOSE_PROFILES=osrm` and `OSRM_URL=http://osrm:5000` in `.env`, the same command then starts an OSRM server too, and the solvers ask it. A solver on the host asks it at `OSRM_URL=http://127.0.0.1:5000`. Only then are the routes on the map drawn along the streets (D73).

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

An API that is to take a bot needs `BOT_SERVICE_KEY` as well, of at least 32 bytes. The bot beside it is `cargo run -p routeforge-bot`, with the same key and `TELEGRAM_BOT_TOKEN` in its shell. It looks for the API at `http://127.0.0.1:8080` unless `BOT_API_URL` says otherwise. Its settings are listed at the top of `crates/routeforge-bot/src/main.rs`.

## Rules that are easy to break

- Exactly five crates. Helpers go into an existing crate, never a new one.
- `routeforge-tsp` has no I/O, no async and no workspace dependencies, and takes every random number from a seeded generator.
- `routeforge-api` never depends on `routeforge-tsp`. `routeforge-bot` never enables the `db` or the `openapi` feature of `routeforge-domain`.
- No `unsafe`. No `unwrap()` outside tests.
- Route costs are integer metres.
- A courier is never overloaded along its route (D65). A route is searched with `optimize_route_within`, on the `Loads` of its orders and its courier, and the solver checks the calling order with `Loads::fits` once more before it stores it. Whatever changes the order of a route's stops after the search, as turning it round for the roads does, has to check that too.
- A route job does not fail for want of road distances. A table that the routing service does not give is made of great circles, and the route is stored as `great_circle` (D66). The search gets a matrix that is the same both ways, and the length that is stored is measured along the table as it was given, leg by leg.
- Nor does it fail for want of the way a route takes along the streets, which a map draws (D73). The way is asked for when the search is over, with the lease kept, for the stops in the order the route is driven and not the order the search found, and only for a route that was measured by road. It is asked for with `continue_straight` off, or it is not the way the table measured. A route that gets none is stored without it.
- Lease and retry times come from the database clock, not the worker's.
- After changing a query or a migration, run `scripts/sqlx-prepare.ps1` (or `.sh`) and commit `.sqlx/`: builds check queries against that directory, not a database (D28). Never edit a migration that has been merged; add a new one.
- Nothing may need OpenSSL, CMake or NASM to build: TLS stays on rustls with ring, crypto on pure-Rust backends (D12).
- A solver applies no migrations. The API owns the schema (D32).
- In a handler, the argument that says who may call it (`AuthUser`, `Staff`, `Admin`, or `Bot` for the bot's key) comes first, so the gate is checked before the body is read (D41, D60).
- A new endpoint needs a row in `MATRIX` in `crates/routeforge-api/src/app/tests.rs`. After changing a handler or a type the API sends or receives, run the tests with `UPDATE_OPENAPI=1`, read the diff of `crates/routeforge-api/openapi.json`, and commit it (D43).
- Every error the API sends is an `ApiError`, which leaves as `application/problem+json`. The cause of a `500` goes to the log and never into the response (D39).
- A login is counted before its account is looked up and before anything is hashed, and a try that came to nothing through a fault of the API's is taken back (D68). A password is hashed, and checked, through `Passwords` alone, which gives each a turn and so many at a time.
- A job's heartbeat, its search and the announcer of its progress belong to the job's future. Put one of them into a task of its own and a job that is dropped keeps its lease, or its thread (D34). An event stream of the API is likewise a value its response owns, with no task behind it (D50).
- A change to a plan or to one of its jobs goes through `db::queue`, which tells whoever watches the plan, in the transaction of the change. A new way to change either has to tell them too, with the plan's id and nothing else as the payload (D48).
- An order that is placed or withdrawn announces its day on `routeforge_orders`, in the statement of the change. A new way to change an order that no plan announces has to do the same, with the day and nothing else as the payload (D63).
- The bot asks the API with a token of the person it is talking to. Its key is for `telegram_login` and for nothing else, and it keeps no token from one update to the next (D9, D60).
- In the bot, `chat.rs` has no teloxide in it, and `telegram.rs` decides nothing. What the bot says and does is written in `chat.rs` and tested against `FakeApi` (D61).
- In the bot, what changes something is done by the press of a confirming button and by nothing before it (D9, D61).
- A request of the bot's that is new, or goes to another path, needs its operation in `crates/routeforge-api/openapi.json`: the bot's tests answer only where that document says (D64).
- Neither the Telegram token nor the bot's key may reach a log. Never format a teloxide `Bot`: its `Debug` shows the token (D62).
- What a person writes to the bot goes to the geocoding service, if there is one, and nowhere else: not into a log, and not into an error. A reqwest error names the address it asked, query and all, so it goes through `without_url()` first (D72).
- Every request to the geocoding service waits for its turn in `Nominatim::turn`: one a second for all the chats together, which is what the public service allows an application. The bot asks it when a person has written an address, and at no other time (D72).
- A file added under `web/` needs a row in `FILES` in `crates/routeforge-api/src/page.rs`. Files under `web/vendor/` are kept as they were released: do not edit them (D52).
- The page asks no other host for anything but map tiles, and puts text from the API into the document as text, never as HTML (D52). After changing `web/index.html`, try it in a browser: the tests check that the page is served and what it loads, not what it does.
- The page is served with a policy that names its one `<script>` and its one `<style>` by their hash, and allows no other inline code (D69). So the page has one of each, without attributes, and no `style` attribute and no handler in an attribute anywhere. What the browser refuses shows in its console and nowhere else.
- The page makes its SVG with Leaflet's `L.SVG.create`, and names no namespace: to the test that reads the page for other hosts, a namespace is an address (D75).
- What moves on the page is a simulation, and the page says so beside it. Nothing reports where a courier is, and nothing is to: tracking is among what the approved description rules out. The vans are reckoned on the page, from the routes as they are stored (D74).
- The list of routes on the page replaces a row only when what it says has changed. Written anew with every event of a search, a button in a row cannot be pressed (D75).
- `rust-toolchain.toml` and the base image in the `Dockerfile` name the same compiler. Change them together (D10).
- `docker compose up` has to work on a clone with no `.env`. A variable the compose file hands to a service needs a fallback there when the binary has no default for it. A test in `crates/routeforge-api/src/config.rs` holds the fallbacks of the `api` service to what the API starts with, and one in `crates/routeforge-bot/src/config.rs` those of the `bot` service (D54). The bot and the API have to fall back to the same `BOT_SERVICE_KEY`. The three values that are no secrets are constants of the API as well, which warns when it starts on one of them: change them in both places (D70).
- What asks a service outside the stack is off on a clone: OSRM is behind a compose profile, and the bot's geocoder has no value to fall back on (D8, D67, D72).
- A service is the first process in its container, and the first process is not ended by a signal it does not listen for. A binary has to hear SIGTERM itself, or stopping the stack waits ten seconds for it (D35, D51, D56). teloxide's own handler hears Ctrl-C alone, so the bot does not use it (D62).
- The runtime image gets no tools. What a health check needs to ask is asked by the binary itself, as `routeforge-api healthcheck` does (D55).

## Dependencies

Versions are pinned in the root `Cargo.toml` and `Cargo.lock`. Several had recent major releases (sqlx 0.9, utoipa 6, rand 0.10, jsonwebtoken 11, argon2 0.6, criterion 0.8), so check docs.rs for the pinned version. `cargo info <crate>` lists a crate's features.

teloxide 0.17 is used without its `ctrlc_handler` and `macros` features, and its `dispatch()` is not called: it panics when Telegram cannot be reached (D62). The traps met with it are in `docs/plan.md`.

The project is developed on Windows. Keep helper scripts in both PowerShell and bash, and files in LF.
