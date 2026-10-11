//! Shared by the tests: the application inside a test, people with tokens, a small
//! JSON client, and a stand-in for the solver.

use std::time::Duration;

use actix_web::http::header::{self, HeaderMap};
use actix_web::http::{Method, StatusCode};
use actix_web::test::{self, TestRequest};
use actix_web::web;
use chrono::NaiveDate;
use routeforge_domain::db::queue;
use routeforge_domain::db::{couriers, depots, orders, plans, routes, users};
use routeforge_domain::{
    Courier, Depot, JobPayload, LatLon, NewCourier, NewDepot, NewOrder, NewPlan, NewRoute, NewUser,
    OrderId, OrderKind, Plan, PlanCounts, Role, RouteBudget, User, UserId, WorkerId,
};
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::app::{AppState, app};
use crate::auth::{Passwords, Tokens};
use crate::config::Limits;

/// The limits the application has in a test. Small, so that a test can step over them.
pub(crate) const LIMITS: Limits = Limits {
    default_budget_ms: 2_000,
    max_budget_ms: 60_000,
    max_budget_iterations: 5_000_000,
};

const SECRET: &[u8] = b"the key of the tests, 32 bytes..";

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
pub(crate) fn api(pool: &PgPool) -> Api {
    let state = web::Data::new(AppState {
        pool: pool.clone(),
        tokens: Tokens::new(SECRET, Duration::from_secs(3_600)),
        passwords: Passwords::cheap(),
        limits: LIMITS,
        worker_alive: Duration::from_secs(15),
    });
    Api {
        pool: pool.clone(),
        state,
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
    /// The body as it was sent.
    pub(crate) text: String,
    /// The body as JSON, or null if it is none.
    pub(crate) body: Value,
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
        let bytes = test::read_body(response).await;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Reply {
            status,
            headers,
            content_type,
            text,
            body,
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

/// Gives orders to a plan of that day, as clustering does: they are planned afterwards.
pub(crate) async fn take_into_a_plan(
    pool: &PgPool,
    day: u32,
    order_ids: &[impl AsRef<str>],
) -> Plan {
    let operator = NewUser {
        email: format!("{}@example.com", UserId::generate()),
        password_hash: "$argon2id$not-a-real-hash".to_owned(),
        role: Role::Operator,
    };
    let depot = NewDepot {
        name: "Depot".to_owned(),
        location: LatLon::new(50.45, 30.52).unwrap(),
    };
    let new = NewPlan {
        service_date: date(day),
        depot_id: depots::set(pool, &depot).await.unwrap().id,
        seed: 2026,
        budget: RouteBudget::new(None, Some(1_000)).unwrap(),
        created_by: users::create(pool, &operator).await.unwrap().id,
    };
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
