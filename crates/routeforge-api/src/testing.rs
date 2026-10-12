//! Shared by the tests: the application inside a test, people with tokens, a small
//! JSON client, a reader of event streams, and a stand-in for the solver.

use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

use actix_web::HttpServer;
use actix_web::body::{BoxBody, MessageBody};
use actix_web::dev::ServerHandle;
use actix_web::http::header::{self, HeaderMap};
use actix_web::http::{Method, StatusCode};
use actix_web::test::{self, TestRequest};
use actix_web::web::{self, Bytes};
use chrono::NaiveDate;
use routeforge_domain::db::queue;
use routeforge_domain::db::{couriers, depots, orders, plans, routes, users};
use routeforge_domain::{
    Courier, Depot, Distances, JobPayload, LatLon, NewCourier, NewDepot, NewOrder, NewPlan,
    NewRoute, NewUser, OrderId, OrderKind, Plan, PlanCounts, Role, RouteBudget, User, UserId,
    WorkerId,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::task::JoinHandle;

use crate::app::{AppState, app};
use crate::auth::{Passwords, ServiceKey, Tokens};
use crate::config::Limits;
use crate::events::{Events, Signal};
use crate::logins::{self, Logins};
use crate::stream::{EVENT_STREAM, Pace};

/// The limits the application has in a test. Small, so that a test can step over them.
pub(crate) const LIMITS: Limits = Limits {
    default_budget_ms: 2_000,
    max_budget_ms: 60_000,
    max_budget_iterations: 5_000_000,
};

const SECRET: &[u8] = b"the key of the tests, 32 bytes..";

/// The key the bot shows in a test.
pub(crate) const BOT_KEY: &str = "the key of the bot in the tests..";

/// How long a login is good for in a test.
pub(crate) const TOKEN_TTL: Duration = Duration::from_secs(3_600);

/// How many logins the application of a test takes for one address within a minute:
/// what the API takes when nothing is set.
pub(crate) const LOGIN_ATTEMPTS: u32 = 5;

/// How long a token is good for in a test that the bot was given.
pub(crate) const BOT_TOKEN_TTL: Duration = Duration::from_secs(60);

/// A lease no test outlives.
pub(crate) const LONG: Duration = Duration::from_secs(600);

/// Longer than any test runs: for what must not happen by itself.
const NEVER: Duration = Duration::from_secs(3_600);

/// The pace of the event streams in a test. A stream reads again at once when it is
/// given a reason, and does nothing without one. So a test that waits for an event
/// knows what brought it, and one that is not due is not sent behind its back.
pub(crate) const PACE: Pace = Pace {
    settle: Duration::from_millis(20),
    plan_every: NEVER,
    stats_every: NEVER,
    keep_alive: NEVER,
};

/// Waits for something that must not take long, so that a test fails instead of
/// hanging.
pub(crate) async fn soon<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), future)
        .await
        .expect("this should not have taken so long")
}

/// A day in October 2026.
pub(crate) fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, day).unwrap()
}

/// The application on the database of one test.
pub(crate) struct Api {
    pool: PgPool,
    state: web::Data<AppState>,
}

/// The application on `pool`, with passwords that are cheap to hash.
///
/// Nothing listens to the database for it: see [`Api::listen`].
pub(crate) fn api(pool: &PgPool) -> Api {
    api_at(pool, PACE)
}

/// The application with event streams that pace themselves as said.
pub(crate) fn api_at(pool: &PgPool, pace: Pace) -> Api {
    api_with(pool, pace, Some(BOT_KEY))
}

/// The application as it is when it was started without a key for the bot.
pub(crate) fn api_without_a_bot(pool: &PgPool) -> Api {
    api_with(pool, PACE, None)
}

/// The application that takes so many logins for one address within so long, and
/// hashes its passwords as `passwords` does.
pub(crate) fn api_with_logins(pool: &PgPool, logins: Logins, passwords: Passwords) -> Api {
    let state = AppState {
        logins,
        passwords,
        ..state_for(pool, PACE, Some(BOT_KEY))
    };
    Api {
        pool: pool.clone(),
        state: web::Data::new(state),
    }
}

fn api_with(pool: &PgPool, pace: Pace, bot_key: Option<&str>) -> Api {
    Api {
        pool: pool.clone(),
        state: web::Data::new(state_for(pool, pace, bot_key)),
    }
}

/// What the application of a test shares, as `main` puts it together for the real one.
fn state_for(pool: &PgPool, pace: Pace, bot_key: Option<&str>) -> AppState {
    AppState {
        pool: pool.clone(),
        tokens: Tokens::new(SECRET, TOKEN_TTL),
        passwords: Passwords::cheap(),
        // As many for an address within a minute as the API takes unless it is told
        // otherwise.
        logins: Logins::new(LOGIN_ATTEMPTS, logins::WINDOW),
        bot_key: bot_key.map(ServiceKey::new),
        bot_token_ttl: BOT_TOKEN_TTL,
        limits: LIMITS,
        worker_alive: Duration::from_secs(15),
        events: Events::new(),
        pace,
    }
}

/// The API listening to the database, for as long as this is held.
pub(crate) struct Listening(JoinHandle<()>);

impl Drop for Listening {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Someone with an account and a token for it.
pub(crate) struct Actor {
    pub(crate) user: User,
    pub(crate) token: String,
}

/// A request for `path`, as `who` or as nobody.
pub(crate) fn request(method: &str, path: &str, who: Option<&Actor>) -> TestRequest {
    let method = Method::from_bytes(method.as_bytes()).unwrap();
    let request = TestRequest::default().method(method).uri(path);
    match who {
        Some(actor) => {
            request.insert_header((header::AUTHORIZATION, format!("Bearer {}", actor.token)))
        }
        None => request,
    }
}

/// What the application answered.
#[derive(Debug)]
pub(crate) struct Reply {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) content_type: Option<String>,
    /// The body as it was sent. Of an event stream, what it opened with.
    pub(crate) bytes: Bytes,
    /// The body as text.
    pub(crate) text: String,
    /// The body as JSON, or null if it is none.
    pub(crate) body: Value,
}

/// The next chunk of a body, or `None` at its end.
async fn next_chunk(body: &mut BoxBody) -> Option<Bytes> {
    let chunk = std::future::poll_fn(|context| Pin::new(&mut *body).poll_next(context)).await;
    chunk.map(|chunk| chunk.expect("a body of the API does not fail half way"))
}

/// An event stream that a test has opened.
pub(crate) struct Stream {
    body: BoxBody,
    /// What has arrived and has not been read as an event yet.
    arrived: String,
}

/// One event of a stream.
#[derive(Debug, PartialEq)]
pub(crate) struct Event {
    pub(crate) name: String,
    pub(crate) data: Value,
}

impl Stream {
    /// The next block of the stream as it was sent, be it an event or a comment.
    /// `None` when the stream has ended.
    pub(crate) async fn block(&mut self) -> Option<String> {
        loop {
            if let Some((block, rest)) = self.arrived.split_once("\n\n") {
                let block = block.to_owned();
                self.arrived = rest.to_owned();
                return Some(block);
            }
            let chunk = next_chunk(&mut self.body).await?;
            let text = std::str::from_utf8(&chunk).expect("a stream is text");
            self.arrived.push_str(text);
        }
    }

    /// The next event, or `None` when the stream has ended. Comments are passed over.
    async fn event(&mut self) -> Option<Event> {
        loop {
            let block = self.block().await?;
            if block.starts_with(':') {
                continue;
            }
            let (mut name, mut data) = (None, None);
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("event: ") {
                    name = Some(value.to_owned());
                } else if let Some(value) = line.strip_prefix("data: ") {
                    data = Some(serde_json::from_str(value).expect("the data of an event is JSON"));
                } else {
                    panic!("a line of an event that is neither its name nor its data: {line:?}");
                }
            }
            return Some(Event {
                name: name.expect("an event has a name"),
                data: data.expect("an event has data"),
            });
        }
    }

    /// The next event. It is a failure if none comes, or if the stream ends.
    pub(crate) async fn next(&mut self) -> Event {
        soon(self.event()).await.expect("the stream has ended")
    }

    /// The data of the next event, which has to be one of this name.
    pub(crate) async fn next_of(&mut self, name: &str) -> Value {
        let event = self.next().await;
        assert_eq!(event.name, name, "{}", event.data);
        event.data
    }

    /// Reads on until an event is the one `wanted`, and gives its data. What comes
    /// before it is passed over: a stream may say in two events what it could have
    /// said in one, depending on how its reads fall.
    pub(crate) async fn until(&mut self, name: &str, wanted: impl Fn(&Value) -> bool) -> Value {
        loop {
            let event = self.next().await;
            if event.name == name && wanted(&event.data) {
                return event.data;
            }
        }
    }

    /// Whether the stream sends no event for `span`.
    pub(crate) async fn is_quiet_for(&mut self, span: Duration) -> bool {
        tokio::time::timeout(span, self.event()).await.is_err()
    }

    /// Waits for the stream to end, passing over what it still sends.
    pub(crate) async fn ended(&mut self) {
        soon(async { while self.event().await.is_some() {} }).await;
    }
}

impl Api {
    /// Sends a request to the application, as [`app`] builds it for `main` too.
    ///
    /// It is built anew for each request. That costs a test little, and spares the
    /// tests a type nobody can write down: that of the service it becomes.
    pub(crate) async fn send(&self, request: TestRequest) -> Reply {
        let service = test::init_service(app(self.state.clone())).await;
        let response = test::call_service(&service, request.to_request()).await;
        let status = response.status();
        let headers = response.headers().clone();
        let content_type = headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bytes = if content_type.as_deref() == Some(EVENT_STREAM) {
            // An event stream has no end to read to. What it opens with is taken, and
            // the stream is dropped with the rest of the response.
            let mut body = response.into_body().boxed();
            next_chunk(&mut body).await.unwrap_or_default()
        } else {
            test::read_body(response).await
        };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Reply {
            status,
            headers,
            content_type,
            bytes,
            text,
            body,
        }
    }

    /// Opens an event stream as `who`. It is a failure if the answer is anything else.
    pub(crate) async fn open(&self, who: &Actor, path: &str) -> Stream {
        let service = test::init_service(app(self.state.clone())).await;
        let request = request("GET", path, Some(who)).to_request();
        let response = test::call_service(&service, request).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let content_type = response.headers().get(header::CONTENT_TYPE);
        assert_eq!(content_type.expect("a content type"), EVENT_STREAM);
        Stream {
            body: response.into_body().boxed(),
            arrived: String::new(),
        }
    }

    /// Serves the application on a port of this machine, as `main` does, for a test
    /// that has to ask over a socket. It is served until the handle is told to stop.
    pub(crate) fn serve(&self) -> (SocketAddr, ServerHandle) {
        let state = self.state.clone();
        let server = HttpServer::new(move || app(state.clone()))
            .workers(1)
            .disable_signals()
            .bind(("127.0.0.1", 0))
            .expect("a free port on this machine");
        let address = server.addrs()[0];
        let server = server.run();
        let handle = server.handle();
        tokio::spawn(server);
        (address, handle)
    }

    /// The hub of the application, for a test that feeds it by hand.
    pub(crate) fn events(&self) -> &Events {
        &self.state.events
    }

    /// Starts the one connection that listens to the database, as `main` does, and
    /// returns once it listens.
    pub(crate) async fn listen(&self) -> Listening {
        let mut subscription = self.state.events.subscribe();
        let listening = Listening(self.state.events.listen(self.pool.clone()));
        // Its first word, which it says when it has begun to listen.
        assert_eq!(soon(subscription.signals.recv()).await, Ok(Signal::Missed));
        listening
    }

    /// A token for `who` that expires in `seconds` from now.
    pub(crate) fn expiring_in(&self, who: &Actor, seconds: u64) -> Actor {
        let issued_at = jsonwebtoken::get_current_timestamp() + seconds - TOKEN_TTL.as_secs();
        Actor {
            user: who.user.clone(),
            token: self.state.tokens.issued_at(&who.user, issued_at),
        }
    }

    pub(crate) async fn get(&self, who: Option<&Actor>, path: &str) -> Reply {
        self.send(request("GET", path, who)).await
    }

    /// Sends `body` as JSON, or no body at all if it is null.
    pub(crate) async fn with_body(
        &self,
        method: &str,
        who: Option<&Actor>,
        path: &str,
        body: Value,
    ) -> Reply {
        let request = request(method, path, who);
        if body.is_null() {
            self.send(request).await
        } else {
            self.send(request.set_json(body)).await
        }
    }

    pub(crate) async fn post(&self, who: Option<&Actor>, path: &str, body: Value) -> Reply {
        self.with_body("POST", who, path, body).await
    }

    pub(crate) async fn put(&self, who: Option<&Actor>, path: &str, body: Value) -> Reply {
        self.with_body("PUT", who, path, body).await
    }

    pub(crate) async fn patch(&self, who: Option<&Actor>, path: &str, body: Value) -> Reply {
        self.with_body("PATCH", who, path, body).await
    }

    /// Asks for a token of a Telegram user, as the bot does: with the key it shows,
    /// or with whatever else `key` is.
    pub(crate) async fn telegram_login(&self, key: Option<&str>, body: Value) -> Reply {
        let request = request("POST", "/api/auth/telegram", None);
        let request = match key {
            Some(key) => request.insert_header((header::AUTHORIZATION, format!("Bearer {key}"))),
            None => request,
        };
        self.send(request.set_json(body)).await
    }

    /// A new account with this role, and a token for it.
    ///
    /// The account is stored directly, with a hash that is none: nobody logs in to it.
    pub(crate) async fn actor(&self, role: Role) -> Actor {
        let new = NewUser {
            email: format!("{}@example.com", UserId::generate()),
            password_hash: "$argon2id$not-a-real-hash".to_owned(),
            role,
        };
        let user = users::create(&self.pool, &new).await.unwrap();
        let token = self.state.tokens.issue(user.clone()).unwrap().token;
        Actor { user, token }
    }

    /// Sets a depot in Kyiv.
    pub(crate) async fn depot(&self) -> Depot {
        let new = NewDepot {
            name: "Depot".to_owned(),
            location: LatLon::new(50.45, 30.52).unwrap(),
        };
        depots::set(&self.pool, &new).await.unwrap()
    }

    pub(crate) async fn courier(&self, capacity: u32) -> Courier {
        let new = NewCourier {
            name: "Van".to_owned(),
            capacity,
        };
        couriers::create(&self.pool, &new).await.unwrap()
    }

    /// Places an order through the API, as `who`, and gives its id.
    pub(crate) async fn order(&self, who: &Actor, day: u32) -> String {
        let asked = json!({
            "kind": "delivery",
            "service_date": date(day),
            "location": { "lat": 50.4, "lon": 30.6 },
            "demand": 1,
        });
        let placed = self.post(Some(who), "/api/orders", asked).await;
        assert_eq!(placed.status, StatusCode::CREATED, "{}", placed.text);
        placed.body["id"].as_str().unwrap().to_owned()
    }
}

/// An order like those [`Api::order`] places, to store without the API.
pub(crate) fn new_order(client_id: UserId, day: u32) -> NewOrder {
    NewOrder {
        client_id,
        kind: OrderKind::Delivery,
        service_date: date(day),
        location: LatLon::new(50.4, 30.6).unwrap(),
        address: None,
        demand: 1,
    }
}

/// What a plan for that day needs, with an operator and a depot of its own, to store
/// without the API.
pub(crate) async fn new_plan(pool: &PgPool, day: u32) -> NewPlan {
    let operator = NewUser {
        email: format!("{}@example.com", UserId::generate()),
        password_hash: "$argon2id$not-a-real-hash".to_owned(),
        role: Role::Operator,
    };
    let depot = NewDepot {
        name: "Depot".to_owned(),
        location: LatLon::new(50.45, 30.52).unwrap(),
    };
    NewPlan {
        service_date: date(day),
        depot_id: depots::set(pool, &depot).await.unwrap().id,
        seed: 2026,
        budget: RouteBudget::new(None, Some(1_000)).unwrap(),
        created_by: users::create(pool, &operator).await.unwrap().id,
    }
}

/// Gives orders to a plan of that day, as clustering does: they are planned afterwards.
pub(crate) async fn take_into_a_plan(
    pool: &PgPool,
    day: u32,
    order_ids: &[impl AsRef<str>],
) -> Plan {
    let new = new_plan(pool, day).await;
    let plan = plans::create(pool, &new).await.unwrap();

    let ids: Vec<OrderId> = order_ids
        .iter()
        .map(|id| id.as_ref().parse().unwrap())
        .collect();
    let taken = orders::mark_planned(pool, plan.id, &ids).await.unwrap();
    assert_eq!(taken, ids.len() as u64);
    plan
}

/// Does the oldest job that is due, as a solver would but without searching for
/// anything. `false` if no job was due.
///
/// The API may not link the solver, so its tests have this in its place. It goes
/// through the same functions of the queue a solver does:
///
/// - A `plan` job gives every pending order of the day to the first active courier,
///   whatever that courier can carry, and enqueues the one route job.
/// - A `route` job stores its stops in the reverse of the order it was given them,
///   so that a test can tell the order of a route from the order of the orders.
pub(crate) async fn work_one(pool: &PgPool) -> bool {
    let lease = Duration::from_secs(600);
    let Some(job) = queue::claim(pool, WorkerId::generate(), lease)
        .await
        .unwrap()
    else {
        return false;
    };
    let mut completion = queue::begin_complete(pool, &job.lease)
        .await
        .unwrap()
        .expect("the job was claimed a moment ago");

    match &job.payload {
        JobPayload::Plan { plan_id } => {
            let plan = plans::get(completion.conn(), *plan_id)
                .await
                .unwrap()
                .unwrap();
            let pending = orders::lock_pending(completion.conn(), plan.service_date)
                .await
                .unwrap();
            let order_ids: Vec<OrderId> = pending.iter().map(|order| order.id).collect();
            let couriers = couriers::list_active(completion.conn()).await.unwrap();

            let routed = match couriers.first() {
                Some(courier) if !order_ids.is_empty() => {
                    orders::mark_planned(completion.conn(), plan.id, &order_ids)
                        .await
                        .unwrap();
                    let route = JobPayload::Route {
                        plan_id: plan.id,
                        courier_id: courier.id,
                        order_ids: order_ids.clone(),
                        seed: u64::from(plan.seed),
                    };
                    queue::enqueue(completion.conn(), &route, job.request_id.as_deref())
                        .await
                        .unwrap();
                    true
                }
                _ => false,
            };
            let orders_total = order_ids.len() as u32;
            let counts = PlanCounts {
                orders_total,
                orders_unassigned: if routed { 0 } else { orders_total },
                routes_total: u32::from(routed),
            };
            plans::start_routing(completion.conn(), plan.id, counts)
                .await
                .unwrap()
                .expect("a plan is queued while its plan job is held");
        }
        JobPayload::Route {
            plan_id,
            courier_id,
            order_ids,
            ..
        } => {
            let mut stops = order_ids.clone();
            stops.reverse();
            let route = NewRoute {
                plan_id: *plan_id,
                courier_id: *courier_id,
                distance_m: 1_000,
                distances: Distances::GreatCircle,
                stops,
            };
            routes::insert(completion.conn(), &route).await.unwrap();
            plans::route_finished(completion.conn(), *plan_id)
                .await
                .unwrap()
                .expect("a plan is routing while one of its route jobs is held");
        }
    }
    completion.commit().await.unwrap();
    true
}

/// Does every job there is, and those that come of them.
pub(crate) async fn work_off(pool: &PgPool) {
    while work_one(pool).await {}
}
