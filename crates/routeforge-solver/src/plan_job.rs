//! The `plan` job: splits the pending orders of a service date into one route job per
//! courier (D2, D36).
//!
//! All of it happens inside the transaction that marks the job done. The orders are
//! locked while they are counted into clusters, so none can be withdrawn in between
//! (D20), and the route jobs appear, and are announced, in the moment the plan
//! becomes `routing`.

use anyhow::{Context, ensure};
use routeforge_domain::db::queue::{self, ClaimedJob};
use routeforge_domain::db::{couriers, depots, orders, plans};
use routeforge_domain::{
    Courier, Depot, JobPayload, Order, OrderId, Plan, PlanCounts, PlanId, Projection,
};
use routeforge_tsp::{Cluster, Clustering, Point, Problem, Stop, cluster};
use sqlx::PgPool;
use tracing::info;

use crate::worker::Outcome;

pub(crate) async fn run(
    pool: &PgPool,
    job: &ClaimedJob,
    plan_id: PlanId,
) -> anyhow::Result<Outcome> {
    // From here on the job's row is locked, and no other worker can claim it however
    // long this takes. That is why this job sends no heartbeat.
    let Some(mut completion) = queue::begin_complete(pool, &job.lease).await? else {
        return Ok(Outcome::Gone);
    };

    let plan = plans::get(completion.conn(), plan_id)
        .await?
        .with_context(|| format!("plan {plan_id} does not exist"))?;
    let depot = depots::get(completion.conn(), plan.depot_id)
        .await?
        .with_context(|| format!("depot {} of the plan does not exist", plan.depot_id))?;
    let couriers = couriers::list_active(completion.conn()).await?;
    let pending = orders::lock_pending(completion.conn(), plan.service_date).await?;

    let problem = problem(&depot, &couriers, &pending);
    let clustering = tokio::task::spawn_blocking(move || cluster(&problem))
        .await
        .context("the clustering panicked")?;
    let routes = route_jobs(&plan, &couriers, &pending, &clustering)?;

    let taken: Vec<OrderId> = routes.iter().flat_map(order_ids).copied().collect();
    let marked = orders::mark_planned(completion.conn(), plan_id, &taken).await?;
    ensure!(
        u64::try_from(taken.len()).is_ok_and(|taken| taken == marked),
        "{} orders were clustered and {marked} could be taken",
        taken.len()
    );
    for route in &routes {
        queue::enqueue(completion.conn(), route, job.request_id.as_deref()).await?;
    }
    let counts = PlanCounts {
        orders_total: count(pending.len())?,
        orders_unassigned: count(clustering.unassigned.len())?,
        routes_total: count(routes.len())?,
    };
    let status = plans::start_routing(completion.conn(), plan_id, counts)
        .await?
        .context("the plan was not waiting to be clustered")?;
    completion.commit().await?;

    info!(
        orders = counts.orders_total,
        unassigned = counts.orders_unassigned,
        routes = counts.routes_total,
        plan = %status,
        "clustered the plan"
    );
    Ok(Outcome::Done)
}

/// The day as the sweep sees it: on a flat map with the depot in the middle (D29).
/// Stop `n` is order `n`, and courier `n` is courier `n`.
fn problem(depot: &Depot, couriers: &[Courier], orders: &[Order]) -> Problem {
    let map = Projection::centered_on(depot.location);
    let stops = orders.iter().map(|order| {
        let at = map.project(order.location);
        Stop {
            point: Point::new(at.x, at.y),
            demand: order.demand,
        }
    });
    Problem {
        depot: Point::new(0.0, 0.0),
        stops: stops.collect(),
        capacities: couriers.iter().map(|courier| courier.capacity).collect(),
    }
}

/// One route job for each cluster, in the order of the clusters.
fn route_jobs(
    plan: &Plan,
    couriers: &[Courier],
    orders: &[Order],
    clustering: &Clustering,
) -> anyhow::Result<Vec<JobPayload>> {
    let route = |(number, cluster): (usize, &Cluster)| -> anyhow::Result<JobPayload> {
        let courier = couriers
            .get(cluster.courier)
            .context("the clustering names a courier that was not offered")?;
        let order_ids = cluster
            .stops
            .iter()
            .map(|&stop| orders.get(stop).map(|order| order.id))
            .collect::<Option<Vec<OrderId>>>()
            .context("the clustering names a stop that was not offered")?;
        Ok(JobPayload::Route {
            plan_id: plan.id,
            courier_id: courier.id,
            order_ids,
            seed: route_seed(plan.seed, count(number)?),
        })
    };
    clustering.clusters.iter().enumerate().map(route).collect()
}

/// The seed of the search for one route: the plan's seed in the high half and the
/// number of the cluster in the low half.
///
/// Clustering gives the same clusters in the same order for the same orders and
/// couriers, so a plan that is run again with its seed gives every route its seed
/// again (D5).
fn route_seed(plan_seed: u32, cluster: u32) -> u64 {
    u64::from(plan_seed) << 32 | u64::from(cluster)
}

fn order_ids(route: &JobPayload) -> &[OrderId] {
    match route {
        JobPayload::Route { order_ids, .. } => order_ids,
        JobPayload::Plan { .. } => &[],
    }
}

/// The plan's counters are a `u32` each.
fn count(number: usize) -> anyhow::Result<u32> {
    u32::try_from(number).context("more than a plan can count")
}

#[cfg(test)]
mod tests {
    use routeforge_domain::db::queue::MAX_ATTEMPTS;
    use routeforge_domain::db::routes;
    use routeforge_domain::{JobStatus, OrderStatus, PlanStatus, WorkerId};

    use super::*;
    use crate::testing::{self, LONG, date, moves};

    #[test]
    fn a_route_seed_holds_the_plan_seed_and_the_number_of_the_cluster() {
        assert_eq!(route_seed(0, 0), 0);
        assert_eq!(route_seed(0, 7), 7);
        assert_eq!(route_seed(1, 0), 1 << 32);
        assert_eq!(route_seed(u32::MAX, u32::MAX), u64::MAX);
        assert_ne!(route_seed(2026, 1), route_seed(2026, 2));
        assert_ne!(route_seed(2026, 1), route_seed(2027, 1));
    }

    /// Submits a plan for the date and claims its `plan` job.
    async fn claimed_plan(pool: &PgPool, city: &testing::City, day: u32) -> (Plan, ClaimedJob) {
        let new = city.new_plan(date(day), moves(1_000));
        let plan = plans::submit(pool, &new, Some("req-plan")).await.unwrap();
        let job = queue::claim(pool, WorkerId::generate(), LONG)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.payload, JobPayload::Plan { plan_id: plan.id });
        (plan, job)
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_plan_is_split_into_a_route_job_for_each_courier_it_needs(pool: PgPool) {
        let city = testing::city(&pool, &[4, 4, 4]).await;
        let placed = city.orders(&pool, date(12), 7, 1).await;
        // On another date, and not this plan's business.
        let tomorrow = city.orders(&pool, date(13), 3, 2).await;
        let (plan, job) = claimed_plan(&pool, &city, 12).await;

        assert_eq!(run(&pool, &job, plan.id).await.unwrap(), Outcome::Done);

        let plan = plans::get(&pool, plan.id).await.unwrap().unwrap();
        assert_eq!(plan.status, PlanStatus::Routing);
        assert_eq!(
            (
                plan.orders_total,
                plan.orders_unassigned,
                plan.routes_total,
                plan.routes_done
            ),
            (7, 0, 2, 0)
        );

        let jobs = queue::list_for_plan(&pool, plan.id).await.unwrap();
        assert_eq!(jobs.len(), 3);
        assert_eq!(jobs[0].status, JobStatus::Done);
        let mut routed = Vec::new();
        for (number, job) in jobs[1..].iter().enumerate() {
            let JobPayload::Route {
                plan_id,
                courier_id,
                order_ids,
                seed,
            } = &job.payload
            else {
                panic!("{:?} is not a route job", job.payload);
            };
            assert_eq!(*plan_id, plan.id);
            assert!(
                city.couriers
                    .iter()
                    .any(|courier| courier.id == *courier_id)
            );
            assert!(order_ids.len() <= 4, "a courier carries four orders of one");
            assert_eq!(*seed, route_seed(plan.seed, number as u32));
            assert_eq!(
                (job.status, job.attempt, job.max_attempts),
                (JobStatus::Queued, 0, MAX_ATTEMPTS)
            );
            // The request that asked for the plan can be followed into every route.
            assert_eq!(job.request_id.as_deref(), Some("req-plan"));
            routed.extend(order_ids.iter().copied());
        }
        routed.sort_unstable();
        let expected: Vec<OrderId> = placed.iter().map(|order| order.id).collect();
        assert_eq!(routed, expected, "every order is in one route job");

        for order in placed {
            let stored = orders::get(&pool, order.id).await.unwrap().unwrap();
            assert_eq!(
                (stored.status, stored.plan_id),
                (OrderStatus::Planned, Some(plan.id))
            );
        }
        for order in tomorrow {
            let stored = orders::get(&pool, order.id).await.unwrap().unwrap();
            assert_eq!(stored.status, OrderStatus::Pending);
        }
        // The routes are for the route jobs to write.
        assert!(
            routes::list_for_plan(&pool, plan.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn an_order_that_fits_no_courier_stays_pending_and_is_counted(pool: PgPool) {
        let city = testing::city(&pool, &[4, 2]).await;
        let fitting = city.orders(&pool, date(12), 5, 1).await;
        let too_large = city.order(&pool, date(12), 5).await;
        let (plan, job) = claimed_plan(&pool, &city, 12).await;

        assert_eq!(run(&pool, &job, plan.id).await.unwrap(), Outcome::Done);

        let plan = plans::get(&pool, plan.id).await.unwrap().unwrap();
        assert_eq!(
            (plan.orders_total, plan.orders_unassigned, plan.routes_total),
            (6, 1, 2)
        );
        let left = orders::get(&pool, too_large.id).await.unwrap().unwrap();
        assert_eq!((left.status, left.plan_id), (OrderStatus::Pending, None));
        for order in fitting {
            let stored = orders::get(&pool, order.id).await.unwrap().unwrap();
            assert_eq!(stored.status, OrderStatus::Planned);
        }
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_plan_with_nothing_to_route_is_done_at_once(pool: PgPool) {
        // Orders and nobody to carry them.
        let city = testing::city(&pool, &[]).await;
        let waiting = city.orders(&pool, date(12), 3, 1).await;
        let (plan, job) = claimed_plan(&pool, &city, 12).await;
        assert_eq!(run(&pool, &job, plan.id).await.unwrap(), Outcome::Done);

        let plan = plans::get(&pool, plan.id).await.unwrap().unwrap();
        assert_eq!(plan.status, PlanStatus::Done);
        assert_eq!(
            (plan.orders_total, plan.orders_unassigned, plan.routes_total),
            (3, 3, 0)
        );
        assert!(plan.finished_at.is_some());
        for order in waiting {
            let stored = orders::get(&pool, order.id).await.unwrap().unwrap();
            assert_eq!(stored.status, OrderStatus::Pending);
        }

        // A date with no orders at all.
        let (plan, job) = claimed_plan(&pool, &city, 13).await;
        assert_eq!(run(&pool, &job, plan.id).await.unwrap(), Outcome::Done);
        let plan = plans::get(&pool, plan.id).await.unwrap().unwrap();
        assert_eq!((plan.status, plan.orders_total), (PlanStatus::Done, 0));
        assert_eq!(queue::list_for_plan(&pool, plan.id).await.unwrap().len(), 1);
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_plan_that_was_cancelled_is_not_split(pool: PgPool) {
        let city = testing::city(&pool, &[4]).await;
        let placed = city.orders(&pool, date(12), 3, 1).await;
        let (plan, job) = claimed_plan(&pool, &city, 12).await;
        assert!(queue::cancel_plan(&pool, plan.id).await.unwrap());

        assert_eq!(run(&pool, &job, plan.id).await.unwrap(), Outcome::Gone);

        let plan = plans::get(&pool, plan.id).await.unwrap().unwrap();
        assert_eq!(plan.status, PlanStatus::Cancelled);
        assert_eq!(queue::list_for_plan(&pool, plan.id).await.unwrap().len(), 1);
        for order in placed {
            let stored = orders::get(&pool, order.id).await.unwrap().unwrap();
            assert_eq!(stored.status, OrderStatus::Pending);
        }
    }

    /// The whole job is one transaction. One that fails half way leaves nothing behind.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_plan_job_that_fails_leaves_no_trace(pool: PgPool) {
        let city = testing::city(&pool, &[4]).await;
        let placed = city.orders(&pool, date(12), 3, 1).await;
        let (plan, job) = claimed_plan(&pool, &city, 12).await;
        // The plan is clustered already, as if this job had run before.
        let counts = PlanCounts {
            orders_total: 0,
            orders_unassigned: 0,
            routes_total: 1,
        };
        plans::start_routing(&pool, plan.id, counts).await.unwrap();

        let error = run(&pool, &job, plan.id).await.unwrap_err();
        assert!(error.to_string().contains("not waiting"), "{error:#}");

        assert_eq!(queue::list_for_plan(&pool, plan.id).await.unwrap().len(), 1);
        for order in placed {
            let stored = orders::get(&pool, order.id).await.unwrap().unwrap();
            assert_eq!(stored.status, OrderStatus::Pending);
        }
        let held = queue::get(&pool, job.lease.job_id).await.unwrap().unwrap();
        assert_eq!(held.status, JobStatus::Running);
    }
}
