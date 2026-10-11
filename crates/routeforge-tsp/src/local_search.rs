//! Local search: 2-opt and Or-opt, each run until it finds nothing more.

use crate::control::Control;
use crate::matrix::DistanceMatrix;
use crate::moves::Move;
use crate::tour::Tour;

/// The longest run of stops Or-opt moves as one piece.
const LONGEST_SEGMENT: usize = 3;

/// Tours with fewer nodes have a single shape, so there is nothing to search.
const FEWEST_NODES: usize = 4;

/// Reverses parts of the tour for as long as a reversal shortens it.
///
/// Returns whether the tour got shorter. Unless the control stops the search first,
/// no single reversal can shorten the tour afterwards.
///
/// # Panics
///
/// If the tour was built on a matrix of another size.
pub fn two_opt(matrix: &DistanceMatrix, tour: &mut Tour, control: &mut Control<'_>) -> bool {
    tour.check(matrix);
    let nodes = tour.order.len();
    if nodes < FEWEST_NODES {
        return false;
    }
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
                if delta < 0 {
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
    tour.check(matrix);
    let nodes = tour.order.len();
    if nodes < FEWEST_NODES {
        return false;
    }
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
                let best = shifts
                    .map(|shift| (shift.delta(matrix, &tour.order), shift))
                    .min_by_key(|&(delta, _)| delta);
                if let Some((delta, shift)) = best
                    && delta < 0
                {
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
    let mut improved = two_opt(matrix, tour, control);
    // Each stage leaves the tour at its own optimum. As soon as one of them changes
    // nothing, the tour is still at the other's optimum as well.
    while or_opt(matrix, tour, control) {
        improved = true;
        if !two_opt(matrix, tour, control) {
            break;
        }
    }
    improved
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::control::StopReason;
    use crate::testing;
    use crate::tour::cycle_cost;

    fn unlimited() -> Control<'static> {
        Control::max_iterations(u64::MAX)
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
            prop_assert_eq!(testing::shorter_by_reversal(&matrix, &tour.order), None);
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
            prop_assert_eq!(testing::shorter_by_shift(&matrix, &tour.order), None);
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
            prop_assert_eq!(testing::shorter_by_reversal(&matrix, &tour.order), None);
            prop_assert_eq!(testing::shorter_by_shift(&matrix, &tour.order), None);
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
