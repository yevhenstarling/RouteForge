//! A closed tour and its length.

use crate::matrix::DistanceMatrix;
use crate::moves::Move;

/// A closed tour: from the depot through every stop once and back to the depot.
///
/// A tour belongs to the matrix it was built on. It comes out of
/// [`nearest_neighbour`](crate::nearest_neighbour) or
/// [`optimize_route`](crate::optimize_route).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tour {
    /// The nodes in visiting order. The depot, node 0, is always first.
    pub(crate) order: Vec<usize>,
    /// Kept equal to the length of `order` on the tour's matrix.
    pub(crate) cost: u64,
}

impl Tour {
    pub(crate) fn new(matrix: &DistanceMatrix, order: Vec<usize>) -> Self {
        let cost = cycle_cost(matrix, &order);
        Self { order, cost }
    }

    /// The stops in visiting order, as node indexes of the matrix. The depot is left out.
    pub fn stops(&self) -> &[usize] {
        self.order.get(1..).unwrap_or_default()
    }

    /// The length of the closed tour in metres.
    pub fn cost(&self) -> u64 {
        self.cost
    }

    /// Every stage starts here: a tour only makes sense on the matrix it was built on.
    ///
    /// # Panics
    ///
    /// If the tour was built on a matrix of another size.
    pub(crate) fn check(&self, matrix: &DistanceMatrix) {
        assert_eq!(
            self.order.len(),
            matrix.len(),
            "the tour was built on a matrix of another size"
        );
        debug_assert_eq!(
            self.cost,
            cycle_cost(matrix, &self.order),
            "the tour's cost has drifted"
        );
    }

    /// Makes a move. `delta` is what the move's own [`delta`](Move::delta) reported, so
    /// the order and the cost change together and stay in step.
    #[inline]
    pub(crate) fn apply(&mut self, change: Move, delta: i64) {
        change.apply(&mut self.order);
        self.cost = self.cost.wrapping_add_signed(delta);
    }

    /// Becomes a copy of `other` without allocating.
    pub(crate) fn copy_from(&mut self, other: &Tour) {
        self.order.clone_from(&other.order);
        self.cost = other.cost;
    }
}

/// The length of the cycle that visits `order` and returns to its first node.
pub(crate) fn cycle_cost(matrix: &DistanceMatrix, order: &[usize]) -> u64 {
    // A depot on its own, or nothing at all, is no trip.
    let &[first, .., last] = order else {
        return 0;
    };
    let legs = order
        .windows(2)
        .map(|leg| (leg[0], leg[1]))
        .chain([(last, first)]);
    legs.map(|(from, to)| u64::from(matrix.get(from, to))).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_counts_the_way_back_to_the_depot() {
        let matrix = DistanceMatrix::new(3, vec![0, 5, 7, 5, 0, 2, 7, 2, 0]).unwrap();
        let tour = Tour::new(&matrix, vec![0, 1, 2]);
        assert_eq!(tour.cost(), 5 + 2 + 7);
        assert_eq!(tour.stops(), [1, 2]);
    }

    #[test]
    fn a_lone_stop_is_a_round_trip() {
        let matrix = DistanceMatrix::from_fn(2, |_, _| 40);
        assert_eq!(Tour::new(&matrix, vec![0, 1]).cost(), 80);
    }

    #[test]
    fn tours_without_stops_have_no_length() {
        let depot_only = DistanceMatrix::new(1, vec![9]).unwrap();
        let tour = Tour::new(&depot_only, vec![0]);
        assert_eq!((tour.cost(), tour.stops()), (0, &[][..]));

        let empty = DistanceMatrix::from_fn(0, |_, _| 0);
        let tour = Tour::new(&empty, Vec::new());
        assert_eq!((tour.cost(), tour.stops()), (0, &[][..]));
    }

    #[test]
    fn the_length_of_a_long_tour_does_not_overflow() {
        let matrix = DistanceMatrix::from_fn(4, |_, _| u32::MAX);
        let tour = Tour::new(&matrix, vec![0, 1, 2, 3]);
        assert_eq!(tour.cost(), 4 * u64::from(u32::MAX));
    }

    #[test]
    #[should_panic(expected = "another size")]
    fn check_refuses_a_matrix_of_another_size() {
        let matrix = DistanceMatrix::from_fn(3, |_, _| 1);
        let other = DistanceMatrix::from_fn(4, |_, _| 1);
        Tour::new(&matrix, vec![0, 1, 2]).check(&other);
    }
}
