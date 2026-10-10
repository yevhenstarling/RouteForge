# Approved project description

This is the description that was approved for the capstone before work started, reproduced with only its bullet formatting changed. It is the contract for the project: the crate count, the stack and the list of things the POC will not do are fixed unless the project owner says otherwise.

---

**RouteForge:** a backend for managing parcel pickup and delivery across a city.

**Objective:** turn a daily flow of orders into optimized courier routes, distributing the NP-hard route optimization across horizontally scalable workers.

**Architecture (briefly):**

- Backend: three Rust services. api (actix-web) accepts orders, handles auth and enqueues solve jobs; solver computes the routes and runs as several instances; bot is an LLM-powered Telegram agent that helps clients and operators place orders.
- Database: a single transactional database for both the domain data (orders, couriers, routes) and the job queue. PostgreSQL for now - the concrete database may still change closer to implementation; the one requirement is transactions and the queue in the same place.
- Load balancer: there is none, and none is needed. Workers pull jobs from the queue themselves (FOR UPDATE SKIP LOCKED + LISTEN/NOTIFY), so whoever is free takes the next one. A lease plus a heartbeat returns a job to the queue if a worker dies mid-solve. Scaling is docker compose up --scale solver=4, with no configuration changes.
- Distances: behind a DistanceProvider trait. Haversine by default (works offline, no external services), optionally OSRM for real road distances. The demo does not depend on the internet.
- Access and observability: JWT with client / operator / admin roles; REST with an OpenAPI schema and Swagger UI at /docs; job progress streams to the frontend over SSE; structured logs via tracing with a request id carried end to end.
- Frontend: deliberately light. A single HTML page with a Leaflet map, no npm and no build step. Shows routes on the map, queue depth and worker load, live.
- Running it: the whole stack comes up with one docker compose up, namely the database, api, N solver instances and bot.
- All of it is one Cargo workspace of 5 crates: routeforge-tsp (pure algorithms, no I/O), routeforge-domain, routeforge-api, routeforge-solver, routeforge-bot.

**Technologies:** Rust, actix-web, sqlx + PostgreSQL, tokio (spawn_blocking for the CPU-bound solve), tracing, JWT + argon2, utoipa/Swagger UI, Docker Compose, criterion, proptest; teloxide + the Claude API for the bot.

**Algorithms:** cluster-first / route-second. Sweep clustering by courier capacity, then TSP per cluster: nearest neighbour → 2-opt + Or-opt → simulated annealing. The solver is anytime: it works until its deadline, can be cancelled, and is deterministic given a seed.

**What this POC will not do:** live GPS tracking of couriers, traffic-aware or on-the-fly re-planning, payments, a mobile app, hard time-window guarantees or proofs of optimality, Kubernetes.
