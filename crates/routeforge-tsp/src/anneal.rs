//! Simulated annealing.

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;

use crate::control::Control;
use crate::loads::Loads;
use crate::local_search::{allowed, local_search_within};
use crate::matrix::DistanceMatrix;
use crate::moves::Move;
use crate::tour::Tour;

// The schedule. The four values come from a sweep over the TSPLIB instances in `tests/`
// and random instances of 30 to 250 stops; D17 in docs/architecture.md has the numbers.

/// Temperature levels in one epoch.
const LEVELS: u32 = 100;

/// What the temperature is multiplied by from one level to the next. An epoch ends
/// at a twentieth of the temperature it started at.
const COOLING: f64 = 0.97;

/// Moves tried at each level, per node of the tour.
const MOVES_PER_NODE: usize = 20;

/// The temperature an epoch starts at, as a share of the tour's mean edge length.
const START_TEMPERATURE: f64 = 0.5;

/// Local search is exact for up to three stops, so smaller tours are left alone.
const FEWEST_NODES: usize = 5;

/// Anneals the tour until the control stops it, and leaves the best tour seen in `tour`.
///
/// The search runs in epochs of a fixed length (D5). An epoch starts from the best tour
/// at the start temperature and cools level by level; a move that lengthens the tour by
/// `delta` is taken with probability `exp(-delta / temperature)`. At the end of an
/// epoch the best tour goes through [`local_search`].
///
/// The schedule depends on the tour and on `seed` only. The control stops the search
/// and never steers it, so the same seed and budget always give the same tour.
///
/// There is no test for convergence (D18): with four or more stops this returns only
/// when the control says so. With fewer it returns at once.
///
/// # Panics
///
/// If the tour was built on a matrix of another size.
pub fn anneal(matrix: &DistanceMatrix, tour: &mut Tour, seed: u64, control: &mut Control<'_>) {
    anneal_within(matrix, None, tour, seed, control);
}

/// [`anneal`] for a courier that can carry only so much (D65). A tour that fits when
/// it is handed over fits afterwards, and so does every tour reported on the way.
///
/// A move is first drawn, and taken or not by its length, exactly as without loads.
/// One that is taken is then made only if the tour still fits. So the loads are asked
/// about the few moves that would be made and not about the many that are looked at.
/// Asking them draws no random number, which is why loads that are never exceeded
/// leave the search on the very course it takes without them.
pub(crate) fn anneal_within(
    matrix: &DistanceMatrix,
    loads: Option<&Loads>,
    tour: &mut Tour,
    seed: u64,
    control: &mut Control<'_>,
) {
    tour.check(matrix);
    let nodes = tour.order.len();
    if nodes < FEWEST_NODES {
        return;
    }
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let start_temperature = START_TEMPERATURE * tour.cost as f64 / nodes as f64;
    let moves_per_level = MOVES_PER_NODE * nodes;
    let mut current = tour.clone();
    let mut scratch = Vec::new();
    loop {
        current.copy_from(tour);
        let mut temperature = start_temperature;
        for _ in 0..LEVELS {
            for _ in 0..moves_per_level {
                if !control.spend(1, tour) {
                    return;
                }
                let change = draw(&mut rng, nodes);
                let delta = change.delta(matrix, &current.order);
                // At temperature zero the exponent is minus infinity and nothing longer is taken.
                if (delta <= 0 || rng.random::<f64>() < (-(delta as f64) / temperature).exp())
                    && allowed(loads, &current.order, change, &mut scratch)
                {
                    current.apply(change, delta);
                    if current.cost < tour.cost {
                        tour.copy_from(&current);
                    }
                }
            }
            temperature *= COOLING;
        }
        local_search_within(matrix, loads, tour, control);
    }
}

/// Draws a move between two different positions after the depot.
///
/// Half of the moves are reversals; relocating one node and swapping two share the rest.
fn draw(rng: &mut ChaCha8Rng, nodes: usize) -> Move {
    let a = rng.random_range(1..nodes);
    let mut b = rng.random_range(1..nodes - 1);
    if b >= a {
        b += 1;
    }
    let (first, second) = (a.min(b), a.max(b));
    match rng.random_range(0..4u32) {
        0 | 1 => Move::Reverse {
            first,
            last: second,
        },
        2 => {
            // The node at position `a` ends up at position `b`.
            let anchor = if b > a { b } else { b - 1 };
            Move::Shift {
                start: a,
                len: 1,
                anchor,
                reversed: false,
            }
        }
        _ => Move::Swap { first, second },
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::construct::nearest_neighbour_within;
    use crate::testing;
    use crate::tour::cycle_cost;

    proptest! {
        #[test]
        fn every_drawn_move_is_valid(nodes in FEWEST_NODES..40, seed: u64) {
            let mut rng = ChaCha8Rng::seed_from_u64(seed);
            let matrix = DistanceMatrix::from_fn(nodes, |from, to| (from + to) as u32);
            let order: Vec<usize> = (0..nodes).collect();
            for _ in 0..200 {
                let change = draw(&mut rng, nodes);
                let mut moved = order.clone();
                change.apply(&mut moved);
                prop_assert!(testing::is_tour(&matrix, &moved), "{change:?} left {moved:?}");
                let recomputed = cycle_cost(&matrix, &moved) as i64 - cycle_cost(&matrix, &order) as i64;
                prop_assert_eq!(change.delta(&matrix, &order), recomputed, "{:?}", change);
            }
        }

        #[test]
        fn the_best_tour_seen_is_kept(
            (matrix, order) in testing::matrix_with_tour(0..=12),
            seed: u64,
            budget in 0..20_000u64,
        ) {
            let mut tour = Tour::new(&matrix, order);
            let before = tour.cost();
            let mut control = Control::max_iterations(budget);
            anneal(&matrix, &mut tour, seed, &mut control);

            prop_assert!(control.iterations() <= budget);
            prop_assert!(testing::is_tour(&matrix, &tour.order));
            prop_assert_eq!(tour.cost(), cycle_cost(&matrix, &tour.order));
            prop_assert!(tour.cost() <= before);
        }
    }

    proptest! {
        /// For a courier that can carry only so much: the tour it is given fits, and
        /// so does the one it is left with.
        #[test]
        fn a_tour_that_fits_still_fits_afterwards(
            matrix in testing::matrix(0..=12),
            numbers in prop::collection::vec((0..=6u32, 0..=6u32), 12),
            slack in 0..=4u32,
            seed: u64,
            budget in 0..20_000u64,
        ) {
            let stops = testing::demands(matrix.len(), &numbers);
            let loads = testing::loads(&stops, slack);
            let mut tour = nearest_neighbour_within(&matrix, Some(&loads));
            let before = tour.cost();
            let mut control = Control::max_iterations(budget);
            anneal_within(&matrix, Some(&loads), &mut tour, seed, &mut control);

            prop_assert!(control.iterations() <= budget);
            prop_assert!(testing::is_tour(&matrix, &tour.order));
            prop_assert!(loads.fits(&tour.order));
            prop_assert_eq!(tour.cost(), cycle_cost(&matrix, &tour.order));
            prop_assert!(tour.cost() <= before);
        }

        /// Loads that no order of the stops can exceed are asked, say yes every time,
        /// and leave the search on the course it takes without them.
        #[test]
        fn loads_that_cannot_be_exceeded_change_nothing(
            (matrix, order) in testing::matrix_with_tour(0..=12),
            numbers in prop::collection::vec((0..=6u32, 0..=6u32), 12),
            seed: u64,
            budget in 0..20_000u64,
        ) {
            let stops = testing::demands(matrix.len(), &numbers);
            let loads = testing::loads(&stops, 12 * 6);

            let mut free = Tour::new(&matrix, order.clone());
            let mut free_control = Control::max_iterations(budget);
            anneal(&matrix, &mut free, seed, &mut free_control);

            let mut loaded = Tour::new(&matrix, order);
            let mut loaded_control = Control::max_iterations(budget);
            anneal_within(&matrix, Some(&loads), &mut loaded, seed, &mut loaded_control);

            prop_assert_eq!(&loaded, &free);
            prop_assert_eq!(loaded_control.iterations(), free_control.iterations());
        }
    }

    /// The tour that fits is the longer one, and annealing does not leave it for the
    /// shorter one that overloads, however long it looks.
    #[test]
    fn a_shorter_tour_that_overloads_is_never_kept() {
        // The square, and beside its corners two stops that ask for nothing, so that
        // there are enough nodes for annealing to run at all.
        let corners = [
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (0.0, 10.0),
            (5.0, 0.0),
            (5.0, 10.0),
        ];
        let points: Vec<crate::Point> = corners
            .iter()
            .map(|&(x, y)| crate::Point::new(x, y))
            .collect();
        let matrix = DistanceMatrix::euclidean(&points);
        let stops = [
            crate::Demand::pickup(1),
            crate::Demand::delivery(2),
            crate::Demand::pickup(1),
            crate::Demand::default(),
            crate::Demand::default(),
        ];
        let loads = testing::loads(&stops, 0);

        for seed in 0..20 {
            let mut tour = nearest_neighbour_within(&matrix, Some(&loads));
            let mut control = Control::max_iterations(200_000);
            anneal_within(&matrix, Some(&loads), &mut tour, seed, &mut control);
            assert!(loads.fits(&tour.order), "seed {seed}: {:?}", tour.order);
            assert_eq!(
                tour.cost(),
                testing::optimum_within(&matrix, Some(&loads)),
                "seed {seed}"
            );
            // Round the square is 40 metres, and comes to a pickup first either way.
            assert!(tour.cost() > testing::optimum(&matrix), "seed {seed}");
        }
    }

    #[test]
    fn small_tours_are_left_alone() {
        let matrix = DistanceMatrix::from_fn(4, |from, to| (from + to) as u32);
        let mut tour = Tour::new(&matrix, vec![0, 3, 1, 2]);
        let mut control = Control::max_iterations(u64::MAX);
        anneal(&matrix, &mut tour, 1, &mut control);
        assert_eq!(tour.order, [0, 3, 1, 2]);
        assert_eq!(control.iterations(), 0);
    }

    #[test]
    fn a_tour_of_length_zero_survives() {
        // Every stop at the depot: the start temperature is zero.
        let matrix = DistanceMatrix::from_fn(8, |_, _| 0);
        let mut tour = Tour::new(&matrix, (0..8).collect());
        let mut control = Control::max_iterations(50_000);
        anneal(&matrix, &mut tour, 1, &mut control);
        assert_eq!(tour.cost(), 0);
        assert!(testing::is_tour(&matrix, &tour.order));
    }
}
