//! Shared by the database tests: the rows that other rows need, and a plan under way.

use std::time::Duration;

use chrono::NaiveDate;
use sqlx::PgPool;

use super::queue::{self, ClaimedJob};
use super::{couriers, depots, orders, plans, routes, users};
use crate::geo::{Distances, LatLon};
use crate::ids::{CourierId, JobId, OrderId, UserId};
use crate::job::JobPayload;
use crate::model::{
    Courier, Depot, NewCourier, NewDepot, NewOrder, NewPlan, NewRoute, NewUser, Order, OrderKind,
    Plan, PlanCounts, PlanStatus, Role, RouteBudget, User,
};

/// A lease no test outlives.
pub(crate) const LONG: Duration = Duration::from_secs(600);

/// A day in October 2026.
pub(crate) fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, day).unwrap()
}

/// Waits for something that must not take long, so that a wrong lock order fails a
/// test instead of hanging it.
pub(crate) async fn soon<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), future)
        .await
        .expect("this should not have had to wait")
}

pub(crate) async fn user(pool: &PgPool, role: Role) -> User {
    let new = NewUser {
        email: format!("{}@example.com", UserId::generate()),
        password_hash: "$argon2id$not-a-real-hash".to_owned(),
        role,
    };
    users::create(pool, &new).await.unwrap()
}

pub(crate) async fn depot(pool: &PgPool) -> Depot {
    let new = NewDepot {
        name: "Depot".to_owned(),
        location: LatLon::new(50.45, 30.52).unwrap(),
    };
    depots::set(pool, &new).await.unwrap()
}

pub(crate) async fn courier(pool: &PgPool, capacity: u32) -> Courier {
    let new = NewCourier {
        name: "Van".to_owned(),
        capacity,
    };
    couriers::create(pool, &new).await.unwrap()
}

pub(crate) fn new_order(client_id: UserId, service_date: NaiveDate) -> NewOrder {
    NewOrder {
        client_id,
        kind: OrderKind::Delivery,
        service_date,
        location: LatLon::new(50.40, 30.60).unwrap(),
        address: None,
        demand: 1,
    }
}

pub(crate) async fn order(pool: &PgPool, client_id: UserId, service_date: NaiveDate) -> Order {
    orders::create(pool, &new_order(client_id, service_date))
        .await
        .unwrap()
}

/// What a plan for this date needs, with an operator and a depot of its own.
pub(crate) async fn new_plan(pool: &PgPool, service_date: NaiveDate) -> NewPlan {
    NewPlan {
        service_date,
        depot_id: depot(pool).await.id,
        seed: 2026,
        budget: RouteBudget::new(None, Some(100_000)).unwrap(),
        created_by: user(pool, Role::Operator).await.id,
    }
}

/// A queued plan with no job on the queue, for tests that enqueue their own.
pub(crate) async fn plan(pool: &PgPool, service_date: NaiveDate) -> Plan {
    let new = new_plan(pool, service_date).await;
    plans::create(pool, &new).await.unwrap()
}

/// A plan as the `plan` job leaves it: clustered, its orders planned, and one route
/// job per courier on the queue, oldest first.
pub(crate) struct Routing {
    pub plan: Plan,
    pub routes: Vec<RouteWork>,
}

pub(crate) struct RouteWork {
    pub job_id: JobId,
    pub courier_id: CourierId,
    pub order_ids: Vec<OrderId>,
}

impl Routing {
    pub(crate) fn order_ids(&self) -> Vec<OrderId> {
        self.routes
            .iter()
            .flat_map(|route| route.order_ids.iter().copied())
            .collect()
    }
}

pub(crate) async fn routing(
    pool: &PgPool,
    service_date: NaiveDate,
    routes: usize,
    stops: usize,
) -> Routing {
    let plan = plan(pool, service_date).await;
    let client = user(pool, Role::Client).await;

    let mut work = Vec::new();
    for number in 0..routes {
        let courier_id = courier(pool, 100).await.id;
        let mut order_ids = Vec::new();
        for _ in 0..stops {
            order_ids.push(order(pool, client.id, service_date).await.id);
        }
        let marked = orders::mark_planned(pool, plan.id, &order_ids)
            .await
            .unwrap();
        assert_eq!(marked, stops as u64);

        let payload = JobPayload::Route {
            plan_id: plan.id,
            courier_id,
            order_ids: order_ids.clone(),
            seed: number as u64,
        };
        let job_id = queue::enqueue(pool, &payload, Some("req-1")).await.unwrap();
        work.push(RouteWork {
            job_id,
            courier_id,
            order_ids,
        });
    }

    let counts = PlanCounts {
        orders_total: (routes * stops) as u32,
        orders_unassigned: 0,
        routes_total: routes as u32,
    };
    let status = plans::start_routing(pool, plan.id, counts).await.unwrap();
    assert!(status.is_some(), "the plan was queued");

    let plan = plans::get(pool, plan.id).await.unwrap().unwrap();
    Routing { plan, routes: work }
}

/// Completes a claimed route job the way the solver will: stores the route and counts
/// it, in the transaction that marks the job done.
///
/// Returns the plan's status afterwards, or `None` if the claim no longer held and
/// nothing was stored.
pub(crate) async fn complete_route(pool: &PgPool, job: &ClaimedJob) -> Option<PlanStatus> {
    let JobPayload::Route {
        plan_id,
        courier_id,
        order_ids,
        ..
    } = &job.payload
    else {
        panic!("{:?} is not a route job", job.payload);
    };
    let mut completion = queue::begin_complete(pool, &job.lease).await.unwrap()?;

    let route = NewRoute {
        plan_id: *plan_id,
        courier_id: *courier_id,
        distance_m: 1_000,
        distances: Distances::GreatCircle,
        stops: order_ids.clone(),
    };
    routes::insert(completion.conn(), &route).await.unwrap();
    let status = plans::route_finished(completion.conn(), *plan_id)
        .await
        .unwrap()
        .expect("a plan is routing while one of its route jobs is held");
    completion.commit().await.unwrap();
    Some(status)
}
