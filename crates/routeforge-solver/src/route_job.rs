//! The `route` job: finds the order in which one courier calls at its stops (D2, D34).
//!
//! It has three parts. The first reads what the search needs: it builds the distance
//! matrix, and reckons what the courier has on board along the route (D65). The second
//! runs the search on a blocking thread. Beside it a heartbeat keeps the lease and
//! stores the progress, and an announcer tells whoever watches the plan how far the
//! search has got and with which route (D49). The third writes the route in the
//! transaction that marks the job done.

use std::collections::HashMap;
use std::convert::Infallible;
use std::iter;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Context as _, ensure};
use routeforge_domain::db::queue::{self, Heartbeat, Lease};
use routeforge_domain::db::{couriers, depots, orders, plans, routes};
use routeforge_domain::{
    CourierId, DistanceProvider, DistanceTable, JobProgress, LatLon, NewRoute, Order, OrderId,
    PlanId, RouteBudget,
};
use routeforge_tsp::{
    Control, Demand, DistanceMatrix, Loads, StopReason, Tour, optimize_route_within,
};
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::plan_job;
use crate::worker::{Context, Outcome};

/// What a route job is to do, as its payload says it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Route<'a> {
    pub(crate) plan_id: PlanId,
    pub(crate) courier_id: CourierId,
    /// The courier's stops. Node `n` of the search is stop `n - 1`, and node 0 is the
    /// depot (D14).
    pub(crate) order_ids: &'a [OrderId],
    pub(crate) seed: u64,
}

pub(crate) async fn run<P>(
    context: &Context<P>,
    lease: &Lease,
    route: &Route<'_>,
) -> anyhow::Result<Outcome>
where
    P: DistanceProvider + Sync,
{
    let Prepared {
        matrix,
        loads,
        budget,
    } = prepare(context, route).await?;
    let Some(found) = solve(
        &context.pool,
        &context.config,
        lease,
        matrix,
        loads.clone(),
        route.seed,
        budget,
    )
    .await?
    else {
        return Ok(Outcome::Gone);
    };
    complete(&context.pool, lease, route, &loads, &found).await
}

/// What the search for a route works on.
#[derive(Debug)]
struct Prepared {
    /// Node 0 is the depot, and node `n` is stop `n - 1` of the job's list (D14).
    matrix: DistanceMatrix,
    /// What the courier has on board along the route, and how much it carries (D65).
    loads: Loads,
    budget: RouteBudget,
}

/// Reads the depot, the courier and the stops, asks for the distances between them,
/// and reckons what the courier carries from stop to stop.
async fn prepare<P>(context: &Context<P>, route: &Route<'_>) -> anyhow::Result<Prepared>
where
    P: DistanceProvider + Sync,
{
    let pool = &context.pool;
    let plan = plans::get(pool, route.plan_id)
        .await?
        .with_context(|| format!("plan {} does not exist", route.plan_id))?;
    let depot = depots::get(pool, plan.depot_id)
        .await?
        .with_context(|| format!("depot {} of the plan does not exist", plan.depot_id))?;
    let courier = couriers::get(pool, route.courier_id)
        .await?
        .with_context(|| format!("courier {} of the route does not exist", route.courier_id))?;
    let stored = orders::list_by_ids(pool, route.order_ids).await?;
    let stops = in_the_order_of_the_job(route.order_ids, &stored)?;

    // Clustering gave the courier no more than it carries out, and no more than it
    // carries back (D65). A route that asks for more is not one to search for.
    let asked: Vec<Demand> = stops.iter().map(|order| plan_job::demand(order)).collect();
    let loads = Loads::new(courier.capacity, &asked)
        .with_context(|| format!("courier {} cannot drive this route", courier.id))?;

    let positions: Vec<LatLon> = iter::once(depot.location)
        .chain(stops.iter().map(|order| order.location))
        .collect();
    let table = context
        .distances
        .table(&positions)
        .await
        .context("could not get the distances between the stops")?;
    Ok(Prepared {
        matrix: search_matrix(table)?,
        loads,
        budget: plan.budget,
    })
}

/// The stops in the order the job lists them, which is the order of the search's
/// nodes: the database returns them in the order of their ids.
fn in_the_order_of_the_job<'a>(
    order_ids: &[OrderId],
    stored: &'a [Order],
) -> anyhow::Result<Vec<&'a Order>> {
    let by_id: HashMap<OrderId, &Order> = stored.iter().map(|order| (order.id, order)).collect();
    let stop = |id: &OrderId| {
        by_id
            .get(id)
            .copied()
            .with_context(|| format!("order {id} of the route does not exist"))
    };
    order_ids.iter().map(stop).collect()
}

/// The distances as the search takes them.
///
/// The search needs the way from A to B to be as long as the way back (D14), which
/// great-circle distances are. Road distances are not, and a provider of those will
/// have to make its table symmetric here.
fn search_matrix(table: DistanceTable) -> anyhow::Result<DistanceMatrix> {
    let nodes = table.nodes();
    DistanceMatrix::new(nodes, table.into_metres())
        .context("the distances between the stops cannot be searched")
}

/// What a search found, and how it ended.
#[derive(Debug)]
struct Found {
    tour: Tour,
    iterations: u64,
    stopped_by: Option<StopReason>,
}

/// What a search last said: how far it has got, and the best route it has found.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Report {
    progress: JobProgress,
    /// The stops of that route in calling order, each as its position in the job's
    /// list of orders. `None` if a stop has no such position, which the search rules out.
    order: Option<Arc<[u32]>>,
}

/// A search under way on a blocking thread.
///
/// It ends when its budget is used up, when it is told to [`stop`](Self::stop), and
/// when this value is dropped. The last is what ties the search to its job. A job's
/// future is dropped when the worker is told to stop, when its task is aborted and
/// when it panics, and a thread knows of none of these: without the flag the search
/// would go on to the end of its budget, with nobody waiting for the route.
struct Search {
    found: JoinHandle<Found>,
    /// How far the search has got, as it last said.
    progress: watch::Receiver<Option<Report>>,
    cancel: Arc<AtomicBool>,
}

impl Search {
    fn start(matrix: DistanceMatrix, loads: Loads, seed: u64, budget: RouteBudget) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let (progress, latest) = watch::channel(None);
        let found = tokio::task::spawn_blocking({
            let cancel = Arc::clone(&cancel);
            move || search(&matrix, &loads, seed, budget, &cancel, &progress)
        });
        Self {
            found,
            progress: latest,
            cancel,
        }
    }

    /// Tells the search to return. It looks at the flag every 16 384 moves (D15).
    fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Runs the search and keeps the lease while it runs.
///
/// `None` means the job was taken away meanwhile: its plan has ended, or another
/// worker has claimed it. The search was stopped and what it had found is dropped.
async fn solve(
    pool: &PgPool,
    config: &Config,
    lease: &Lease,
    matrix: DistanceMatrix,
    loads: Loads,
    seed: u64,
    budget: RouteBudget,
) -> anyhow::Result<Option<Found>> {
    let mut search = Search::start(matrix, loads, seed, budget);
    // Beside the search and not in tasks of their own: the lease is kept, and the
    // search is announced, for exactly as long as this future lives. A job that is
    // dropped can go on doing neither.
    let heartbeats = heartbeats(pool, config, lease, search.progress.clone());
    let announcing = announce(pool, config, lease, search.progress.clone());
    tokio::pin!(heartbeats, announcing);

    tokio::select! {
        found = &mut search.found => Ok(Some(found.context("the route search panicked")?)),
        never = &mut announcing => match never {},
        answer = &mut heartbeats => {
            match answer {
                Heartbeat::Cancelled => info!("the plan has ended; stopping the search"),
                _ => info!("another worker has the job; stopping the search"),
            }
            // What the search returns now is of no use to anyone.
            search.stop();
            let _ = (&mut search.found).await;
            Ok(None)
        }
    }
}

/// The search itself, on a blocking thread. Every route it reports, and the one it
/// returns, is one the courier can drive with what it carries (D65).
fn search(
    matrix: &DistanceMatrix,
    loads: &Loads,
    seed: u64,
    budget: RouteBudget,
    cancel: &AtomicBool,
    progress: &watch::Sender<Option<Report>>,
) -> Found {
    // The route last reported, with its length. The best tour changes only by getting
    // shorter, so a report of the same length is of the same route.
    let mut reported: Option<(u64, Option<Arc<[u32]>>)> = None;
    // A control borrows the flag and owns the callback, so it is built here, on the
    // thread that runs the search, and not handed over to it.
    let mut control = Control::max_iterations(budget.iterations().unwrap_or(u64::MAX))
        .with_cancel(cancel)
        .with_progress(|at, best| {
            let order = match &reported {
                Some((cost, order)) if *cost == at.best_cost => order.clone(),
                _ => positions_of(best),
            };
            reported = Some((at.best_cost, order.clone()));
            progress.send_replace(Some(Report {
                progress: JobProgress {
                    iterations: at.iterations,
                    best_cost_m: at.best_cost,
                },
                order,
            }));
        });
    // The budget is a span, measured from the moment the search starts.
    let deadline = budget
        .time()
        .and_then(|time| Instant::now().checked_add(time));
    if let Some(deadline) = deadline {
        control = control.with_deadline(deadline);
    }

    let tour = optimize_route_within(matrix, loads, seed, &mut control);
    Found {
        tour,
        iterations: control.iterations(),
        stopped_by: control.stop_reason(),
    }
}

/// The stops of a tour in calling order, each as its position in the job's list of
/// orders: node `n` of the search is stop `n - 1` (D14).
fn positions_of(tour: &Tour) -> Option<Arc<[u32]>> {
    let position = |&node: &usize| u32::try_from(node.checked_sub(1)?).ok();
    tour.stops().iter().map(position).collect()
}

/// Tells whoever watches the plan how far the search has got and with which route:
/// whenever the search has said something new, and at most every `config.progress`.
///
/// Never returns. It is polled beside the search, as the heartbeat is and for the
/// reason the heartbeat is (D34). It ends with the job's future, however that ends.
async fn announce(
    pool: &PgPool,
    config: &Config,
    lease: &Lease,
    mut reports: watch::Receiver<Option<Report>>,
) -> Infallible {
    loop {
        // The other end is on the thread of the search. Once that has returned there
        // is no more to say, and the search's own result ends the job.
        if reports.changed().await.is_err() {
            return std::future::pending().await;
        }
        let report = reports.borrow_and_update().clone();
        if let Some(report) = report {
            let said =
                queue::announce_progress(pool, lease, report.progress, report.order.as_deref());
            // Nobody waits for it, and the next one says more.
            if let Err(error) = said.await {
                debug!(%error, "could not announce the progress of the search");
            }
        }
        tokio::time::sleep(config.progress).await;
    }
}

/// Extends the lease every heartbeat interval and stores how far the search has got.
///
/// Returns when the job is no longer this worker's, with the answer that said so.
async fn heartbeats(
    pool: &PgPool,
    config: &Config,
    lease: &Lease,
    progress: watch::Receiver<Option<Report>>,
) -> Heartbeat {
    let first = tokio::time::Instant::now() + config.heartbeat;
    let mut every = tokio::time::interval_at(first, config.heartbeat);
    every.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        let latest = progress.borrow().as_ref().map(|report| report.progress);
        match queue::heartbeat(pool, lease, config.lease, latest).await {
            Ok(Heartbeat::Extended) => {}
            Ok(ended) => return ended,
            // The next one may get through. If none does the lease runs out, and
            // should another worker then claim the job, the completion is refused (D6).
            Err(error) => warn!(%error, "a heartbeat did not get through"),
        }
    }
}

/// Stores the route and counts it, in the transaction that marks the job done.
async fn complete(
    pool: &PgPool,
    lease: &Lease,
    route: &Route<'_>,
    loads: &Loads,
    found: &Found,
) -> anyhow::Result<Outcome> {
    // The search makes no move that overloads the courier. A route that did all the
    // same is refused here, where it would be stored, and not driven.
    ensure!(
        loads.fits(found.tour.stops()),
        "the route that was found has more on board than its courier carries"
    );
    let new = NewRoute {
        plan_id: route.plan_id,
        courier_id: route.courier_id,
        distance_m: found.tour.cost(),
        stops: calling_order(&found.tour, route.order_ids)?,
    };
    let Some(mut completion) = queue::begin_complete(pool, lease).await? else {
        return Ok(Outcome::Gone);
    };
    routes::insert(completion.conn(), &new).await?;
    let plan = plans::route_finished(completion.conn(), route.plan_id)
        .await?
        .context("the plan was not waiting for its routes")?;
    completion.commit().await?;

    info!(
        stops = new.stops.len(),
        distance_m = new.distance_m,
        iterations = found.iterations,
        stopped_by = ?found.stopped_by,
        %plan,
        "stored the route"
    );
    Ok(Outcome::Done)
}

/// The orders in the order the tour calls at them.
fn calling_order(tour: &Tour, order_ids: &[OrderId]) -> anyhow::Result<Vec<OrderId>> {
    let order_at = |&node: &usize| {
        node.checked_sub(1)
            .and_then(|stop| order_ids.get(stop))
            .copied()
            .with_context(|| format!("the tour calls at node {node}, which is no stop"))
    };
    tour.stops().iter().map(order_at).collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use routeforge_domain::db::queue::PROGRESS_CHANNEL;
    use routeforge_domain::{Haversine, JobPayload, OrderKind, RouteProgress, WorkerId};
    use routeforge_tsp::{Point, optimize_route};
    use sqlx::postgres::PgListener;

    use super::*;
    use crate::testing::{self, LONG, date, moves, one_at_a_time, soon};

    /// The depot and `stops` stops on a line, 100 metres apart.
    fn line(stops: usize) -> DistanceMatrix {
        let points: Vec<Point> = (0..=stops)
            .map(|at| Point::new(100.0 * at as f64, 0.0))
            .collect();
        DistanceMatrix::euclidean(&points)
    }

    /// Stops scattered so that no order of them is obviously the best.
    fn scattered(stops: usize) -> DistanceMatrix {
        let points: Vec<Point> = (0..=stops)
            .map(|at| {
                let at = at as f64;
                Point::new(1_000.0 * (at * 2.4).sin(), 1_000.0 * (at * 1.7).cos())
            })
            .collect();
        DistanceMatrix::euclidean(&points)
    }

    fn ids(count: usize) -> Vec<OrderId> {
        (0..count).map(|_| OrderId::generate()).collect()
    }

    #[test]
    fn a_tour_is_stored_as_the_orders_it_calls_at() {
        let order_ids = ids(4);
        let matrix = line(4);
        let tour = optimize_route(&matrix, 1, &mut Control::max_iterations(1_000));
        assert_eq!(tour.cost(), 800);

        let called = calling_order(&tour, &order_ids).unwrap();
        // Out along the line and back, in one direction or the other.
        let mut reversed = order_ids.clone();
        reversed.reverse();
        assert!(called == order_ids || called == reversed, "{called:?}");

        // A tour over more stops than the job has is refused, not truncated.
        let error = calling_order(&tour, &order_ids[..3]).unwrap_err();
        assert!(error.to_string().contains("no stop"), "{error}");
        assert!(calling_order(&tour, &[]).is_err());
    }

    #[test]
    fn a_route_without_stops_is_a_tour_without_stops() {
        let depot_alone = line(0);
        let tour = optimize_route(&depot_alone, 1, &mut Control::max_iterations(1));
        assert!(calling_order(&tour, &[]).unwrap().is_empty());
        assert_eq!(tour.cost(), 0);
    }

    #[test]
    fn a_table_that_differs_by_direction_is_refused() {
        let there_and_back = DistanceTable::new(2, vec![0, 5, 5, 0]).unwrap();
        assert_eq!(search_matrix(there_and_back).unwrap().get(0, 1), 5);

        let one_way_street = DistanceTable::new(2, vec![0, 5, 7, 0]).unwrap();
        let error = search_matrix(one_way_street).unwrap_err();
        assert!(format!("{error:#}").contains("differs"), "{error:#}");
    }

    #[test]
    fn the_search_stops_at_its_budget_and_says_how_far_it_got() {
        let matrix = scattered(12);
        let cancel = AtomicBool::new(false);
        let (progress, latest) = watch::channel(None);

        let found = search(
            &matrix,
            &Loads::none(12),
            7,
            moves(50_000),
            &cancel,
            &progress,
        );
        assert_eq!(found.stopped_by, Some(StopReason::Budget));
        assert!(found.iterations <= 50_000);
        let reported = latest.borrow().clone().unwrap();
        assert_eq!(reported.progress.best_cost_m, found.tour.cost());
        assert!(reported.progress.iterations <= found.iterations);

        // With the numbers comes the route they are of: every stop once, by its place
        // in the job's list, which is one less than its node in the search.
        let order = reported.order.unwrap();
        let mut places = order.to_vec();
        places.sort_unstable();
        assert!(places.into_iter().eq(0..12));
        let nodes: Vec<usize> = order.iter().map(|&place| place as usize + 1).collect();
        assert_eq!(
            nodes,
            found.tour.stops(),
            "the last report is of the result"
        );
        let round_trip: Vec<usize> = iter::once(0).chain(nodes).chain([0]).collect();
        let length: u64 = round_trip
            .windows(2)
            .map(|leg| u64::from(matrix.get(leg[0], leg[1])))
            .sum();
        assert_eq!(length, reported.progress.best_cost_m);

        // The same seed and budget give the same route, as the plan promises (D5).
        let again = search(
            &matrix,
            &Loads::none(12),
            7,
            moves(50_000),
            &cancel,
            &progress,
        );
        assert_eq!(again.tour, found.tour);
    }

    #[test]
    fn the_search_stops_at_its_deadline_or_its_flag() {
        let matrix = scattered(12);
        let (progress, _latest) = watch::channel(None);

        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let found = search(
            &matrix,
            &Loads::none(12),
            7,
            testing::millis(50),
            &cancel,
            &progress,
        );
        assert_eq!(found.stopped_by, Some(StopReason::Deadline));
        assert!(started.elapsed() >= Duration::from_millis(50));

        // Both limits: the one that is reached first ends the search.
        let both = RouteBudget::new(Some(60_000), Some(10_000)).unwrap();
        let found = search(&matrix, &Loads::none(12), 7, both, &cancel, &progress);
        assert_eq!(found.stopped_by, Some(StopReason::Budget));

        cancel.store(true, Ordering::Relaxed);
        let found = search(
            &matrix,
            &Loads::none(12),
            7,
            testing::millis(60_000),
            &cancel,
            &progress,
        );
        assert_eq!(found.stopped_by, Some(StopReason::Cancelled));
        assert_eq!(found.iterations, 0);
    }

    /// Waits until the search's thread has returned. The sending side of the progress
    /// lives on that thread, and is gone when the thread is.
    async fn returned(mut progress: watch::Receiver<Option<Report>>) {
        soon(async { while progress.changed().await.is_ok() {} }).await;
    }

    #[tokio::test]
    async fn a_search_that_is_told_to_stop_returns_what_it_has() {
        // A minute is longer than this test waits for anything.
        let mut search = Search::start(scattered(12), Loads::none(12), 7, testing::millis(60_000));
        let mut progress = search.progress.clone();
        soon(progress.changed()).await.unwrap();

        search.stop();
        let found = soon(&mut search.found).await.unwrap();
        assert_eq!(found.stopped_by, Some(StopReason::Cancelled));
        assert_eq!(found.tour.stops().len(), 12);
    }

    /// What ties a search to its job: the job's future owns the search, and however
    /// that future ends, the thread does not go on searching.
    #[tokio::test]
    async fn a_search_that_is_dropped_stops() {
        let search = Search::start(scattered(12), Loads::none(12), 7, testing::millis(60_000));
        let mut progress = search.progress.clone();
        soon(progress.changed()).await.unwrap();

        drop(search);
        returned(progress).await;
    }

    #[tokio::test]
    async fn a_job_that_is_aborted_takes_its_search_with_it() {
        let search = Search::start(scattered(12), Loads::none(12), 7, testing::millis(60_000));
        let progress = search.progress.clone();
        // A task that owns the search, as a job's does, and never gets to its end.
        let job = tokio::spawn(async move {
            let _search = search;
            std::future::pending::<()>().await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        job.abort();
        returned(progress).await;
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn the_matrix_has_the_depot_first_and_the_stops_in_the_order_of_the_job(pool: PgPool) {
        let city = testing::city(&pool, &[10]).await;
        let placed = city.orders(&pool, date(12), 4, 1).await;
        let plan = plans::create(&pool, &city.new_plan(date(12), moves(10)))
            .await
            .unwrap();
        // Not the order of their ids, which is the order the database returns.
        let listed: Vec<&Order> = [2, 0, 3, 1].map(|at: usize| &placed[at]).to_vec();
        let order_ids: Vec<OrderId> = listed.iter().map(|order| order.id).collect();
        let route = Route {
            plan_id: plan.id,
            courier_id: city.couriers[0].id,
            order_ids: &order_ids,
            seed: 1,
        };
        let context = testing::context(&pool, testing::config());

        let Prepared {
            matrix,
            loads,
            budget,
        } = prepare(&context, &route).await.unwrap();

        assert_eq!(budget, moves(10));
        // Four deliveries of one, on a courier that carries ten.
        assert_eq!(loads, Loads::new(10, &[Demand::delivery(1); 4]).unwrap());
        assert_eq!(matrix.len(), 5);
        let expected: Vec<LatLon> = iter::once(city.depot.location)
            .chain(listed.iter().map(|order| order.location))
            .collect();
        let Ok(table) = Haversine.table(&expected).await;
        for from in 0..5 {
            for to in 0..5 {
                assert_eq!(matrix.get(from, to), table.get(from, to), "{from} to {to}");
            }
        }
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_route_with_an_order_that_does_not_exist_is_an_error(pool: PgPool) {
        let city = testing::city(&pool, &[10]).await;
        let placed = city.orders(&pool, date(12), 2, 1).await;
        let plan = plans::create(&pool, &city.new_plan(date(12), moves(10)))
            .await
            .unwrap();
        let missing = OrderId::generate();
        let order_ids = [placed[0].id, missing, placed[1].id];
        let route = Route {
            plan_id: plan.id,
            courier_id: city.couriers[0].id,
            order_ids: &order_ids,
            seed: 1,
        };
        let context = testing::context(&pool, testing::config());

        let error = prepare(&context, &route).await.unwrap_err();
        assert!(error.to_string().contains(&missing.to_string()), "{error}");
    }

    /// The depot and three orders on the corners of a rectangle, for a courier that
    /// carries two: the order opposite the depot is a delivery of two, and its
    /// neighbours are a pickup of one each.
    async fn rectangle(pool: &PgPool) -> (testing::City, Vec<Order>) {
        let city = testing::city(pool, &[2]).await;
        let mut placed = Vec::new();
        for (off, kind, demand) in [
            ((0.0, 0.01), OrderKind::Pickup, 1),
            ((0.01, 0.01), OrderKind::Delivery, 2),
            ((0.01, 0.0), OrderKind::Pickup, 1),
        ] {
            placed.push(city.order_at(pool, date(12), off, kind, demand).await);
        }
        (city, placed)
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_route_is_searched_within_what_its_courier_carries(pool: PgPool) {
        let (city, placed) = rectangle(&pool).await;
        let plan = plans::create(&pool, &city.new_plan(date(12), moves(1_000)))
            .await
            .unwrap();
        let order_ids: Vec<OrderId> = placed.iter().map(|order| order.id).collect();
        let route = Route {
            plan_id: plan.id,
            courier_id: city.couriers[0].id,
            order_ids: &order_ids,
            seed: 1,
        };
        let context = testing::context(&pool, testing::config());

        let Prepared {
            matrix,
            loads,
            budget,
        } = prepare(&context, &route).await.unwrap();
        // What each order asks for, in the order of the job.
        let asked = [Demand::pickup(1), Demand::delivery(2), Demand::pickup(1)];
        assert_eq!(loads, Loads::new(2, &asked).unwrap());

        let cancel = AtomicBool::new(false);
        let (progress, latest) = watch::channel(None);
        let found = search(&matrix, &loads, 1, budget, &cancel, &progress);

        // Round the rectangle is the shortest way, and it comes to a pickup first
        // whichever way it is driven. The courier leaves full, with the delivery.
        let round = optimize_route(&matrix, 1, &mut Control::max_iterations(1_000));
        assert!(!loads.fits(round.stops()));
        assert!(loads.fits(found.tour.stops()));
        assert!(found.tour.cost() > round.cost());
        let called = calling_order(&found.tour, &order_ids).unwrap();
        assert_eq!(called[0], placed[1].id, "the delivery comes first");
        // What was announced on the way is a route the courier can drive as well.
        let reported = latest.borrow().clone().unwrap();
        let nodes: Vec<usize> = reported
            .order
            .unwrap()
            .iter()
            .map(|&place| place as usize + 1)
            .collect();
        assert!(loads.fits(&nodes));
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_route_its_courier_cannot_carry_is_an_error(pool: PgPool) {
        // Three deliveries of one for a courier that carries two. Clustering never
        // gives a courier that much, so a job that says so is not to be believed.
        let city = testing::city(&pool, &[2]).await;
        let placed = city.orders(&pool, date(12), 3, 1).await;
        let plan = plans::create(&pool, &city.new_plan(date(12), moves(10)))
            .await
            .unwrap();
        let order_ids: Vec<OrderId> = placed.iter().map(|order| order.id).collect();
        let route = Route {
            plan_id: plan.id,
            courier_id: city.couriers[0].id,
            order_ids: &order_ids,
            seed: 1,
        };
        let context = testing::context(&pool, testing::config());

        let error = prepare(&context, &route).await.unwrap_err();
        let said = format!("{error:#}");
        assert!(said.contains("deliveries"), "{said}");
        assert!(said.contains(&city.couriers[0].id.to_string()), "{said}");

        // Two pickups of two are as much too much, the other way.
        let mut heavy = Vec::new();
        for _ in 0..2 {
            let order = city
                .order_at(&pool, date(13), (0.01, 0.01), OrderKind::Pickup, 2)
                .await;
            heavy.push(order.id);
        }
        let route = Route {
            order_ids: &heavy,
            ..route
        };
        let error = prepare(&context, &route).await.unwrap_err();
        assert!(format!("{error:#}").contains("pickups"), "{error:#}");
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_route_for_a_courier_that_does_not_exist_is_an_error(pool: PgPool) {
        let city = testing::city(&pool, &[10]).await;
        let placed = city.orders(&pool, date(12), 2, 1).await;
        let plan = plans::create(&pool, &city.new_plan(date(12), moves(10)))
            .await
            .unwrap();
        let order_ids: Vec<OrderId> = placed.iter().map(|order| order.id).collect();
        let nobody = CourierId::generate();
        let route = Route {
            plan_id: plan.id,
            courier_id: nobody,
            order_ids: &order_ids,
            seed: 1,
        };
        let context = testing::context(&pool, testing::config());

        let error = prepare(&context, &route).await.unwrap_err();
        assert!(error.to_string().contains(&nobody.to_string()), "{error}");
    }

    /// The last line of defence: a route that has the courier over its capacity is
    /// not stored, whatever found it.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_route_that_overloads_its_courier_is_not_stored(pool: PgPool) {
        let lease = claimed_route(&pool, 3).await;
        // A tour over three stops, and loads it does not fit: the first stop it calls
        // at is a pickup, on a courier that leaves full.
        let matrix = line(3);
        let tour = optimize_route(&matrix, 1, &mut Control::max_iterations(100));
        let first = tour.stops()[0];
        let mut asked = [Demand::delivery(1); 3];
        asked[first - 1] = Demand::pickup(2);
        let loads = Loads::new(2, &asked).unwrap();
        assert!(!loads.fits(tour.stops()));

        let found = Found {
            tour,
            iterations: 0,
            stopped_by: None,
        };
        let order_ids = ids(3);
        let route = Route {
            plan_id: lease.plan_id,
            courier_id: CourierId::generate(),
            order_ids: &order_ids,
            seed: 1,
        };
        let error = complete(&pool, &lease, &route, &loads, &found)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("more on board"), "{error}");
        assert!(
            routes::list_for_plan(&pool, lease.plan_id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// A route job of `stops` stops, claimed, for the lease a search is kept under.
    async fn claimed_route(pool: &PgPool, stops: usize) -> Lease {
        let city = testing::city(pool, &[100]).await;
        let placed = city.orders(pool, date(12), stops, 1).await;
        let plan = plans::create(pool, &city.new_plan(date(12), moves(1)))
            .await
            .unwrap();
        let route = JobPayload::Route {
            plan_id: plan.id,
            courier_id: city.couriers[0].id,
            order_ids: placed.iter().map(|order| order.id).collect(),
            seed: 7,
        };
        queue::enqueue(pool, &route, None).await.unwrap();
        let job = queue::claim(pool, WorkerId::generate(), LONG)
            .await
            .unwrap();
        job.expect("the job was enqueued a moment ago").lease
    }

    async fn listening_for_progress(pool: &PgPool) -> PgListener {
        let mut listener = PgListener::connect_with(pool).await.unwrap();
        listener.listen(PROGRESS_CHANNEL).await.unwrap();
        listener
    }

    /// What is announced within `span` from now.
    async fn announced_within(listener: &mut PgListener, span: Duration) -> Vec<RouteProgress> {
        let until = tokio::time::Instant::now() + span;
        let mut said = Vec::new();
        while let Ok(notification) = tokio::time::timeout_at(until, listener.recv()).await {
            said.push(serde_json::from_str(notification.unwrap().payload()).unwrap());
        }
        said
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_search_under_way_is_announced_and_no_more_often_than_it_is_set_to(pool: PgPool) {
        let _turn = one_at_a_time().await;
        let lease = claimed_route(&pool, 12).await;
        let config = Config {
            progress: Duration::from_millis(100),
            ..testing::config()
        };
        let mut listener = listening_for_progress(&pool).await;

        let started = Instant::now();
        let budget = testing::millis(1_000);
        let found = solve(
            &pool,
            &config,
            &lease,
            scattered(12),
            Loads::none(12),
            7,
            budget,
        )
        .await
        .unwrap()
        .expect("the job is this worker's throughout");
        let took = started.elapsed();
        let said = announced_within(&mut listener, Duration::from_millis(300)).await;

        // One as soon as the search has a tour, and then one every 100 ms at most.
        let most = 1 + took.as_millis() / 100;
        assert!(
            said.len() as u128 <= most,
            "{} announcements in {took:?}",
            said.len()
        );
        assert!(said.len() >= 3, "{} announcements in {took:?}", said.len());
        for progress in &said {
            assert_eq!(
                (progress.plan_id, progress.job_id, progress.attempt),
                (lease.plan_id, lease.job_id, lease.attempt)
            );
            let order = progress.order.clone();
            let mut places = order.expect("a route of twelve stops fits a notification");
            places.sort_unstable();
            assert!(places.into_iter().eq(0..12));
        }
        assert!(said.is_sorted_by_key(|progress| progress.iterations));
        assert!(
            said.windows(2)
                .all(|pair| pair[1].best_cost_m <= pair[0].best_cost_m)
        );
        // The search came back with a route no longer than the last one announced.
        assert!(said.last().unwrap().best_cost_m >= found.tour.cost());
    }

    /// The announcer is a part of the job's future, and goes when the job goes.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_job_that_is_dropped_is_no_longer_announced(pool: PgPool) {
        let _turn = one_at_a_time().await;
        let lease = claimed_route(&pool, 12).await;
        let config = Config {
            progress: Duration::from_millis(50),
            ..testing::config()
        };
        let mut listener = listening_for_progress(&pool).await;
        // A minute is longer than this test waits for anything.
        let job = tokio::spawn({
            let pool = pool.clone();
            let budget = testing::millis(60_000);
            async move {
                solve(
                    &pool,
                    &config,
                    &lease,
                    scattered(12),
                    Loads::none(12),
                    7,
                    budget,
                )
                .await
            }
        });
        soon(listener.recv()).await.unwrap();

        job.abort();
        assert!(job.await.unwrap_err().is_cancelled());

        // What was on its way may still arrive. After that there is nothing.
        announced_within(&mut listener, Duration::from_millis(300)).await;
        let late = announced_within(&mut listener, Duration::from_millis(500)).await;
        assert!(
            late.is_empty(),
            "{} announcements with nobody searching",
            late.len()
        );
    }
}
