//! The `route` job: finds the order in which one courier calls at its stops (D2, D34).
//!
//! It has three parts. The first reads what the search needs and builds the distance
//! matrix. The second runs the search on a blocking thread, with a heartbeat beside it
//! that keeps the lease and stores the progress. The third writes the route in the
//! transaction that marks the job done.

use std::collections::HashMap;
use std::iter;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::Context as _;
use routeforge_domain::db::queue::{self, Heartbeat, Lease};
use routeforge_domain::db::{depots, orders, plans, routes};
use routeforge_domain::{
    CourierId, Depot, DistanceProvider, DistanceTable, JobProgress, LatLon, NewRoute, Order,
    OrderId, PlanId, RouteBudget,
};
use routeforge_tsp::{Control, DistanceMatrix, StopReason, Tour, optimize_route};
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::{info, warn};

use crate::config::Config;
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
    let (matrix, budget) = prepare(context, route).await?;
    let Some(found) = solve(
        &context.pool,
        &context.config,
        lease,
        matrix,
        route.seed,
        budget,
    )
    .await?
    else {
        return Ok(Outcome::Gone);
    };
    complete(&context.pool, lease, route, &found).await
}

/// Reads the depot and the stops, and asks for the distances between them.
async fn prepare<P>(
    context: &Context<P>,
    route: &Route<'_>,
) -> anyhow::Result<(DistanceMatrix, RouteBudget)>
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
    let stops = orders::list_by_ids(pool, route.order_ids).await?;

    let positions = positions(&depot, route.order_ids, &stops)?;
    let table = context
        .distances
        .table(&positions)
        .await
        .context("could not get the distances between the stops")?;
    Ok((search_matrix(table)?, plan.budget))
}

/// The depot, then the stops in the order the job lists them.
fn positions(depot: &Depot, order_ids: &[OrderId], stops: &[Order]) -> anyhow::Result<Vec<LatLon>> {
    let location: HashMap<OrderId, LatLon> = stops
        .iter()
        .map(|order| (order.id, order.location))
        .collect();
    let of_the_stops = order_ids.iter().map(|id| {
        location
            .get(id)
            .copied()
            .with_context(|| format!("order {id} of the route does not exist"))
    });
    iter::once(Ok(depot.location)).chain(of_the_stops).collect()
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
    progress: watch::Receiver<Option<JobProgress>>,
    cancel: Arc<AtomicBool>,
}

impl Search {
    fn start(matrix: DistanceMatrix, seed: u64, budget: RouteBudget) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let (progress, latest) = watch::channel(None);
        let found = tokio::task::spawn_blocking({
            let cancel = Arc::clone(&cancel);
            move || search(&matrix, seed, budget, &cancel, &progress)
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
    seed: u64,
    budget: RouteBudget,
) -> anyhow::Result<Option<Found>> {
    let mut search = Search::start(matrix, seed, budget);
    // Beside the search and not in a task of its own: the lease is kept for exactly
    // as long as this future lives, so a job that is dropped cannot go on holding it.
    let heartbeats = heartbeats(pool, config, lease, search.progress.clone());
    tokio::pin!(heartbeats);

    tokio::select! {
        found = &mut search.found => Ok(Some(found.context("the route search panicked")?)),
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

/// The search itself, on a blocking thread.
fn search(
    matrix: &DistanceMatrix,
    seed: u64,
    budget: RouteBudget,
    cancel: &AtomicBool,
    progress: &watch::Sender<Option<JobProgress>>,
) -> Found {
    // A control borrows the flag and owns the callback, so it is built here, on the
    // thread that runs the search, and not handed over to it.
    let mut control = Control::max_iterations(budget.iterations().unwrap_or(u64::MAX))
        .with_cancel(cancel)
        .with_progress(|at| {
            progress.send_replace(Some(JobProgress {
                iterations: at.iterations,
                best_cost_m: at.best_cost,
            }));
        });
    // The budget is a span, measured from the moment the search starts.
    let deadline = budget
        .time()
        .and_then(|time| Instant::now().checked_add(time));
    if let Some(deadline) = deadline {
        control = control.with_deadline(deadline);
    }

    let tour = optimize_route(matrix, seed, &mut control);
    Found {
        tour,
        iterations: control.iterations(),
        stopped_by: control.stop_reason(),
    }
}

/// Extends the lease every heartbeat interval and stores how far the search has got.
///
/// Returns when the job is no longer this worker's, with the answer that said so.
async fn heartbeats(
    pool: &PgPool,
    config: &Config,
    lease: &Lease,
    progress: watch::Receiver<Option<JobProgress>>,
) -> Heartbeat {
    let first = tokio::time::Instant::now() + config.heartbeat;
    let mut every = tokio::time::interval_at(first, config.heartbeat);
    every.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        let latest = *progress.borrow();
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
    found: &Found,
) -> anyhow::Result<Outcome> {
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

    use routeforge_domain::Haversine;
    use routeforge_tsp::Point;

    use super::*;
    use crate::testing::{self, date, moves, soon};

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

        let found = search(&matrix, 7, moves(50_000), &cancel, &progress);
        assert_eq!(found.stopped_by, Some(StopReason::Budget));
        assert!(found.iterations <= 50_000);
        let reported = (*latest.borrow()).unwrap();
        assert_eq!(reported.best_cost_m, found.tour.cost());
        assert!(reported.iterations <= found.iterations);

        // The same seed and budget give the same route, as the plan promises (D5).
        let again = search(&matrix, 7, moves(50_000), &cancel, &progress);
        assert_eq!(again.tour, found.tour);
    }

    #[test]
    fn the_search_stops_at_its_deadline_or_its_flag() {
        let matrix = scattered(12);
        let (progress, _latest) = watch::channel(None);

        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let found = search(&matrix, 7, testing::millis(50), &cancel, &progress);
        assert_eq!(found.stopped_by, Some(StopReason::Deadline));
        assert!(started.elapsed() >= Duration::from_millis(50));

        // Both limits: the one that is reached first ends the search.
        let both = RouteBudget::new(Some(60_000), Some(10_000)).unwrap();
        let found = search(&matrix, 7, both, &cancel, &progress);
        assert_eq!(found.stopped_by, Some(StopReason::Budget));

        cancel.store(true, Ordering::Relaxed);
        let found = search(&matrix, 7, testing::millis(60_000), &cancel, &progress);
        assert_eq!(found.stopped_by, Some(StopReason::Cancelled));
        assert_eq!(found.iterations, 0);
    }

    /// Waits until the search's thread has returned. The sending side of the progress
    /// lives on that thread, and is gone when the thread is.
    async fn returned(mut progress: watch::Receiver<Option<JobProgress>>) {
        soon(async { while progress.changed().await.is_ok() {} }).await;
    }

    #[tokio::test]
    async fn a_search_that_is_told_to_stop_returns_what_it_has() {
        // A minute is longer than this test waits for anything.
        let mut search = Search::start(scattered(12), 7, testing::millis(60_000));
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
        let search = Search::start(scattered(12), 7, testing::millis(60_000));
        let mut progress = search.progress.clone();
        soon(progress.changed()).await.unwrap();

        drop(search);
        returned(progress).await;
    }

    #[tokio::test]
    async fn a_job_that_is_aborted_takes_its_search_with_it() {
        let search = Search::start(scattered(12), 7, testing::millis(60_000));
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

        let (matrix, budget) = prepare(&context, &route).await.unwrap();

        assert_eq!(budget, moves(10));
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
}
