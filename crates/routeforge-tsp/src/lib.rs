//! Clustering and route optimization for RouteForge.
//!
//! Pure computation: no I/O, no async and no dependency on other workspace crates.
//! Every random number comes from a generator seeded by the caller.
//!
//! There are two entry points, one for each kind of job the solver runs:
//!
//! - [`cluster`] splits a day's stops into one cluster per courier, by capacity.
//! - [`optimize_route`] puts the stops of one courier in a good order.
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
pub use local_search::{local_search, or_opt, two_opt};
pub use matrix::{DistanceMatrix, MatrixError};
pub use problem::{Cluster, Clustering, Point, Problem, Stop};
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
    let mut tour = nearest_neighbour(matrix);
    control.report(&tour);
    local_search(matrix, &mut tour, control);
    anneal(matrix, &mut tour, seed, control);
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
