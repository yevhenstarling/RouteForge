//! Clustering and route optimization for RouteForge.
//!
//! Pure computation: no I/O, no async and no dependency on other workspace crates.
//! Every random number comes from a generator seeded by the caller.
//!
//! There are two entry points, one for each kind of job the solver runs:
//!
//! - [`cluster`] splits a day's stops into one cluster per courier, by capacity.
//! - [`optimize_route_within`] puts the stops of one courier in a good order, in which
//!   the courier never has more on board than it can carry. [`optimize_route`] does
//!   the same for a route on which nothing is carried.
//!
//! ```
//! use routeforge_tsp::{Control, DistanceMatrix, Point, optimize_route};
//!
//! // The depot and three stops on the corners of a rectangle of 300 by 400 metres.
//! let corners = [(0.0, 0.0), (300.0, 400.0), (0.0, 400.0), (300.0, 0.0)];
//! let points: Vec<Point> = corners.iter().map(|&(x, y)| Point::new(x, y)).collect();
//! let matrix = DistanceMatrix::euclidean(&points);
//!
//! let mut control = Control::max_iterations(10_000);
//! let tour = optimize_route(&matrix, 42, &mut control);
//!
//! // Round the rectangle, not across it.
//! assert_eq!(tour.cost(), 1_400);
//! assert!(tour.stops() == [2, 1, 3] || tour.stops() == [3, 1, 2]);
//! ```
//!
//! The stages behind [`optimize_route`] are public as well, for the benchmarks in
//! `benches/` and for `examples/stages.rs`, which prints the cost after each of them:
//!
//! ```text
//! cargo run -p routeforge-tsp --release --example stages
//! cargo bench -p routeforge-tsp
//! ```

mod anneal;
mod construct;
mod control;
mod loads;
mod local_search;
mod matrix;
mod moves;
mod problem;
mod sweep;
#[cfg(test)]
mod testing;
mod tour;

pub use anneal::anneal;
pub use construct::nearest_neighbour;
pub use control::{Control, Progress, StopReason};
pub use loads::{LoadError, Loads};
pub use local_search::{local_search, or_opt, two_opt};
pub use matrix::{DistanceMatrix, MatrixError};
pub use problem::{Cluster, Clustering, Demand, Point, Problem, Stop};
pub use sweep::cluster;
pub use tour::Tour;

/// Finds a short closed tour from the depot, node 0, through every other node.
///
/// Three stages share `control`: [`nearest_neighbour`] builds a tour, [`local_search`]
/// improves it until 2-opt and Or-opt find nothing more, and [`anneal`] keeps looking
/// for a better one until the control stops it. The best tour seen is returned.
///
/// The search is anytime and deterministic (D5). There is always a tour to return,
/// even when the control stops the search before its first move, and the same matrix,
/// seed and iteration budget always give the same tour.
///
/// With up to three stops the result is optimal and the function returns without
/// using up its budget or waiting for its deadline.
pub fn optimize_route(matrix: &DistanceMatrix, seed: u64, control: &mut Control<'_>) -> Tour {
    search(matrix, None, seed, control)
}

/// Finds a short closed tour from the depot through every stop, on which the courier
/// never has more on board than it can carry (D65).
///
/// It is the search of [`optimize_route`], and what is said there holds here: it is
/// anytime and deterministic, and with up to three stops its result is the shortest
/// tour the courier can drive. Every stage makes only moves that leave the courier
/// within its capacity all along the tour: the tour it starts from, every tour it
/// reports on the way and the one it returns all fit.
///
/// Where the deliveries and the pickups of the route fit the courier even together, no
/// order of the stops can overload it. The search is then the one of
/// [`optimize_route`], move for move.
///
/// ```
/// use routeforge_tsp::{
///     Control, Demand, DistanceMatrix, Loads, Point, optimize_route, optimize_route_within,
/// };
///
/// // The depot and three stops on the corners of a square of ten metres. The stop
/// // opposite the depot is a delivery of two parcels. Its neighbours are a pickup of
/// // one parcel each.
/// let corners = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
/// let points: Vec<Point> = corners.iter().map(|&(x, y)| Point::new(x, y)).collect();
/// let matrix = DistanceMatrix::euclidean(&points);
/// let stops = [Demand::pickup(1), Demand::delivery(2), Demand::pickup(1)];
///
/// // Round the square is the shortest way.
/// let round = optimize_route(&matrix, 42, &mut Control::max_iterations(1_000));
/// assert_eq!(round.cost(), 40);
///
/// // A courier that carries two leaves the depot full, and round the square comes
/// // to a pickup first, whichever way. It has to make the delivery first.
/// let loads = Loads::new(2, &stops)?;
/// assert!(!loads.fits(round.stops()));
/// let tour = optimize_route_within(&matrix, &loads, 42, &mut Control::max_iterations(1_000));
/// assert_eq!(tour.stops()[0], 2);
/// assert_eq!(tour.cost(), 48);
/// assert!(loads.fits(tour.stops()));
/// # Ok::<(), routeforge_tsp::LoadError>(())
/// ```
///
/// # Panics
///
/// If the loads are of a route with another number of stops than the matrix has.
pub fn optimize_route_within(
    matrix: &DistanceMatrix,
    loads: &Loads,
    seed: u64,
    control: &mut Control<'_>,
) -> Tour {
    loads.check(matrix);
    search(matrix, loads.limit(), seed, control)
}

/// The three stages, one after the other, minding the loads if there are any to mind.
fn search(
    matrix: &DistanceMatrix,
    loads: Option<&Loads>,
    seed: u64,
    control: &mut Control<'_>,
) -> Tour {
    let mut tour = construct::nearest_neighbour_within(matrix, loads);
    control.report(&tour);
    local_search::local_search_within(matrix, loads, &mut tour, control);
    anneal::anneal_within(matrix, loads, &mut tour, seed, control);
    control.report(&tour);
    tour
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use proptest::prelude::*;
    use rand::{RngExt, SeedableRng};
    use rand_chacha::ChaCha8Rng;

    use super::*;
    use crate::control::POLL_INTERVAL;
    use crate::tour::cycle_cost;

    /// Stops scattered over a square of two kilometres, the same for the same seed.
    fn scattered(stops: usize, seed: u64) -> DistanceMatrix {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let mut coordinate = || rng.random_range(0.0..2_000.0);
        let points: Vec<Point> = (0..=stops)
            .map(|_| Point::new(coordinate(), coordinate()))
            .collect();
        DistanceMatrix::euclidean(&points)
    }

    proptest! {
        #[test]
        fn the_route_visits_every_stop_once_and_knows_its_length(
            matrix in testing::matrix(0..=12),
            seed in any::<u64>(),
            budget in 0..30_000u64,
        ) {
            let mut control = Control::max_iterations(budget);
            let tour = optimize_route(&matrix, seed, &mut control);

            prop_assert!(control.iterations() <= budget);
            prop_assert!(testing::is_tour(&matrix, &tour.order));
            let mut stops = tour.stops().to_vec();
            stops.sort_unstable();
            prop_assert!(stops.into_iter().eq(1..matrix.len()));
            prop_assert_eq!(tour.cost(), cycle_cost(&matrix, &tour.order));
        }

        #[test]
        fn the_same_seed_and_budget_give_the_same_route(
            matrix in testing::matrix(0..=12),
            seed in any::<u64>(),
            budget in 0..60_000u64,
        ) {
            let run = || {
                let mut reports = Vec::new();
                let mut control = Control::max_iterations(budget)
                    .with_progress(|progress, best| reports.push((progress, best.clone())));
                let tour = optimize_route(&matrix, seed, &mut control);
                let iterations = control.iterations();
                drop(control);
                (tour, iterations, reports)
            };
            prop_assert_eq!(run(), run());
        }

        #[test]
        fn a_larger_budget_never_gives_a_longer_route(
            matrix in testing::matrix(0..=12),
            seed in any::<u64>(),
            budget in 0..30_000u64,
            extra in 0..30_000u64,
        ) {
            let cost = |budget| optimize_route(&matrix, seed, &mut Control::max_iterations(budget)).cost();
            prop_assert!(cost(budget + extra) <= cost(budget));
        }

        #[test]
        fn no_route_is_shorter_than_the_shortest_one(
            matrix in prop_oneof![testing::matrix(0..=10), testing::euclidean(0..=10)],
            seed in any::<u64>(),
            budget in 0..30_000u64,
        ) {
            let tour = optimize_route(&matrix, seed, &mut Control::max_iterations(budget));
            prop_assert!(tour.cost() >= testing::optimum(&matrix));
        }
    }

    /// What each stop of a route asks for, drawn for a route of up to eleven stops.
    fn numbers() -> impl Strategy<Value = Vec<(u32, u32)>> {
        prop::collection::vec((0..=6u32, 0..=6u32), 12)
    }

    // The same promises for a courier that can carry only so much (D65), and one more:
    // the courier is never over its capacity, on the route or on any tour reported
    // on the way to it.
    proptest! {
        #[test]
        fn a_route_within_loads_is_one_the_courier_can_drive(
            matrix in testing::matrix(0..=12),
            numbers in numbers(),
            slack in 0..=4u32,
            seed in any::<u64>(),
            budget in 0..30_000u64,
        ) {
            let loads = testing::loads(&testing::demands(matrix.len(), &numbers), slack);
            let mut reported = Vec::new();
            let mut control = Control::max_iterations(budget)
                .with_progress(|_, best| reported.push(best.clone()));
            let tour = optimize_route_within(&matrix, &loads, seed, &mut control);
            let iterations = control.iterations();
            drop(control);

            prop_assert!(iterations <= budget);
            prop_assert!(testing::is_tour(&matrix, &tour.order));
            prop_assert!(loads.fits(&tour.order));
            // The depot changes nothing, so it may be left out.
            prop_assert!(loads.fits(tour.stops()));
            prop_assert_eq!(tour.cost(), cycle_cost(&matrix, &tour.order));
            for best in &reported {
                prop_assert!(testing::is_tour(&matrix, &best.order));
                prop_assert!(loads.fits(&best.order), "reported {:?}", best.order);
            }
        }

        #[test]
        fn the_same_seed_and_budget_give_the_same_route_within_loads(
            matrix in testing::matrix(0..=12),
            numbers in numbers(),
            seed in any::<u64>(),
            budget in 0..60_000u64,
        ) {
            let loads = testing::loads(&testing::demands(matrix.len(), &numbers), 0);
            let run = || {
                let mut reports = Vec::new();
                let mut control = Control::max_iterations(budget)
                    .with_progress(|progress, best| reports.push((progress, best.clone())));
                let tour = optimize_route_within(&matrix, &loads, seed, &mut control);
                let iterations = control.iterations();
                drop(control);
                (tour, iterations, reports)
            };
            prop_assert_eq!(run(), run());
        }

        #[test]
        fn a_larger_budget_never_gives_a_longer_route_within_loads(
            matrix in testing::matrix(0..=12),
            numbers in numbers(),
            seed in any::<u64>(),
            budget in 0..30_000u64,
            extra in 0..30_000u64,
        ) {
            let loads = testing::loads(&testing::demands(matrix.len(), &numbers), 0);
            let cost = |budget| {
                optimize_route_within(&matrix, &loads, seed, &mut Control::max_iterations(budget))
                    .cost()
            };
            prop_assert!(cost(budget + extra) <= cost(budget));
        }

        #[test]
        fn no_route_within_loads_is_shorter_than_the_shortest_that_fits(
            matrix in prop_oneof![testing::matrix(0..=10), testing::euclidean(0..=10)],
            numbers in numbers(),
            slack in 0..=2u32,
            seed in any::<u64>(),
            budget in 0..30_000u64,
        ) {
            let loads = testing::loads(&testing::demands(matrix.len(), &numbers), slack);
            let tour = optimize_route_within(
                &matrix,
                &loads,
                seed,
                &mut Control::max_iterations(budget),
            );
            prop_assert!(tour.cost() >= testing::optimum_within(&matrix, Some(&loads)));
        }

        /// A courier with room for the deliveries and the pickups together cannot be
        /// overloaded by any order of the stops. Its route is the one found without
        /// loads, by the same moves: this is the case every route was before the load
        /// was followed along it (D7).
        #[test]
        fn loads_that_cannot_be_exceeded_give_the_route_of_no_loads(
            matrix in testing::matrix(0..=12),
            numbers in numbers(),
            seed in any::<u64>(),
            budget in 0..60_000u64,
        ) {
            let stops = testing::demands(matrix.len(), &numbers);
            let out: u32 = stops.iter().map(|stop| stop.delivery).sum();
            let back: u32 = stops.iter().map(|stop| stop.pickup).sum();
            let loads = Loads::new(out + back, &stops).unwrap();

            let run = |loads: Option<&Loads>| {
                let mut reports = Vec::new();
                let mut control = Control::max_iterations(budget)
                    .with_progress(|progress, best| reports.push((progress, best.clone())));
                let tour = match loads {
                    Some(loads) => optimize_route_within(&matrix, loads, seed, &mut control),
                    None => optimize_route(&matrix, seed, &mut control),
                };
                let iterations = control.iterations();
                drop(control);
                (tour, iterations, reports)
            };
            prop_assert_eq!(run(Some(&loads)), run(None));
        }
    }

    /// With up to nine stops every order can be tried, so the right answer is known.
    ///
    /// The instances are fixed. Annealing gives no guarantee, so on random instances
    /// this test could fail without anything being wrong.
    #[test]
    fn small_routes_come_out_optimal() {
        for seed in 0..120 {
            let matrix = scattered(1 + seed as usize % 9, seed);
            let tour = optimize_route(&matrix, seed, &mut Control::max_iterations(100_000));
            assert_eq!(tour.cost(), testing::optimum(&matrix), "instance {seed}");
        }
    }

    /// The same instances, each for a courier that is as small as its stops allow:
    /// every third stop a pickup, and the others deliveries, of one to three parcels.
    #[test]
    fn small_routes_within_loads_come_out_as_short_as_the_courier_can_drive() {
        let mut overloading = 0;
        for seed in 0..120 {
            let stops = 1 + seed as usize % 9;
            let matrix = scattered(stops, seed);
            let asked: Vec<Demand> = (0..stops as u32)
                .map(|stop| {
                    let parcels = 1 + (stop + seed as u32) % 3;
                    if stop % 3 == 2 {
                        Demand::pickup(parcels)
                    } else {
                        Demand::delivery(parcels)
                    }
                })
                .collect();
            let loads = testing::loads(&asked, 0);

            let mut control = Control::max_iterations(100_000);
            let tour = optimize_route_within(&matrix, &loads, seed, &mut control);
            assert!(loads.fits(tour.stops()), "instance {seed}");
            assert_eq!(
                tour.cost(),
                testing::optimum_within(&matrix, Some(&loads)),
                "instance {seed}"
            );
            // How many of them the loads had a say in: the shortest tour there is
            // would overload the courier.
            let free = optimize_route(&matrix, seed, &mut Control::max_iterations(100_000));
            overloading += usize::from(!loads.fits(free.stops()));
        }
        assert!(overloading >= 20, "{overloading} of 120");
    }

    #[test]
    fn the_square_is_driven_delivery_first_by_a_courier_that_leaves_full() {
        let (matrix, loads) = testing::square_with_the_delivery_opposite();
        // Three stops: local search is exact, and annealing is not needed.
        let mut control = Control::max_iterations(1_000_000);
        let tour = optimize_route_within(&matrix, &loads, 7, &mut control);
        assert_eq!(control.stop_reason(), None);
        assert_eq!(tour.cost(), 48);
        assert_eq!(tour.stops()[0], 2);
        assert_eq!(loads.peak(tour.stops()), 2);

        assert_eq!(optimize_route(&matrix, 7, &mut control).cost(), 40);
    }

    #[test]
    #[should_panic(expected = "another number of stops")]
    fn loads_of_another_route_are_refused() {
        let matrix = scattered(5, 1);
        let loads = Loads::none(4);
        optimize_route_within(&matrix, &loads, 1, &mut Control::max_iterations(10));
    }

    #[test]
    fn up_to_three_stops_need_no_annealing() {
        for stops in 0..=3 {
            let matrix = scattered(stops, 7);
            // Annealing would use the budget up and say so.
            let mut control = Control::max_iterations(1_000_000);
            let tour = optimize_route(&matrix, 7, &mut control);
            assert_eq!(control.stop_reason(), None, "{stops} stops");
            assert_eq!(tour.cost(), testing::optimum(&matrix), "{stops} stops");
        }
        let empty = DistanceMatrix::from_fn(0, |_, _| 0);
        assert!(
            optimize_route(&empty, 7, &mut Control::max_iterations(1_000))
                .stops()
                .is_empty()
        );
    }

    #[test]
    fn progress_tells_the_story_of_the_search() {
        let matrix = scattered(40, 5);
        let mut reports = Vec::new();
        let mut control = Control::max_iterations(300_000)
            .with_progress(|progress, best| reports.push((progress, best.clone())));
        let tour = optimize_route(&matrix, 5, &mut control);
        drop(control);

        assert!(reports.len() > 10);
        assert!(reports.is_sorted_by_key(|(progress, _)| progress.iterations));
        assert!(
            reports
                .windows(2)
                .all(|pair| pair[1].0.best_cost <= pair[0].0.best_cost)
        );
        assert!(tour.cost() < nearest_neighbour(&matrix).cost());

        // Each report comes with the tour it speaks of: a whole tour, of that length.
        for (progress, best) in &reports {
            assert!(testing::is_tour(&matrix, &best.order));
            assert_eq!(best.cost(), cycle_cost(&matrix, &best.order));
            assert_eq!(best.cost(), progress.best_cost);
        }
        // The first is the tour the search starts from and the last is the one it returns.
        assert_eq!(reports.first().unwrap().1, nearest_neighbour(&matrix));
        assert_eq!(reports.last().unwrap().1, tour);
        // In between the tour changes only by getting shorter. A caller may rely on
        // that: a report with the same length as the one before has the same tour.
        for pair in reports.windows(2) {
            let same_tour = pair[1].1 == pair[0].1;
            let same_length = pair[1].0.best_cost == pair[0].0.best_cost;
            assert_eq!(same_tour, same_length);
        }
    }

    /// A search is looked in on once per poll interval, and it ends between two looks.
    /// So it reports once more as it returns: whoever shows the search then shows its
    /// result, and not the tour of a moment before.
    #[test]
    fn the_last_report_is_of_the_tour_that_is_returned() {
        // Two hundred stops keep local search busy for a million moves. This budget
        // ends it half an interval after the second look, while it still improves.
        let matrix = scattered(200, 5);
        let budget = POLL_INTERVAL + POLL_INTERVAL / 2;
        let mut reports = Vec::new();
        let mut control = Control::max_iterations(budget)
            .with_progress(|progress, best| reports.push((progress, best.clone())));
        let tour = optimize_route(&matrix, 5, &mut control);
        drop(control);

        let (last, best) = reports.last().unwrap();
        assert_eq!(best, &tour);
        assert_eq!(last.best_cost, tour.cost());
        // It says what no report before it could. The look before it came one poll
        // interval in, give or take the row local search was at, and the search went
        // on shortening the tour after that.
        let (before, _) = &reports[reports.len() - 2];
        assert!((POLL_INTERVAL..budget).contains(&before.iterations));
        assert!(last.iterations > before.iterations);
        assert!(before.best_cost > tour.cost());
    }

    #[test]
    fn a_flag_set_beforehand_still_gives_a_route() {
        let matrix = scattered(30, 1);
        let flag = AtomicBool::new(true);
        let mut control = Control::max_iterations(u64::MAX).with_cancel(&flag);
        let tour = optimize_route(&matrix, 1, &mut control);

        assert_eq!(control.stop_reason(), Some(StopReason::Cancelled));
        assert_eq!(control.iterations(), 0);
        assert_eq!(tour, nearest_neighbour(&matrix));
    }

    #[test]
    fn a_flag_set_during_the_search_stops_it_within_a_poll_interval() {
        let matrix = scattered(30, 2);
        let flag = AtomicBool::new(false);
        let mut flagged_at = None;
        let mut control = Control::max_iterations(u64::MAX)
            .with_cancel(&flag)
            .with_progress(|progress, _| {
                if progress.iterations >= 100_000 && flagged_at.is_none() {
                    flag.store(true, Ordering::Relaxed);
                    flagged_at = Some(progress.iterations);
                }
            });
        let tour = optimize_route(&matrix, 2, &mut control);
        let iterations = control.iterations();
        assert_eq!(control.stop_reason(), Some(StopReason::Cancelled));
        drop(control);

        // The flag is looked at once per poll interval. Local search asks for a whole
        // scan row at a time, which is fewer moves than twice the nodes.
        let longest_row = 2 * matrix.len() as u64;
        assert!(iterations <= flagged_at.unwrap() + POLL_INTERVAL + longest_row);
        assert!(testing::is_tour(&matrix, &tour.order));
    }

    #[test]
    fn another_thread_can_cancel_the_search() {
        let matrix = scattered(30, 3);
        let flag = AtomicBool::new(false);
        let tour = thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(20));
                flag.store(true, Ordering::Relaxed);
            });
            let mut control = Control::max_iterations(u64::MAX).with_cancel(&flag);
            let tour = optimize_route(&matrix, 3, &mut control);
            assert_eq!(control.stop_reason(), Some(StopReason::Cancelled));
            tour
        });
        assert!(testing::is_tour(&matrix, &tour.order));
    }

    #[test]
    fn a_deadline_in_the_past_still_gives_a_route() {
        let matrix = scattered(30, 4);
        let mut control = Control::until(Instant::now());
        let tour = optimize_route(&matrix, 4, &mut control);

        assert_eq!(control.stop_reason(), Some(StopReason::Deadline));
        assert_eq!(control.iterations(), 0);
        assert_eq!(tour, nearest_neighbour(&matrix));
    }

    #[test]
    fn a_search_without_a_budget_ends_at_its_deadline() {
        let matrix = scattered(30, 6);
        let started = Instant::now();
        let deadline = started + Duration::from_millis(50);
        let mut control = Control::until(deadline);
        let tour = optimize_route(&matrix, 6, &mut control);

        assert_eq!(control.stop_reason(), Some(StopReason::Deadline));
        assert!(Instant::now() >= deadline);
        // Generous, for a loaded machine. The search itself overshoots by one poll interval.
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(control.iterations() > 0);
        assert!(testing::is_tour(&matrix, &tour.order));
    }
}
