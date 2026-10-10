//! Shared by the unit tests: input strategies and slow, obviously correct checkers.

use std::ops::RangeInclusive;

use proptest::prelude::*;

use crate::matrix::DistanceMatrix;
use crate::problem::Point;
use crate::tour::cycle_cost;

/// A symmetric matrix with arbitrary distances, not necessarily a metric.
///
/// Most distances are short. Some are as long as a `u32` allows, which pushes the
/// length of a tour beyond 32 bits.
pub(crate) fn matrix(nodes: RangeInclusive<usize>) -> impl Strategy<Value = DistanceMatrix> {
    let distance = prop_oneof![4 => 0..=1_000u32, 1 => any::<u32>()];
    nodes.prop_flat_map(move |nodes| {
        let pairs = nodes * nodes.saturating_sub(1) / 2;
        prop::collection::vec(distance.clone(), pairs).prop_map(move |distances| {
            let mut distances = distances.into_iter();
            DistanceMatrix::from_fn(nodes, |_, _| distances.next().unwrap_or_default())
        })
    })
}

/// Straight-line distances between points scattered over a square of two kilometres.
pub(crate) fn euclidean(nodes: RangeInclusive<usize>) -> impl Strategy<Value = DistanceMatrix> {
    let coordinate = 0.0..2_000.0f64;
    prop::collection::vec((coordinate.clone(), coordinate), nodes).prop_map(|points| {
        let points: Vec<Point> = points.into_iter().map(|(x, y)| Point::new(x, y)).collect();
        DistanceMatrix::euclidean(&points)
    })
}

/// A matrix and the order of a tour over it: the depot, then the stops shuffled.
pub(crate) fn matrix_with_tour(
    nodes: RangeInclusive<usize>,
) -> impl Strategy<Value = (DistanceMatrix, Vec<usize>)> {
    matrix(nodes)
        .prop_flat_map(|matrix| {
            let stops: Vec<usize> = (1..matrix.len()).collect();
            (Just(matrix), Just(stops).prop_shuffle())
        })
        .prop_map(|(matrix, stops)| {
            let order = (0..matrix.len()).take(1).chain(stops).collect();
            (matrix, order)
        })
}

/// Whether `order` starts at the depot and visits every node of the matrix once.
pub(crate) fn is_tour(matrix: &DistanceMatrix, order: &[usize]) -> bool {
    let mut sorted = order.to_vec();
    sorted.sort_unstable();
    order.first().is_none_or(|&first| first == 0) && sorted.into_iter().eq(0..matrix.len())
}

/// The length of the shortest closed tour, found by trying every order of the stops.
pub(crate) fn optimum(matrix: &DistanceMatrix) -> u64 {
    if matrix.len() < 2 {
        return 0;
    }
    let mut left: Vec<usize> = (1..matrix.len()).collect();
    let mut best = u64::MAX;
    extend(matrix, 0, &mut left, 0, &mut best);
    best
}

/// Tries every way to finish a tour that has driven `so_far` metres and stands at `at`.
fn extend(matrix: &DistanceMatrix, at: usize, left: &mut Vec<usize>, so_far: u64, best: &mut u64) {
    // Distances are not negative, so a tour that is already too long cannot recover.
    if so_far >= *best {
        return;
    }
    if left.is_empty() {
        *best = (*best).min(so_far + u64::from(matrix.get(at, 0)));
        return;
    }
    for slot in 0..left.len() {
        let next = left.swap_remove(slot);
        extend(
            matrix,
            next,
            left,
            so_far + u64::from(matrix.get(at, next)),
            best,
        );
        left.push(next);
        let last = left.len() - 1;
        left.swap(slot, last);
    }
}

/// A shorter tour one 2-opt reversal away from `order`, if there is one.
///
/// Like [`shorter_by_shift`] it builds every neighbour and measures it from scratch, so
/// it shares no code with the moves under test.
pub(crate) fn shorter_by_reversal(matrix: &DistanceMatrix, order: &[usize]) -> Option<Vec<usize>> {
    let cost = cycle_cost(matrix, order);
    for first in 1..order.len() {
        for last in first + 1..order.len() {
            let mut neighbour = order.to_vec();
            neighbour[first..=last].reverse();
            if cycle_cost(matrix, &neighbour) < cost {
                return Some(neighbour);
            }
        }
    }
    None
}

/// A shorter tour one Or-opt move away from `order`, if there is one: a run of one to
/// three stops taken out and put back anywhere else, either way round.
pub(crate) fn shorter_by_shift(matrix: &DistanceMatrix, order: &[usize]) -> Option<Vec<usize>> {
    let cost = cycle_cost(matrix, order);
    for len in 1..=3 {
        for start in 1..(order.len() + 1).saturating_sub(len) {
            let mut rest = order.to_vec();
            let mut run: Vec<usize> = rest.drain(start..start + len).collect();
            for _way_round in 0..2 {
                for slot in 1..=rest.len() {
                    let mut neighbour = rest.clone();
                    neighbour.splice(slot..slot, run.iter().copied());
                    if cycle_cost(matrix, &neighbour) < cost {
                        return Some(neighbour);
                    }
                }
                run.reverse();
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optimum_of_a_square_is_its_perimeter() {
        let corners = [(0.0, 0.0), (300.0, 400.0), (0.0, 400.0), (300.0, 0.0)];
        let points: Vec<Point> = corners.iter().map(|&(x, y)| Point::new(x, y)).collect();
        assert_eq!(optimum(&DistanceMatrix::euclidean(&points)), 1_400);
    }

    #[test]
    fn optimum_without_stops_is_zero() {
        assert_eq!(optimum(&DistanceMatrix::from_fn(0, |_, _| 0)), 0);
        assert_eq!(optimum(&DistanceMatrix::new(1, vec![9]).unwrap()), 0);
    }

    #[test]
    fn the_checkers_find_an_obvious_improvement() {
        // Stops on a line; visiting them out of order doubles back.
        let at = [0u32, 10, 20, 30, 40];
        let matrix = DistanceMatrix::from_fn(5, |from, to| at[from].abs_diff(at[to]));
        assert!(shorter_by_reversal(&matrix, &[0, 2, 1, 3, 4]).is_some());
        assert!(shorter_by_shift(&matrix, &[0, 2, 1, 3, 4]).is_some());
        assert_eq!(shorter_by_reversal(&matrix, &[0, 1, 2, 3, 4]), None);
        assert_eq!(shorter_by_shift(&matrix, &[0, 1, 2, 3, 4]), None);
    }
}
