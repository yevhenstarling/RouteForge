//! Local search: 2-opt and Or-opt, each run until it finds nothing more.
//!
//! Each stage is there twice. The public one searches a tour with nothing on board.
//! The one that ends in `_within` minds what the courier carries as well (D65): of the
//! moves that shorten the tour it makes those that leave the courier within its
//! capacity all along it. Given no loads the two are the same search, move for move.

use crate::control::Control;
use crate::loads::Loads;
use crate::matrix::DistanceMatrix;
use crate::moves::Move;
use crate::tour::Tour;

/// The longest run of stops Or-opt moves as one piece.
const LONGEST_SEGMENT: usize = 3;

/// Tours with fewer nodes have a single shape, so there is nothing to search.
const FEWEST_NODES: usize = 4;

/// Whether a move may be made: always where there are no loads to mind, and otherwise
/// if the tour still fits afterwards.
#[inline]
pub(crate) fn allowed(
    loads: Option<&Loads>,
    order: &[usize],
    change: Move,
    scratch: &mut Vec<usize>,
) -> bool {
    loads.is_none_or(|loads| loads.allows(order, change, scratch))
}

/// Reverses parts of the tour for as long as a reversal shortens it.
///
/// Returns whether the tour got shorter. Unless the control stops the search first,
/// no single reversal can shorten the tour afterwards.
///
/// # Panics
///
/// If the tour was built on a matrix of another size.
pub fn two_opt(matrix: &DistanceMatrix, tour: &mut Tour, control: &mut Control<'_>) -> bool {
    two_opt_within(matrix, None, tour, control)
}

/// [`two_opt`] for a courier that can carry only so much: afterwards no reversal that
/// the loads allow can shorten the tour.
pub(crate) fn two_opt_within(
    matrix: &DistanceMatrix,
    loads: Option<&Loads>,
    tour: &mut Tour,
    control: &mut Control<'_>,
) -> bool {
    tour.check(matrix);
    let nodes = tour.order.len();
    if nodes < FEWEST_NODES {
        return false;
    }
    let mut scratch = Vec::new();
    let mut improved = false;
    loop {
        let mut improved_in_this_pass = false;
        for first in 1..nodes - 1 {
            if !control.spend((nodes - 1 - first) as u64, tour) {
                return improved || improved_in_this_pass;
            }
            for last in first + 1..nodes {
                let reversal = Move::Reverse { first, last };
                let delta = reversal.delta(matrix, &tour.order);
                if delta < 0 && allowed(loads, &tour.order, reversal, &mut scratch) {
                    tour.apply(reversal, delta);
                    improved_in_this_pass = true;
                }
            }
        }
        if !improved_in_this_pass {
            return improved;
        }
        improved = true;
    }
}

/// Moves runs of one to three stops elsewhere for as long as that shortens the tour.
///
/// Each run goes to the best place among all other edges, the right way round or back
/// to front. Returns whether the tour got shorter. Unless the control stops the search
/// first, no single such move can shorten the tour afterwards.
///
/// # Panics
///
/// If the tour was built on a matrix of another size.
pub fn or_opt(matrix: &DistanceMatrix, tour: &mut Tour, control: &mut Control<'_>) -> bool {
    or_opt_within(matrix, None, tour, control)
}

/// [`or_opt`] for a courier that can carry only so much: each run goes to the best
/// place the loads allow, and afterwards no such move that they allow can shorten the
/// tour.
pub(crate) fn or_opt_within(
    matrix: &DistanceMatrix,
    loads: Option<&Loads>,
    tour: &mut Tour,
    control: &mut Control<'_>,
) -> bool {
    tour.check(matrix);
    let nodes = tour.order.len();
    if nodes < FEWEST_NODES {
        return false;
    }
    let mut scratch = Vec::new();
    let mut improved = false;
    loop {
        let mut improved_in_this_pass = false;
        // A run of all the stops could only be turned round, which changes nothing.
        for len in 1..=LONGEST_SEGMENT.min(nodes - 2) {
            for start in 1..=nodes - len {
                // Everywhere but inside the run and the place it is in now.
                let anchors = (0..start - 1).chain(start + len..nodes);
                // Turning a single stop round changes nothing.
                let ways_round = if len == 1 { 1 } else { 2 };
                if !control.spend((ways_round * (nodes - len - 1)) as u64, tour) {
                    return improved || improved_in_this_pass;
                }
                let shifts = anchors.flat_map(|anchor| {
                    [false, true]
                        .into_iter()
                        .take(ways_round)
                        .map(move |reversed| Move::Shift {
                            start,
                            len,
                            anchor,
                            reversed,
                        })
                });
                // The loads are asked only about a shift that would shorten the tour.
                // Without loads this is the shortest of all the shifts, if that one
                // shortens the tour: the first of them, should several be as short.
                let best = shifts
                    .map(|shift| (shift.delta(matrix, &tour.order), shift))
                    .filter(|&(delta, shift)| {
                        delta < 0 && allowed(loads, &tour.order, shift, &mut scratch)
                    })
                    .min_by_key(|&(delta, _)| delta);
                if let Some((delta, shift)) = best {
                    tour.apply(shift, delta);
                    improved_in_this_pass = true;
                }
            }
        }
        if !improved_in_this_pass {
            return improved;
        }
        improved = true;
    }
}

/// Alternates [`two_opt`] and [`or_opt`] until neither finds anything.
///
/// Returns whether the tour got shorter. Every move shortens the tour by at least a
/// metre, so the search ends even without a limit.
///
/// # Panics
///
/// If the tour was built on a matrix of another size.
pub fn local_search(matrix: &DistanceMatrix, tour: &mut Tour, control: &mut Control<'_>) -> bool {
    local_search_within(matrix, None, tour, control)
}

/// [`local_search`] for a courier that can carry only so much. A tour that fits when
/// it is handed over fits afterwards.
pub(crate) fn local_search_within(
    matrix: &DistanceMatrix,
    loads: Option<&Loads>,
    tour: &mut Tour,
    control: &mut Control<'_>,
) -> bool {
    let mut improved = two_opt_within(matrix, loads, tour, control);
    // Each stage leaves the tour at its own optimum. As soon as one of them changes
    // nothing, the tour is still at the other's optimum as well.
    while or_opt_within(matrix, loads, tour, control) {
        improved = true;
        if !two_opt_within(matrix, loads, tour, control) {
            break;
        }
    }
    improved
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::construct::nearest_neighbour_within;
    use crate::control::StopReason;
    use crate::testing;
    use crate::tour::cycle_cost;

    fn unlimited() -> Control<'static> {
        Control::max_iterations(u64::MAX)
    }

    /// The shortest tour begins with a pickup the courier has no room for yet, either
    /// way round. Local search starts from the tour that fits and leaves it alone,
    /// where without loads it would go round the square.
    #[test]
    fn a_shorter_tour_that_overloads_is_not_taken() {
        let (matrix, loads) = testing::square_with_the_delivery_opposite();
        let mut tour = Tour::new(&matrix, vec![0, 2, 1, 3]);
        assert_eq!(tour.cost(), 48);

        assert!(!local_search_within(
            &matrix,
            Some(&loads),
            &mut tour,
            &mut unlimited()
        ));
        assert_eq!(tour.order, [0, 2, 1, 3]);

        assert!(local_search(&matrix, &mut tour, &mut unlimited()));
        assert_eq!(tour.cost(), 40);
        assert!(!loads.fits(&tour.order));
    }

    /// Of the places a stop could go, Or-opt takes the best that the loads allow, and
    /// not the best there is.
    #[test]
    fn or_opt_takes_the_best_place_the_loads_allow() {
        // Stops on a line at 10, 20 and 30 metres: a pickup, then two deliveries, for
        // a courier that leaves full. The pickup is called at between the deliveries.
        let at = [0u32, 10, 20, 30];
        let matrix = DistanceMatrix::from_fn(4, |from, to| at[from].abs_diff(at[to]));
        let stops = [
            crate::Demand::pickup(2),
            crate::Demand::delivery(2),
            crate::Demand::delivery(2),
        ];
        let loads = testing::loads(&stops, 0);
        let mut tour = Tour::new(&matrix, vec![0, 2, 1, 3]);
        assert_eq!(tour.cost(), 80);

        // First is the best place for it by length, and the courier is full there.
        // Last is as short, on the way home.
        assert!(or_opt_within(
            &matrix,
            Some(&loads),
            &mut tour,
            &mut unlimited()
        ));
        assert_eq!(tour.cost(), 60);
        assert!(loads.fits(&tour.order));
        assert_eq!(tour.order, [0, 2, 3, 1]);
    }

    proptest! {
        #[test]
        fn two_opt_ends_where_no_reversal_helps(
            (matrix, order) in testing::matrix_with_tour(0..=12),
        ) {
            let mut tour = Tour::new(&matrix, order);
            let before = tour.cost();
            let mut control = unlimited();
            let improved = two_opt(&matrix, &mut tour, &mut control);

            prop_assert_eq!(control.stop_reason(), None);
            prop_assert!(testing::is_tour(&matrix, &tour.order));
            prop_assert_eq!(tour.cost(), cycle_cost(&matrix, &tour.order));
            prop_assert_eq!(improved, tour.cost() < before);
            prop_assert_eq!(testing::shorter_by_reversal(&matrix, None, &tour.order), None);
        }

        #[test]
        fn or_opt_ends_where_no_shift_helps(
            (matrix, order) in testing::matrix_with_tour(0..=12),
        ) {
            let mut tour = Tour::new(&matrix, order);
            let before = tour.cost();
            let mut control = unlimited();
            let improved = or_opt(&matrix, &mut tour, &mut control);

            prop_assert_eq!(control.stop_reason(), None);
            prop_assert!(testing::is_tour(&matrix, &tour.order));
            prop_assert_eq!(tour.cost(), cycle_cost(&matrix, &tour.order));
            prop_assert_eq!(improved, tour.cost() < before);
            prop_assert_eq!(testing::shorter_by_shift(&matrix, None, &tour.order), None);
        }

        #[test]
        fn local_search_ends_where_neither_move_helps(
            (matrix, order) in testing::matrix_with_tour(0..=12),
        ) {
            let mut tour = Tour::new(&matrix, order);
            let before = tour.cost();
            let improved = local_search(&matrix, &mut tour, &mut unlimited());

            prop_assert!(testing::is_tour(&matrix, &tour.order));
            prop_assert_eq!(tour.cost(), cycle_cost(&matrix, &tour.order));
            prop_assert_eq!(improved, tour.cost() < before);
            prop_assert_eq!(testing::shorter_by_reversal(&matrix, None, &tour.order), None);
            prop_assert_eq!(testing::shorter_by_shift(&matrix, None, &tour.order), None);
        }

        /// The same three, for a courier that can carry only so much: each starts from
        /// a tour the courier can drive, and ends at one where no move it can make
        /// helps.
        #[test]
        fn the_stages_end_where_no_move_the_loads_allow_helps(
            matrix in testing::matrix(0..=12),
            numbers in prop::collection::vec((0..=6u32, 0..=6u32), 12),
            slack in 0..=4u32,
        ) {
            let stops = testing::demands(matrix.len(), &numbers);
            let loads = testing::loads(&stops, slack);
            let start = nearest_neighbour_within(&matrix, Some(&loads));
            let before = start.cost();

            let mut reversed = start.clone();
            let improved = two_opt_within(&matrix, Some(&loads), &mut reversed, &mut unlimited());
            prop_assert!(testing::is_tour(&matrix, &reversed.order));
            prop_assert!(loads.fits(&reversed.order));
            prop_assert_eq!(reversed.cost(), cycle_cost(&matrix, &reversed.order));
            prop_assert_eq!(improved, reversed.cost() < before);
            prop_assert_eq!(
                testing::shorter_by_reversal(&matrix, Some(&loads), &reversed.order),
                None
            );

            let mut shifted = start.clone();
            let improved = or_opt_within(&matrix, Some(&loads), &mut shifted, &mut unlimited());
            prop_assert!(testing::is_tour(&matrix, &shifted.order));
            prop_assert!(loads.fits(&shifted.order));
            prop_assert_eq!(shifted.cost(), cycle_cost(&matrix, &shifted.order));
            prop_assert_eq!(improved, shifted.cost() < before);
            prop_assert_eq!(
                testing::shorter_by_shift(&matrix, Some(&loads), &shifted.order),
                None
            );

            let mut searched = start;
            let improved = local_search_within(&matrix, Some(&loads), &mut searched, &mut unlimited());
            prop_assert!(testing::is_tour(&matrix, &searched.order));
            prop_assert!(loads.fits(&searched.order));
            prop_assert_eq!(searched.cost(), cycle_cost(&matrix, &searched.order));
            prop_assert_eq!(improved, searched.cost() < before);
            prop_assert_eq!(
                testing::shorter_by_reversal(&matrix, Some(&loads), &searched.order),
                None
            );
            prop_assert_eq!(
                testing::shorter_by_shift(&matrix, Some(&loads), &searched.order),
                None
            );
        }

        /// With up to three stops, every order of them is one move from every other.
        /// So a search that ends where no move helps has found the shortest tour the
        /// courier can drive, and annealing has nothing to add (D18).
        #[test]
        fn up_to_three_stops_are_searched_exactly_within_loads(
            matrix in prop_oneof![testing::matrix(0..=4), testing::euclidean(0..=4)],
            numbers in prop::collection::vec((0..=6u32, 0..=6u32), 3),
            slack in 0..=3u32,
        ) {
            let stops = testing::demands(matrix.len(), &numbers);
            let loads = testing::loads(&stops, slack);
            let mut tour = nearest_neighbour_within(&matrix, Some(&loads));
            local_search_within(&matrix, Some(&loads), &mut tour, &mut unlimited());
            prop_assert!(loads.fits(&tour.order));
            prop_assert_eq!(tour.cost(), testing::optimum_within(&matrix, Some(&loads)));
        }

        /// A search cut short has made only moves the loads allow, so what it leaves
        /// is a tour the courier can drive.
        #[test]
        fn a_search_within_loads_that_is_cut_short_leaves_a_tour_that_fits(
            matrix in testing::matrix(0..=12),
            numbers in prop::collection::vec((0..=6u32, 0..=6u32), 12),
            budget in 0..400u64,
        ) {
            let stops = testing::demands(matrix.len(), &numbers);
            let loads = testing::loads(&stops, 0);
            let mut tour = nearest_neighbour_within(&matrix, Some(&loads));
            let before = tour.cost();
            let mut control = Control::max_iterations(budget);
            local_search_within(&matrix, Some(&loads), &mut tour, &mut control);

            prop_assert!(control.iterations() <= budget);
            prop_assert!(testing::is_tour(&matrix, &tour.order));
            prop_assert!(loads.fits(&tour.order));
            prop_assert_eq!(tour.cost(), cycle_cost(&matrix, &tour.order));
            prop_assert!(tour.cost() <= before);
        }

        /// Loads that no order of the stops can exceed are no loads to the search: it
        /// takes the same course and spends the same moves.
        #[test]
        fn loads_that_cannot_be_exceeded_change_nothing(
            (matrix, order) in testing::matrix_with_tour(0..=12),
            numbers in prop::collection::vec((0..=6u32, 0..=6u32), 12),
            budget in 0..2_000u64,
        ) {
            let stops = testing::demands(matrix.len(), &numbers);
            let loads = testing::loads(&stops, 12 * 6);

            let mut free = Tour::new(&matrix, order.clone());
            let mut free_control = Control::max_iterations(budget);
            local_search(&matrix, &mut free, &mut free_control);

            let mut loaded = Tour::new(&matrix, order);
            let mut loaded_control = Control::max_iterations(budget);
            local_search_within(&matrix, Some(&loads), &mut loaded, &mut loaded_control);

            prop_assert_eq!(&loaded, &free);
            prop_assert_eq!(loaded_control.iterations(), free_control.iterations());
        }

        #[test]
        fn a_search_cut_short_still_leaves_a_tour_that_is_no_longer(
            (matrix, order) in testing::matrix_with_tour(0..=12),
            budget in 0..400u64,
        ) {
            let mut tour = Tour::new(&matrix, order);
            let before = tour.cost();
            let mut control = Control::max_iterations(budget);
            local_search(&matrix, &mut tour, &mut control);

            prop_assert!(control.iterations() <= budget);
            prop_assert!(testing::is_tour(&matrix, &tour.order));
            prop_assert_eq!(tour.cost(), cycle_cost(&matrix, &tour.order));
            prop_assert!(tour.cost() <= before);
        }

        #[test]
        fn a_longer_search_continues_the_shorter_one(
            (matrix, order) in testing::matrix_with_tour(4..=12),
            budget in 0..400u64,
            extra in 0..400u64,
        ) {
            let run = |budget| {
                let mut tour = Tour::new(&matrix, order.clone());
                local_search(&matrix, &mut tour, &mut Control::max_iterations(budget));
                tour.cost()
            };
            prop_assert!(run(budget + extra) <= run(budget));
        }
    }

    #[test]
    fn two_opt_uncrosses_a_tour() {
        // The corners of a square, visited along both diagonals.
        let matrix = DistanceMatrix::new(
            4,
            vec![0, 10, 14, 10, 10, 0, 10, 14, 14, 10, 0, 10, 10, 14, 10, 0],
        )
        .unwrap();
        let mut tour = Tour::new(&matrix, vec![0, 2, 1, 3]);
        assert_eq!(tour.cost(), 48);
        assert!(two_opt(&matrix, &mut tour, &mut unlimited()));
        assert_eq!(tour.cost(), 40);
    }

    #[test]
    fn or_opt_moves_a_stop_to_where_it_belongs() {
        // Stops on a line. The stop at 10 metres is visited last, on the way back from 40.
        let at = [0u32, 20, 30, 40, 10];
        let matrix = DistanceMatrix::from_fn(5, |from, to| at[from].abs_diff(at[to]));
        let mut tour = Tour::new(&matrix, vec![0, 1, 2, 3, 4]);
        assert_eq!(tour.cost(), 80);
        // Already as short as a tour of this line can be, so nothing to do.
        assert!(!or_opt(&matrix, &mut tour, &mut unlimited()));

        let mut detour = Tour::new(&matrix, vec![0, 1, 4, 2, 3]);
        assert_eq!(detour.cost(), 100);
        assert!(or_opt(&matrix, &mut detour, &mut unlimited()));
        assert_eq!(detour.cost(), 80);
    }

    #[test]
    fn a_search_out_of_budget_says_so() {
        let matrix = DistanceMatrix::from_fn(6, |from, to| (from * 7 + to * 13) as u32 % 50);
        let mut tour = Tour::new(&matrix, vec![0, 1, 2, 3, 4, 5]);
        let mut control = Control::max_iterations(3);
        local_search(&matrix, &mut tour, &mut control);
        assert_eq!(control.stop_reason(), Some(StopReason::Budget));
        assert!(control.iterations() <= 3);
    }
}
