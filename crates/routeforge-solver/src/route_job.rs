//! The `route` job: finds the order in which one courier calls at its stops (D2, D34).
//!
//! It has three parts. The first reads what the search needs: it asks for the
//! distances between the stops, and reckons what the courier has on board along the
//! route (D65). The second runs the search on a blocking thread. Beside it a heartbeat
//! keeps the lease and stores the progress, and an announcer tells whoever watches the
//! plan how far the search has got and with which route (D49). The third writes the
//! route in the transaction that marks the job done.
//!
//! The distances come from whatever the solver was started with: great circles, or a
//! routing service that knows the roads (D66). Roads differ from great circles in
//! three ways, and each has its place here:
//!
//! - **The service can fail**, or take its time. It is waited for with the heartbeat
//!   beside it, and a route it gives no distances for is searched for on great circles.
//!   The route says which of the two its length was measured along.
//! - **The way there is not the way back.** The search needs distances that are the
//!   same both ways (D14), so it works on the mean of the two.
//! - **So a tour is not as long as the same tour the other way round.** Both are
//!   measured on the roads as they run, and the shorter is the route, if the courier
//!   can drive it that way round with what it carries.
//!
//! A route by road also has a way along the streets, which a map can draw (D73). It is
//! asked for last, when it is settled which way round the route is driven, and a route
//! that gets none is stored without it.

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
    CourierId, DistanceProvider, DistanceTable, Distances, JobProgress, LatLon, NewRoute, Order,
    OrderId, PlanId, Polyline, RouteBudget,
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
    let Some(prepared) = prepare(context, lease, route).await? else {
        return Ok(Outcome::Gone);
    };
    let Prepared {
        positions,
        table,
        matrix,
        loads,
        budget,
        distances,
    } = prepared;
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
    let mut driven = Driven::of(&table, &loads, found.tour.stops(), distances);
    if let Some(round) = driven.round(&positions) {
        let asked = context.distances.path(&round);
        let Some(answered) =
            with_the_lease_kept(&context.pool, &context.config, lease, asked).await
        else {
            return Ok(Outcome::Gone);
        };
        match answered {
            Ok(path) => driven.path = path,
            // The route is found and measured, and is none the worse for this. It is
            // stored without its way, and whoever draws it draws it from stop to stop.
            Err(error) => warn!(
                %error,
                stops = driven.stops.len(),
                "could not get the way of the route along the streets; storing it without"
            ),
        }
    }
    complete(&context.pool, lease, route, &loads, &driven, &found).await
}

/// What the search for a route works on, and what its result is measured by.
#[derive(Debug)]
struct Prepared {
    /// Where the depot and the stops are, in the order of the nodes: node 0 is the
    /// depot, and node `n` is stop `n - 1` of the job's list (D14).
    positions: Vec<LatLon>,
    /// The distances as they were given: from each node to each other, which by road
    /// is not the distance back.
    table: DistanceTable,
    /// The same distances as the search takes them: the same both ways.
    matrix: DistanceMatrix,
    /// What the courier has on board along the route, and how much it carries (D65).
    loads: Loads,
    budget: RouteBudget,
    /// What the distances were measured along.
    distances: Distances,
}

/// Reads the depot, the courier and the stops, asks for the distances between them,
/// and reckons what the courier carries from stop to stop.
///
/// `None` means the job was taken away while the distances were waited for: its plan
/// has ended, or another worker has claimed it.
async fn prepare<P>(
    context: &Context<P>,
    lease: &Lease,
    route: &Route<'_>,
) -> anyhow::Result<Option<Prepared>>
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
    let asked = context.distances.table(&positions);
    let Some(answered) = with_the_lease_kept(pool, &context.config, lease, asked).await else {
        return Ok(None);
    };
    let (table, distances) = match answered {
        Ok(table) => (table, context.distances.measures()),
        // Great circles need nothing and nobody, and a route by them is a route. The
        // one that is stored says what it was measured along, so nobody takes it for
        // a route by road.
        Err(error) => {
            warn!(
                %error,
                stops = stops.len(),
                "could not get the distances between the stops; searching on great circles"
            );
            (DistanceTable::haversine(&positions), Distances::GreatCircle)
        }
    };
    ensure!(
        table.nodes() == positions.len(),
        "asked for the distances between {} positions and was given those between {}",
        positions.len(),
        table.nodes()
    );
    Ok(Some(Prepared {
        matrix: search_matrix(&table),
        positions,
        table,
        loads,
        budget: plan.budget,
        distances,
    }))
}

/// Waits for `work` with the job's heartbeat beside it (D34). So the lease is kept
/// however long a routing service takes over its answer, and a plan that ends
/// meanwhile is heard of within a heartbeat.
///
/// `None` means the job was taken away. `work` was dropped, and with it whatever
/// request it had under way.
async fn with_the_lease_kept<T>(
    pool: &PgPool,
    config: &Config,
    lease: &Lease,
    work: impl Future<Output = T>,
) -> Option<T> {
    // No search is under way, before it or after it, and so there is no progress to
    // store with a heartbeat. What a search stored is left as it is.
    let (_no_search, nothing) = watch::channel(None);
    let heartbeats = heartbeats(pool, config, lease, nothing);
    tokio::pin!(work, heartbeats);
    tokio::select! {
        done = &mut work => Some(done),
        answer = &mut heartbeats => {
            match answer {
                Heartbeat::Cancelled => {
                    info!("the plan has ended; not waiting for the routing service");
                }
                _ => info!("another worker has the job; not waiting for the routing service"),
            }
            None
        }
    }
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
/// great-circle distances are and road distances are not. It is given the mean of the
/// two, rounded up to the metre. A tour is then as long to the search as the mean of
/// driving it one way round and the other, so a tour that is short to the search is
/// short on the roads, whichever way it is driven in the end.
///
/// Where the two are the same already, as between great circles, the mean is what
/// they are.
fn search_matrix(table: &DistanceTable) -> DistanceMatrix {
    DistanceMatrix::from_fn(table.nodes(), |from, to| {
        let there_and_back = u64::from(table.get(from, to)) + u64::from(table.get(to, from));
        // Half of two `u32`s is a `u32` again, rounded up or not.
        u32::try_from(there_and_back.div_ceil(2)).unwrap_or(u32::MAX)
    })
}

/// A route as it is to be driven, and stored.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Driven {
    /// The stops in calling order, each as its node of the search.
    stops: Vec<usize>,
    /// How far it is from the depot through the stops and back, every leg measured in
    /// the direction it is driven.
    distance_m: u64,
    /// What that was measured along.
    distances: Distances,
    /// The way it takes along the streets, once the routing service has given it
    /// (D73).
    path: Option<Polyline>,
}

impl Driven {
    /// The route that the tour the search found stands for. `found` are its stops in
    /// calling order.
    ///
    /// The search cannot tell a tour from the same tour driven the other way round:
    /// on its matrix the two are equally long (D14). On roads they are not, for the
    /// one-way streets and the turns that are allowed from one side alone. So both are
    /// measured on the distances as they were given, and the shorter one is the route.
    ///
    /// The other way round the courier calls at its stops in the opposite order, and
    /// what it carries on the way changes with that (D65). A way round that would
    /// overload it is not taken, however short. Between two that are equally long,
    /// which every pair is on great circles, the tour stays as the search found it.
    fn of(table: &DistanceTable, loads: &Loads, found: &[usize], distances: Distances) -> Self {
        let found = found.to_vec();
        let turned: Vec<usize> = found.iter().rev().copied().collect();
        let (as_found, as_turned) = (round_trip(table, &found), round_trip(table, &turned));
        let (stops, distance_m) = if as_turned < as_found && loads.fits(&turned) {
            (turned, as_turned)
        } else {
            (found, as_found)
        };
        Self {
            stops,
            distance_m,
            distances,
            path: None,
        }
    }

    /// The positions the route passes in the order it is driven, from the depot and
    /// back to it: what its way along the streets is asked for by (D73). `positions`
    /// are those of the nodes.
    ///
    /// The order is the one the route is stored with, and not the one the search
    /// found. Turned round, a route takes other streets.
    ///
    /// `None` for a route with no such way to ask for: one that was measured along
    /// great circles, where the straight line from stop to stop is the way, and one
    /// without stops, which goes nowhere.
    fn round(&self, positions: &[LatLon]) -> Option<Vec<LatLon>> {
        if self.distances != Distances::Road || self.stops.is_empty() {
            return None;
        }
        let depot = positions.first().copied();
        let stops = self.stops.iter().map(|&node| positions.get(node).copied());
        iter::once(depot)
            .chain(stops)
            .chain(iter::once(depot))
            .collect()
    }
}

/// How far it is from the depot, node 0, through the stops in this order and back,
/// every leg in the direction it is driven.
fn round_trip(table: &DistanceTable, stops: &[usize]) -> u64 {
    let depot = iter::once(0);
    let from = depot.clone().chain(stops.iter().copied());
    let to = stops.iter().copied().chain(depot);
    from.zip(to)
        .map(|(from, to)| u64::from(table.get(from, to)))
        .sum()
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
    driven: &Driven,
    found: &Found,
) -> anyhow::Result<Outcome> {
    // The search makes no move that overloads the courier, and the route is not
    // turned round if that would. A route that did all the same is refused here,
    // where it would be stored, and not driven.
    ensure!(
        loads.fits(&driven.stops),
        "the route that was found has more on board than its courier carries"
    );
    let new = NewRoute {
        plan_id: route.plan_id,
        courier_id: route.courier_id,
        distance_m: driven.distance_m,
        distances: driven.distances,
        path: driven.path.clone(),
        stops: calling_order(&driven.stops, route.order_ids)?,
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
        distances = %new.distances,
        // How long the way along the streets is, measured from point to point of
        // the line. It is the way the distance was measured along, so the two agree
        // to a fraction of a percent, or the line is of another route.
        path_m = new.path.as_ref().map(|path| path.length_m().round() as u64),
        // What the search took the route's length for, on distances that are the same
        // both ways. By road that is a little off what the route is stored with.
        searched_m = found.tour.cost(),
        iterations = found.iterations,
        stopped_by = ?found.stopped_by,
        %plan,
        "stored the route"
    );
    Ok(Outcome::Done)
}

/// The orders in the order a route calls at them. `stops` are the nodes of the search:
/// node `n` is order `n - 1` of the job's list.
fn calling_order(stops: &[usize], order_ids: &[OrderId]) -> anyhow::Result<Vec<OrderId>> {
    let order_at = |&node: &usize| {
        node.checked_sub(1)
            .and_then(|stop| order_ids.get(stop))
            .copied()
            .with_context(|| format!("the tour calls at node {node}, which is no stop"))
    };
    stops.iter().map(order_at).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use routeforge_domain::db::queue::PROGRESS_CHANNEL;
    use routeforge_domain::{Haversine, JobId, JobPayload, OrderKind, RouteProgress, WorkerId};
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

        let called = calling_order(tour.stops(), &order_ids).unwrap();
        // Out along the line and back, in one direction or the other.
        let mut reversed = order_ids.clone();
        reversed.reverse();
        assert!(called == order_ids || called == reversed, "{called:?}");

        // A tour over more stops than the job has is refused, not truncated.
        let error = calling_order(tour.stops(), &order_ids[..3]).unwrap_err();
        assert!(error.to_string().contains("no stop"), "{error}");
        assert!(calling_order(tour.stops(), &[]).is_err());
        // And the depot is no stop.
        assert!(calling_order(&[0, 1], &order_ids).is_err());
    }

    #[test]
    fn a_route_without_stops_is_a_tour_without_stops() {
        let depot_alone = line(0);
        let tour = optimize_route(&depot_alone, 1, &mut Control::max_iterations(1));
        assert!(calling_order(tour.stops(), &[]).unwrap().is_empty());
        assert_eq!(tour.cost(), 0);

        let table = DistanceTable::new(1, vec![0]).unwrap();
        let driven = Driven::of(&table, &Loads::none(0), tour.stops(), Distances::Road);
        assert_eq!((driven.stops.len(), driven.distance_m), (0, 0));
        // It goes nowhere, and has no way to ask for.
        assert_eq!(driven.round(&four_places()[..1]), None);
    }

    /// The depot of the tests and three places near it, for the nodes of a ring.
    fn four_places() -> Vec<LatLon> {
        [
            (50.45, 30.52),
            (50.46, 30.52),
            (50.46, 30.53),
            (50.45, 30.53),
        ]
        .map(|(lat, lon)| LatLon::new(lat, lon).unwrap())
        .to_vec()
    }

    /// The depot and three places on a ring road that is driven one way: 100 metres
    /// to the next place, and 300 the long way round to the one before.
    fn one_way_ring() -> DistanceTable {
        #[rustfmt::skip]
        let metres = vec![
              0, 100, 200, 300,
            300,   0, 100, 200,
            200, 300,   0, 100,
            100, 200, 300,   0,
        ];
        DistanceTable::new(4, metres).unwrap()
    }

    #[test]
    fn a_table_that_differs_by_direction_is_searched_on_the_mean_of_the_two() {
        let there_and_back = DistanceTable::new(2, vec![0, 5, 5, 0]).unwrap();
        assert_eq!(search_matrix(&there_and_back).get(0, 1), 5);

        // Rounded up, so that a way is never taken for shorter than it is both ways.
        let one_way_street = DistanceTable::new(2, vec![0, 5, 8, 0]).unwrap();
        let matrix = search_matrix(&one_way_street);
        assert_eq!((matrix.get(0, 1), matrix.get(1, 0)), (7, 7));

        let ring = search_matrix(&one_way_ring());
        for (from, to, mean) in [(0, 1, 200), (1, 2, 200), (0, 2, 200), (0, 3, 200)] {
            assert_eq!(ring.get(from, to), mean, "{from} to {to}");
            assert_eq!(ring.get(to, from), mean, "{to} to {from}");
        }

        // The longest way a table can hold, both ways, is still a way.
        let far = DistanceTable::new(2, vec![0, u32::MAX, u32::MAX, 0]).unwrap();
        assert_eq!(search_matrix(&far).get(0, 1), u32::MAX);

        // Great circles are the same both ways already, and stay what they are.
        let places: Vec<LatLon> = [(50.45, 30.52), (50.40, 30.60), (50.50, 30.45)]
            .map(|(lat, lon)| LatLon::new(lat, lon).unwrap())
            .to_vec();
        let table = DistanceTable::haversine(&places);
        let matrix = search_matrix(&table);
        for from in 0..3 {
            for to in 0..3 {
                assert_eq!(matrix.get(from, to), table.get(from, to));
            }
        }
    }

    /// The search cannot tell a tour from the same tour the other way round. On roads
    /// that run one way the two differ, and the route is the shorter.
    #[test]
    fn a_route_is_driven_the_way_round_that_is_shorter_by_road() {
        let table = one_way_ring();
        // Every pair is 200 metres apart to the search, so every tour is 800 long, and
        // which way round it comes out is chance.
        let free = Loads::none(3);

        // Against the traffic it is 300 metres from each place to the one before.
        let driven = Driven::of(&table, &free, &[3, 2, 1], Distances::Road);
        assert_eq!(
            driven,
            Driven {
                stops: vec![1, 2, 3],
                distance_m: 400,
                distances: Distances::Road,
                path: None,
            }
        );
        // A tour that goes with the traffic already is left as it is.
        let driven = Driven::of(&table, &free, &[1, 2, 3], Distances::Road);
        assert_eq!((driven.stops, driven.distance_m), (vec![1, 2, 3], 400));
        // And one that crosses the ring is as long either way, and stays as it was.
        let driven = Driven::of(&table, &free, &[2, 1, 3], Distances::Road);
        assert_eq!((driven.stops, driven.distance_m), (vec![2, 1, 3], 800));
    }

    /// Turned round, a route calls at its stops in the opposite order, and what the
    /// courier has on board along it is another matter (D65).
    #[test]
    fn a_route_is_not_turned_round_if_that_overloads_the_courier() {
        let table = one_way_ring();
        // Stop 3 is a delivery of two and stop 1 a pickup of two, for a courier that
        // carries two: the delivery has to come first, and it does, against the traffic.
        let asked = [Demand::pickup(2), Demand::default(), Demand::delivery(2)];
        let loads = Loads::new(2, &asked).unwrap();
        assert!(loads.fits(&[3, 2, 1]));
        assert!(!loads.fits(&[1, 2, 3]));

        let driven = Driven::of(&table, &loads, &[3, 2, 1], Distances::Road);
        assert_eq!((driven.stops, driven.distance_m), (vec![3, 2, 1], 1_200));
        // With room for both at once it is turned round.
        let roomy = Loads::new(4, &asked).unwrap();
        let driven = Driven::of(&table, &roomy, &[3, 2, 1], Distances::Road);
        assert_eq!((driven.stops, driven.distance_m), (vec![1, 2, 3], 400));
    }

    /// The way along the streets is that of the route as it is driven (D73). The other
    /// way round the streets are other streets.
    #[test]
    fn the_way_of_a_route_is_asked_for_in_the_order_it_is_driven() {
        let table = one_way_ring();
        let places = four_places();
        let [depot, first, second, third] = [places[0], places[1], places[2], places[3]];

        // Found against the traffic, and turned round.
        let driven = Driven::of(&table, &Loads::none(3), &[3, 2, 1], Distances::Road);
        assert_eq!(driven.path, None);
        assert_eq!(
            driven.round(&places).unwrap(),
            [depot, first, second, third, depot]
        );
        // One that cannot be turned round, for what the courier carries, is asked for
        // as it stands.
        let asked = [Demand::pickup(2), Demand::default(), Demand::delivery(2)];
        let tight = Loads::new(2, &asked).unwrap();
        let driven = Driven::of(&table, &tight, &[3, 2, 1], Distances::Road);
        assert_eq!(
            driven.round(&places).unwrap(),
            [depot, third, second, first, depot]
        );

        // On great circles the straight line is the way, and nobody is asked for it.
        let as_the_crow_flies = DistanceTable::haversine(&places);
        let free = Loads::none(3);
        let driven = Driven::of(
            &as_the_crow_flies,
            &free,
            &[1, 2, 3],
            Distances::GreatCircle,
        );
        assert_eq!(driven.round(&places), None);
        // And a stop that has no position is no reason to ask for a way past the others.
        let driven = Driven::of(&table, &free, &[1, 2, 3], Distances::Road);
        assert_eq!(driven.round(&places[..3]), None);
        assert_eq!(driven.round(&[]), None);
    }

    /// On great circles both ways round are equally long, and the route is the tour as
    /// the search found it: what a seed gives does not depend on this.
    #[test]
    fn a_route_on_great_circles_is_the_tour_as_it_was_found() {
        let places: Vec<LatLon> = [
            (50.45, 30.52),
            (50.40, 30.60),
            (50.50, 30.45),
            (50.47, 30.40),
        ]
        .map(|(lat, lon)| LatLon::new(lat, lon).unwrap())
        .to_vec();
        let table = DistanceTable::haversine(&places);
        let matrix = search_matrix(&table);
        for stops in [[1, 2, 3], [3, 2, 1], [2, 1, 3]] {
            let driven = Driven::of(&table, &Loads::none(3), &stops, Distances::GreatCircle);
            assert_eq!(driven.stops, stops);
            assert_eq!(driven.distances, Distances::GreatCircle);
            // As long as the search took it for.
            let legs = [
                (0, stops[0]),
                (stops[0], stops[1]),
                (stops[1], stops[2]),
                (stops[2], 0),
            ];
            let searched: u64 = legs
                .iter()
                .map(|&(from, to)| u64::from(matrix.get(from, to)))
                .sum();
            assert_eq!(driven.distance_m, searched);
        }
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
            positions,
            table,
            matrix,
            loads,
            budget,
            distances,
        } = prepare(&context, &no_lease(route.plan_id), &route)
            .await
            .unwrap()
            .expect("nobody takes the job away");

        assert_eq!(budget, moves(10));
        // Four deliveries of one, on a courier that carries ten.
        assert_eq!(loads, Loads::new(10, &[Demand::delivery(1); 4]).unwrap());
        assert_eq!(matrix.len(), 5);
        let expected: Vec<LatLon> = iter::once(city.depot.location)
            .chain(listed.iter().map(|order| order.location))
            .collect();
        assert_eq!(positions, expected);
        let Ok(as_the_crow_flies) = Haversine.table(&expected).await;
        // Great circles, which is what the solver of these tests has: the table as it
        // was given, and the matrix the search works on, which is the same.
        assert_eq!(distances, Distances::GreatCircle);
        assert_eq!(table, as_the_crow_flies);
        for from in 0..5 {
            for to in 0..5 {
                assert_eq!(matrix.get(from, to), table.get(from, to), "{from} to {to}");
            }
        }
    }

    /// A lease on no job, for what is handed one and gets no further than holding it:
    /// great circles are there before the first heartbeat is due.
    fn no_lease(plan_id: PlanId) -> Lease {
        Lease {
            job_id: JobId::generate(),
            plan_id,
            worker_id: WorkerId::generate(),
            attempt: 1,
            max_attempts: 3,
        }
    }

    /// In the place of a routing service. Asked for a table and asked for a way, it
    /// gives one, says that it failed, or never answers, each as it was made to.
    #[derive(Clone, Debug)]
    struct Roads {
        table: Answer<DistanceTable>,
        /// The way it gives is the line straight through the positions it was asked
        /// about: enough of a way to tell which they were, and in which order.
        path: Answer<()>,
        /// The positions each way was asked for.
        asked: Arc<Mutex<Vec<Vec<LatLon>>>>,
    }

    #[derive(Clone, Debug)]
    enum Answer<T> {
        Give(T),
        Fail,
        Hang,
    }

    impl Roads {
        /// A service that answers for the table as `table` says, and gives every way
        /// it is asked for.
        fn with_table(table: Answer<DistanceTable>) -> Self {
            Self {
                table,
                path: Answer::Give(()),
                asked: Arc::default(),
            }
        }

        fn giving(table: DistanceTable) -> Self {
            Self::with_table(Answer::Give(table))
        }

        /// The same service, answering for the way as `path` says.
        fn and_the_way(mut self, path: Answer<()>) -> Self {
            self.path = path;
            self
        }

        /// The positions each way was asked for so far.
        fn asked(&self) -> Vec<Vec<LatLon>> {
            self.asked.lock().unwrap().clone()
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("the roads are closed")]
    struct Closed;

    impl DistanceProvider for Roads {
        type Error = Closed;

        fn measures(&self) -> Distances {
            Distances::Road
        }

        async fn table(&self, _positions: &[LatLon]) -> Result<DistanceTable, Closed> {
            match &self.table {
                Answer::Give(table) => Ok(table.clone()),
                Answer::Fail => Err(Closed),
                Answer::Hang => std::future::pending().await,
            }
        }

        async fn path(&self, positions: &[LatLon]) -> Result<Option<Polyline>, Closed> {
            self.asked.lock().unwrap().push(positions.to_vec());
            match self.path {
                Answer::Give(()) => Ok(Polyline::through(positions)),
                Answer::Fail => Err(Closed),
                Answer::Hang => std::future::pending().await,
            }
        }
    }

    fn with_roads(pool: &PgPool, roads: Roads) -> Context<Roads> {
        Context {
            pool: pool.clone(),
            config: testing::config(),
            distances: roads,
        }
    }

    /// A plan of three deliveries of one for a courier that carries ten, and the route
    /// job of that courier, claimed.
    async fn a_route_on_the_ring(pool: &PgPool) -> (testing::City, Vec<OrderId>, Lease) {
        let city = testing::city(pool, &[10]).await;
        let placed = city.orders(pool, date(12), 3, 1).await;
        let plan = plans::submit(pool, &city.new_plan(date(12), moves(1_000)), None)
            .await
            .unwrap();
        // The plan job, done by hand: the orders are the plan's, and it waits for one
        // route.
        let plan_job = queue::claim(pool, WorkerId::generate(), LONG)
            .await
            .unwrap()
            .unwrap();
        let order_ids: Vec<OrderId> = placed.iter().map(|order| order.id).collect();
        let mut completion = queue::begin_complete(pool, &plan_job.lease)
            .await
            .unwrap()
            .unwrap();
        orders::mark_planned(completion.conn(), plan.id, &order_ids)
            .await
            .unwrap();
        let route = JobPayload::Route {
            plan_id: plan.id,
            courier_id: city.couriers[0].id,
            order_ids: order_ids.clone(),
            seed: 7,
        };
        queue::enqueue(completion.conn(), &route, None)
            .await
            .unwrap();
        let counts = routeforge_domain::PlanCounts {
            orders_total: 3,
            orders_unassigned: 0,
            routes_total: 1,
        };
        plans::start_routing(completion.conn(), plan.id, counts)
            .await
            .unwrap();
        completion.commit().await.unwrap();

        let job = queue::claim(pool, WorkerId::generate(), LONG)
            .await
            .unwrap()
            .expect("the route job was enqueued a moment ago");
        (city, order_ids, job.lease)
    }

    /// The ring of `one_way_ring` with its traffic the other way.
    fn the_ring_the_other_way() -> DistanceTable {
        let ring = one_way_ring();
        let nodes = ring.nodes();
        let mut metres = Vec::with_capacity(nodes * nodes);
        for from in 0..nodes {
            for to in 0..nodes {
                metres.push(ring.get(to, from));
            }
        }
        DistanceTable::new(nodes, metres).unwrap()
    }

    /// Runs the route job of `a_route_on_the_ring` on roads that give `table`, and
    /// holds what it stored to the ring: by road, with the traffic, and with the way
    /// of the route as it is driven. Returns the orders as the job lists them and as
    /// the stored route calls at them.
    async fn stored_on(pool: &PgPool, table: DistanceTable) -> (Vec<OrderId>, Vec<OrderId>) {
        let (city, order_ids, lease) = a_route_on_the_ring(pool).await;
        let roads = Roads::giving(table);
        let context = with_roads(pool, roads.clone());
        let route = Route {
            plan_id: lease.plan_id,
            courier_id: city.couriers[0].id,
            order_ids: &order_ids,
            seed: 7,
        };

        assert_eq!(run(&context, &lease, &route).await.unwrap(), Outcome::Done);

        let stored = routes::list_for_plan(pool, lease.plan_id).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].distances, Distances::Road);
        // With the traffic, whichever way round the search happened to come out.
        assert_eq!(stored[0].distance_m, 400);

        // The way was asked for once: from the depot through the stops in the order
        // the route calls at them, and back. And that is the way it is stored with.
        let round: Vec<LatLon> = iter::once(city.depot.location)
            .chain(stored[0].stops.iter().map(|stop| stop.location))
            .chain(iter::once(city.depot.location))
            .collect();
        assert_eq!(roads.asked(), std::slice::from_ref(&round));
        assert_eq!(stored[0].path, Polyline::through(&round));

        let called = stored[0].stops.iter().map(|stop| stop.order_id).collect();
        (order_ids, called)
    }

    /// The whole job on roads that run one way: searched on the mean of the two
    /// directions, stored the way round that is shorter, with the length by road and
    /// the way along the streets.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_route_by_road_is_stored_the_shorter_way_round_and_says_so(pool: PgPool) {
        let (order_ids, called) = stored_on(&pool, one_way_ring()).await;
        assert_eq!(called, order_ids);
    }

    /// The same ring with its traffic the other way. To the search the two are one
    /// ring, so it finds the same tour on both, and here that tour is against the
    /// traffic: the route is turned round. Its way along the streets has to be the way
    /// of the route as it is driven, and not as it was found (D73).
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_route_that_is_turned_round_is_stored_with_the_way_it_is_driven(pool: PgPool) {
        let (mut order_ids, called) = stored_on(&pool, the_ring_the_other_way()).await;
        order_ids.reverse();
        assert_eq!(called, order_ids);
    }

    /// A service that gives the distances and then no way costs the route its line on
    /// the map, and nothing else: it is found and measured by road, and stored so.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_route_the_service_gives_no_way_for_is_stored_without_one(pool: PgPool) {
        let (city, order_ids, lease) = a_route_on_the_ring(&pool).await;
        let roads = Roads::giving(one_way_ring()).and_the_way(Answer::Fail);
        let context = with_roads(&pool, roads.clone());
        let route = Route {
            plan_id: lease.plan_id,
            courier_id: city.couriers[0].id,
            order_ids: &order_ids,
            seed: 7,
        };

        assert_eq!(run(&context, &lease, &route).await.unwrap(), Outcome::Done);

        assert_eq!(roads.asked().len(), 1);
        let stored = routes::list_for_plan(&pool, lease.plan_id).await.unwrap();
        assert_eq!(
            (stored[0].distances, stored[0].distance_m, &stored[0].path),
            (Distances::Road, 400, &None)
        );
        let plan = plans::get(&pool, lease.plan_id).await.unwrap().unwrap();
        assert_eq!(plan.status, routeforge_domain::PlanStatus::Done);
    }

    /// A service that fails costs the route its roads, and nothing else.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_route_the_service_gives_no_distances_for_is_searched_on_great_circles(pool: PgPool) {
        let (city, order_ids, lease) = a_route_on_the_ring(&pool).await;
        let roads = Roads::with_table(Answer::Fail);
        let context = with_roads(&pool, roads.clone());
        let route = Route {
            plan_id: lease.plan_id,
            courier_id: city.couriers[0].id,
            order_ids: &order_ids,
            seed: 7,
        };

        let prepared = prepare(&context, &lease, &route)
            .await
            .unwrap()
            .expect("the job is this worker's");
        assert_eq!(prepared.distances, Distances::GreatCircle);
        let placed = orders::list_by_ids(&pool, &order_ids).await.unwrap();
        let positions: Vec<LatLon> = iter::once(city.depot.location)
            .chain(order_ids.iter().map(|id| {
                let order = placed.iter().find(|order| order.id == *id).unwrap();
                order.location
            }))
            .collect();
        assert_eq!(prepared.table, DistanceTable::haversine(&positions));

        assert_eq!(run(&context, &lease, &route).await.unwrap(), Outcome::Done);
        let stored = routes::list_for_plan(&pool, lease.plan_id).await.unwrap();
        // Not taken for a route by road, and as long as it says by the crow's way.
        assert_eq!(stored[0].distances, Distances::GreatCircle);
        assert_eq!(
            stored[0].distance_m,
            testing::round_trip(city.depot.location, &stored[0].stops)
        );
        // Its way is the straight line from stop to stop. The service, which would
        // have given one along the streets, was not asked for it.
        assert_eq!(roads.asked(), Vec::<Vec<LatLon>>::new());
        assert_eq!(stored[0].path, None);
    }

    /// A table of another size than was asked for is no table of this route.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_table_of_other_positions_is_an_error(pool: PgPool) {
        let (city, order_ids, lease) = a_route_on_the_ring(&pool).await;
        let two_places = DistanceTable::new(2, vec![0, 5, 5, 0]).unwrap();
        let context = with_roads(&pool, Roads::giving(two_places));
        let route = Route {
            plan_id: lease.plan_id,
            courier_id: city.couriers[0].id,
            order_ids: &order_ids,
            seed: 7,
        };
        let error = prepare(&context, &lease, &route).await.unwrap_err();
        assert!(error.to_string().contains("4 positions"), "{error}");
    }

    /// The service is waited for with the heartbeat beside it. So the lease, which is
    /// two seconds long here, outlasts a wait of three, and nobody else gets the job.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn the_lease_is_kept_while_the_distances_are_waited_for(pool: PgPool) {
        let _turn = one_at_a_time().await;
        let (city, order_ids, lease) = a_route_on_the_ring(&pool).await;
        // Claimed for good in the helper. Here it is held as a worker holds it: for
        // the two seconds of the test's settings, from now.
        let config = testing::config();
        assert_eq!(
            queue::heartbeat(&pool, &lease, config.lease, None)
                .await
                .unwrap(),
            Heartbeat::Extended
        );
        let context = with_roads(&pool, Roads::with_table(Answer::Hang));
        let route = Route {
            plan_id: lease.plan_id,
            courier_id: city.couriers[0].id,
            order_ids: &order_ids,
            seed: 7,
        };

        let waiting = prepare(&context, &lease, &route);
        tokio::pin!(waiting);
        let waited = tokio::time::timeout(Duration::from_secs(3), &mut waiting).await;
        assert!(waited.is_err(), "the service never answers");
        // Still this worker's: another finds nothing to claim.
        let other = queue::claim(&pool, WorkerId::generate(), LONG)
            .await
            .unwrap();
        assert!(other.is_none(), "{other:?}");
        assert!(
            queue::begin_complete(&pool, &lease)
                .await
                .unwrap()
                .is_some()
        );
    }

    /// And a plan that ends meanwhile is heard of, within a heartbeat or so.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_plan_that_is_cancelled_ends_the_wait_for_the_distances(pool: PgPool) {
        let _turn = one_at_a_time().await;
        let (city, order_ids, lease) = a_route_on_the_ring(&pool).await;
        let context = with_roads(&pool, Roads::with_table(Answer::Hang));
        let route = Route {
            plan_id: lease.plan_id,
            courier_id: city.couriers[0].id,
            order_ids: &order_ids,
            seed: 7,
        };

        let cancelling = async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(queue::cancel_plan(&pool, lease.plan_id).await.unwrap());
        };
        let (outcome, ()) = tokio::join!(soon(run(&context, &lease, &route)), cancelling);
        assert_eq!(outcome.unwrap(), Outcome::Gone);
        assert!(
            routes::list_for_plan(&pool, lease.plan_id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// The way is waited for as the distances are: with the heartbeat beside it. The
    /// search is over by then, and the route is not stored yet, so a lease that ran
    /// out here would have the whole route searched for again by another worker.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn the_lease_is_kept_while_the_way_is_waited_for(pool: PgPool) {
        let _turn = one_at_a_time().await;
        let (city, order_ids, lease) = a_route_on_the_ring(&pool).await;
        let config = testing::config();
        assert_eq!(
            queue::heartbeat(&pool, &lease, config.lease, None)
                .await
                .unwrap(),
            Heartbeat::Extended
        );
        let roads = Roads::giving(one_way_ring()).and_the_way(Answer::Hang);
        let context = with_roads(&pool, roads.clone());
        let route = Route {
            plan_id: lease.plan_id,
            courier_id: city.couriers[0].id,
            order_ids: &order_ids,
            seed: 7,
        };

        let job = run(&context, &lease, &route);
        tokio::pin!(job);
        let waited = tokio::time::timeout(Duration::from_secs(3), &mut job).await;
        assert!(waited.is_err(), "the service never answers");
        // The job got as far as asking for the way, and is waiting for it.
        assert_eq!(roads.asked().len(), 1);
        // Still this worker's: another finds nothing to claim.
        let other = queue::claim(&pool, WorkerId::generate(), LONG)
            .await
            .unwrap();
        assert!(other.is_none(), "{other:?}");
        assert!(
            queue::begin_complete(&pool, &lease)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_plan_that_is_cancelled_ends_the_wait_for_the_way(pool: PgPool) {
        let _turn = one_at_a_time().await;
        let (city, order_ids, lease) = a_route_on_the_ring(&pool).await;
        let roads = Roads::giving(one_way_ring()).and_the_way(Answer::Hang);
        let context = with_roads(&pool, roads.clone());
        let route = Route {
            plan_id: lease.plan_id,
            courier_id: city.couriers[0].id,
            order_ids: &order_ids,
            seed: 7,
        };

        let cancelling = async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            // By now the search is over, and the way is what the job waits for.
            assert_eq!(roads.asked().len(), 1);
            assert!(queue::cancel_plan(&pool, lease.plan_id).await.unwrap());
        };
        let (outcome, ()) = tokio::join!(soon(run(&context, &lease, &route)), cancelling);
        assert_eq!(outcome.unwrap(), Outcome::Gone);
        assert!(
            routes::list_for_plan(&pool, lease.plan_id)
                .await
                .unwrap()
                .is_empty()
        );
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

        let error = prepare(&context, &no_lease(route.plan_id), &route)
            .await
            .unwrap_err();
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
            ..
        } = prepare(&context, &no_lease(route.plan_id), &route)
            .await
            .unwrap()
            .expect("nobody takes the job away");
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
        let called = calling_order(found.tour.stops(), &order_ids).unwrap();
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

        let error = prepare(&context, &no_lease(route.plan_id), &route)
            .await
            .unwrap_err();
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
        let error = prepare(&context, &no_lease(route.plan_id), &route)
            .await
            .unwrap_err();
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

        let error = prepare(&context, &no_lease(route.plan_id), &route)
            .await
            .unwrap_err();
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

        let driven = Driven {
            stops: tour.stops().to_vec(),
            distance_m: tour.cost(),
            distances: Distances::GreatCircle,
            path: None,
        };
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
        let error = complete(&pool, &lease, &route, &loads, &driven, &found)
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
