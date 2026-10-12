//! Shared by the unit tests: input strategies and slow, obviously correct checkers.

use std::ops::RangeInclusive;

use proptest::prelude::*;

use crate::loads::Loads;
use crate::matrix::DistanceMatrix;
use crate::problem::{Demand, Point};
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

/// What the stops of a route of `nodes` nodes ask for, made of pairs of numbers. A
/// test draws as many pairs as its largest route has stops, and this takes what the
/// route at hand needs.
///
/// Mostly a stop is brought something or has something taken away, as an order is
/// (D1), and now and then both.
pub(crate) fn demands(nodes: usize, numbers: &[(u32, u32)]) -> Vec<Demand> {
    let stops = nodes.saturating_sub(1);
    assert!(numbers.len() >= stops, "too few numbers for {stops} stops");
    numbers[..stops]
        .iter()
        .map(|&(which, parcels)| match which % 4 {
            0 | 1 => Demand::delivery(parcels),
            2 => Demand::pickup(parcels),
            _ => Demand {
                delivery: which,
                pickup: parcels,
            },
        })
        .collect()
}

/// Loads for the stops, on a courier that carries `slack` more than the least a
/// courier has to. With no slack the courier is as small as the route allows, and
/// many an order of its stops overloads it.
pub(crate) fn loads(stops: &[Demand], slack: u32) -> Loads {
    let out: u32 = stops.iter().map(|stop| stop.delivery).sum();
    let back: u32 = stops.iter().map(|stop| stop.pickup).sum();
    Loads::new(out.max(back) + slack, stops).expect("the courier carries either")
}

/// What the stops of [`square_with_the_delivery_opposite`] ask for.
pub(crate) const SQUARE_STOPS: [Demand; 3] =
    [Demand::pickup(1), Demand::delivery(2), Demand::pickup(1)];

/// The depot and three stops on the corners of a square of ten metres, for a courier
/// that leaves full: the stop opposite the depot is a delivery of two, and its
/// neighbours are a pickup of one each.
///
/// Round the square is the shortest tour, 40 metres, and it begins with a pickup
/// whichever way it is driven. The shortest tour the courier can drive goes to the
/// delivery first, along a diagonal, and is 48 metres long.
pub(crate) fn square_with_the_delivery_opposite() -> (DistanceMatrix, Loads) {
    let corners = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
    let points: Vec<Point> = corners.iter().map(|&(x, y)| Point::new(x, y)).collect();
    (DistanceMatrix::euclidean(&points), loads(&SQUARE_STOPS, 0))
}

/// The length of the shortest closed tour, found by trying every order of the stops.
pub(crate) fn optimum(matrix: &DistanceMatrix) -> u64 {
    optimum_within(matrix, None)
}

/// The length of the shortest closed tour that the loads allow, found by trying every
/// order of the stops.
pub(crate) fn optimum_within(matrix: &DistanceMatrix, loads: Option<&Loads>) -> u64 {
    if matrix.len() < 2 {
        return 0;
    }
    let mut left: Vec<usize> = (1..matrix.len()).collect();
    let mut order = vec![0];
    let mut best = u64::MAX;
    extend(matrix, loads, &mut order, &mut left, 0, &mut best);
    best
}

/// Tries every way to finish the tour that has called at `order` and driven `so_far`
/// metres.
fn extend(
    matrix: &DistanceMatrix,
    loads: Option<&Loads>,
    order: &mut Vec<usize>,
    left: &mut Vec<usize>,
    so_far: u64,
    best: &mut u64,
) {
    // Distances are not negative, so a tour that is already too long cannot recover.
    // Nor can one that has overloaded its courier: what is on board is asked of the
    // tour so far as a whole, with nothing that this search and the one under test
    // could share but `Loads::fits`.
    if so_far >= *best || loads.is_some_and(|loads| !loads.fits(order)) {
        return;
    }
    let at = order[order.len() - 1];
    if left.is_empty() {
        *best = (*best).min(so_far + u64::from(matrix.get(at, 0)));
        return;
    }
    for slot in 0..left.len() {
        let next = left.swap_remove(slot);
        order.push(next);
        extend(
            matrix,
            loads,
            order,
            left,
            so_far + u64::from(matrix.get(at, next)),
            best,
        );
        order.pop();
        left.push(next);
        let last = left.len() - 1;
        left.swap(slot, last);
    }
}

/// A shorter tour one 2-opt reversal away from `order`, if there is one that the
/// loads allow.
///
/// Like [`shorter_by_shift`] it builds every neighbour and measures it from scratch, so
/// it shares no code with the moves under test.
pub(crate) fn shorter_by_reversal(
    matrix: &DistanceMatrix,
    loads: Option<&Loads>,
    order: &[usize],
) -> Option<Vec<usize>> {
    let cost = cycle_cost(matrix, order);
    for first in 1..order.len() {
        for last in first + 1..order.len() {
            let mut neighbour = order.to_vec();
            neighbour[first..=last].reverse();
            if cycle_cost(matrix, &neighbour) < cost
                && loads.is_none_or(|loads| loads.fits(&neighbour))
            {
                return Some(neighbour);
            }
        }
    }
    None
}

/// A shorter tour one Or-opt move away from `order`, if there is one that the loads
/// allow: a run of one to three stops taken out and put back anywhere else, either
/// way round.
pub(crate) fn shorter_by_shift(
    matrix: &DistanceMatrix,
    loads: Option<&Loads>,
    order: &[usize],
) -> Option<Vec<usize>> {
    let cost = cycle_cost(matrix, order);
    for len in 1..=3 {
        for start in 1..(order.len() + 1).saturating_sub(len) {
            let mut rest = order.to_vec();
            let mut run: Vec<usize> = rest.drain(start..start + len).collect();
            for _way_round in 0..2 {
                for slot in 1..=rest.len() {
                    let mut neighbour = rest.clone();
                    neighbour.splice(slot..slot, run.iter().copied());
                    if cycle_cost(matrix, &neighbour) < cost
                        && loads.is_none_or(|loads| loads.fits(&neighbour))
                    {
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
        assert!(shorter_by_reversal(&matrix, None, &[0, 2, 1, 3, 4]).is_some());
        assert!(shorter_by_shift(&matrix, None, &[0, 2, 1, 3, 4]).is_some());
        assert_eq!(shorter_by_reversal(&matrix, None, &[0, 1, 2, 3, 4]), None);
        assert_eq!(shorter_by_shift(&matrix, None, &[0, 1, 2, 3, 4]), None);
    }

    #[test]
    fn the_optimum_within_loads_leaves_out_the_tours_that_overload() {
        let (matrix, loads) = square_with_the_delivery_opposite();
        // Round the square, either way: 40 metres. Both ways begin with a pickup.
        assert_eq!(optimum(&matrix), 40);
        // The delivery first, along a diagonal, and back along the other.
        assert_eq!(optimum_within(&matrix, Some(&loads)), 48);
        // On a courier with room for one parcel more, one pickup can come first.
        let roomier = Loads::new(3, &SQUARE_STOPS).unwrap();
        assert_eq!(optimum_within(&matrix, Some(&roomier)), 40);
    }

    #[test]
    fn the_checkers_pass_over_a_neighbour_that_overloads() {
        let (matrix, loads) = square_with_the_delivery_opposite();
        // The delivery first. Round the square would be shorter, and overloads.
        let order = [0, 2, 1, 3];
        assert!(loads.fits(&order));
        assert!(shorter_by_reversal(&matrix, None, &order).is_some());
        assert!(shorter_by_shift(&matrix, None, &order).is_some());
        assert_eq!(shorter_by_reversal(&matrix, Some(&loads), &order), None);
        assert_eq!(shorter_by_shift(&matrix, Some(&loads), &order), None);

        // Stops on a line at 10, 20 and 30 metres: a pickup, then two deliveries, for
        // a courier that leaves full. Calling at the pickup between the deliveries
        // doubles back, and calling at it last, on the way home, does not.
        let at = [0u32, 10, 20, 30];
        let line = DistanceMatrix::from_fn(4, |from, to| at[from].abs_diff(at[to]));
        let stops = [Demand::pickup(2), Demand::delivery(2), Demand::delivery(2)];
        let loads = super::loads(&stops, 0);
        let doubling_back = [0, 2, 1, 3];
        assert!(loads.fits(&doubling_back));
        assert_eq!(
            shorter_by_reversal(&line, Some(&loads), &doubling_back),
            Some(vec![0, 2, 3, 1])
        );
    }

    #[test]
    fn demands_are_made_of_as_many_pairs_as_there_are_stops() {
        let numbers = [(0, 5), (2, 4), (3, 1), (1, 6)];
        assert_eq!(
            demands(4, &numbers),
            [
                Demand::delivery(5),
                Demand::pickup(4),
                Demand {
                    delivery: 3,
                    pickup: 1
                }
            ]
        );
        assert!(demands(1, &numbers).is_empty());
        assert!(demands(0, &numbers).is_empty());
    }
}
