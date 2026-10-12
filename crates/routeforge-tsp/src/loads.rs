//! What a courier has on board along a route.

use std::fmt;

use crate::matrix::DistanceMatrix;
use crate::moves::Move;
use crate::problem::Demand;

/// What one courier carries along one route, and how much it can carry (D65).
///
/// The courier leaves the depot with everything it is to deliver. Calling at a stop
/// takes that stop's delivery off and puts its pickup on, so what is on board at any
/// moment depends on the order of the stops. A tour fits when the load is within the
/// capacity all along it.
///
/// A value of this type is of a route that has such a tour. [`Loads::new`] checks that
/// the deliveries fit the courier together, and that the pickups do. Then the tour
/// that makes every delivery before the first pickup fits, so a search always has a
/// tour to start from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loads {
    capacity: i64,
    /// What is on board on leaving the depot: everything that is to be delivered.
    start: i64,
    /// What calling at each node adds to the load: a pickup adds, a delivery takes
    /// away. Node 0, the depot, changes nothing.
    change: Vec<i64>,
    /// Whether some order of the stops would overload the courier. Where the
    /// deliveries and the pickups fit it even together, none does, and a search has
    /// nothing to look out for.
    binding: bool,
}

/// Why no order of a route's stops is one its courier could drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The deliveries are more than the courier can carry out of the depot.
    Deliveries { load: u64, capacity: u32 },
    /// The pickups are more than the courier can carry back to it.
    Pickups { load: u64, capacity: u32 },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Deliveries { load, capacity } => write!(
                f,
                "the deliveries of the route come to {load}, and its courier carries {capacity}"
            ),
            Self::Pickups { load, capacity } => write!(
                f,
                "the pickups of the route come to {load}, and its courier carries {capacity}"
            ),
        }
    }
}

impl std::error::Error for LoadError {}

impl Loads {
    /// The loads of a route whose courier carries `capacity`. `stops[n]` is what stop
    /// `n` asks for, which is node `n + 1` of the route's matrix: node 0 is the depot.
    ///
    /// # Errors
    ///
    /// If the deliveries together are more than the capacity, or the pickups are.
    /// No order of the stops could be driven then.
    pub fn new(capacity: u32, stops: &[Demand]) -> Result<Self, LoadError> {
        let out: u64 = stops.iter().map(|stop| u64::from(stop.delivery)).sum();
        let back: u64 = stops.iter().map(|stop| u64::from(stop.pickup)).sum();
        if out > u64::from(capacity) {
            return Err(LoadError::Deliveries {
                load: out,
                capacity,
            });
        }
        if back > u64::from(capacity) {
            return Err(LoadError::Pickups {
                load: back,
                capacity,
            });
        }
        let depot = [Demand::default()];
        let change = depot
            .iter()
            .chain(stops)
            .map(|stop| i64::from(stop.pickup) - i64::from(stop.delivery))
            .collect();
        Ok(Self {
            capacity: i64::from(capacity),
            // Both are within the capacity, which is a `u32`.
            start: out as i64,
            change,
            binding: out + back > u64::from(capacity),
        })
    }

    /// The loads of a route of `stops` stops on which nothing is carried. Every order
    /// of its stops fits.
    pub fn none(stops: usize) -> Self {
        Self {
            capacity: 0,
            start: 0,
            change: vec![0; stops + 1],
            binding: false,
        }
    }

    /// Whether the courier is within its capacity all along `nodes`, called at in that
    /// order. The depot may be among them, as it is first in a tour's order, and
    /// changes nothing.
    ///
    /// The nodes are taken for all the stops of the route: what is on board at the
    /// start is every delivery there is.
    ///
    /// # Panics
    ///
    /// If a node is none of the route's, like indexing a slice.
    pub fn fits(&self, nodes: &[usize]) -> bool {
        let mut on_board = self.start;
        nodes.iter().all(|&node| {
            on_board += self.change[node];
            on_board <= self.capacity
        })
    }

    /// The most the courier has on board at any moment along `nodes`, called at in
    /// that order.
    ///
    /// # Panics
    ///
    /// If a node is none of the route's, like indexing a slice.
    pub fn peak(&self, nodes: &[usize]) -> u64 {
        let mut on_board = self.start;
        let mut peak = on_board;
        for &node in nodes {
            on_board += self.change[node];
            peak = peak.max(on_board);
        }
        // What is on board is never less than nothing.
        peak.unsigned_abs()
    }

    /// The loads as a search has to mind them: `None` if no order of the stops could
    /// overload the courier. The search then runs as it does without loads, move for
    /// move.
    pub(crate) fn limit(&self) -> Option<&Self> {
        self.binding.then_some(self)
    }

    /// What is on board on leaving the depot.
    pub(crate) fn start(&self) -> i64 {
        self.start
    }

    /// What is on board after calling at `node` with `on_board`. `None` if that is
    /// more than the courier carries.
    pub(crate) fn after(&self, on_board: i64, node: usize) -> Option<i64> {
        Some(on_board + self.change[node]).filter(|&load| load <= self.capacity)
    }

    /// Whether the tour still fits once `change` is made to `order`. `scratch` is room
    /// to try it in, which the caller keeps so that nothing is allocated here.
    ///
    /// It walks the whole tour. A search asks only about a move it would make, which
    /// is a small share of the moves it looks at.
    pub(crate) fn allows(&self, order: &[usize], change: Move, scratch: &mut Vec<usize>) -> bool {
        scratch.clear();
        scratch.extend_from_slice(order);
        change.apply(scratch);
        self.fits(scratch)
    }

    /// Every stage starts here, as it does with the tour: loads only make sense on the
    /// matrix of their route.
    ///
    /// # Panics
    ///
    /// If the loads are of a route with another number of stops.
    pub(crate) fn check(&self, matrix: &DistanceMatrix) {
        // A matrix without a node is of a route without stops, as one of the depot
        // alone is.
        assert_eq!(
            self.change.len(),
            matrix.len().max(1),
            "the loads are of a route with another number of stops"
        );
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::testing;

    fn delivery(parcels: u32) -> Demand {
        Demand::delivery(parcels)
    }

    fn pickup(parcels: u32) -> Demand {
        Demand::pickup(parcels)
    }

    #[test]
    fn the_load_falls_with_a_delivery_and_rises_with_a_pickup() {
        // Leaves with 5. Stops 1 and 2 are deliveries of 3 and 2, stop 3 a pickup of 4.
        let loads = Loads::new(5, &[delivery(3), delivery(2), pickup(4)]).unwrap();
        assert_eq!(loads.start(), 5);

        // 5, then 2, 0, 4.
        assert!(loads.fits(&[0, 1, 2, 3]));
        assert_eq!(loads.peak(&[0, 1, 2, 3]), 5);
        // 5, then 2, 6: the pickup comes before there is room for it.
        assert!(!loads.fits(&[0, 1, 3, 2]));
        assert_eq!(loads.peak(&[0, 1, 3, 2]), 6);
        // 5, then 9 at once.
        assert!(!loads.fits(&[0, 3, 1, 2]));
        assert_eq!(loads.peak(&[0, 3, 1, 2]), 9);
        // The depot may be left out: it changes nothing.
        assert!(loads.fits(&[2, 1, 3]));
        assert!(!loads.fits(&[3, 2, 1]));
    }

    #[test]
    fn a_stop_can_be_brought_something_and_have_something_taken() {
        let both = Demand {
            delivery: 2,
            pickup: 3,
        };
        let loads = Loads::new(4, &[both, delivery(2)]).unwrap();
        // Leaves with 4. At the first stop 2 come off and 3 come on: 5.
        assert!(!loads.fits(&[0, 1, 2]));
        // The other way round it is 4, 2, 3.
        assert!(loads.fits(&[0, 2, 1]));
        assert_eq!(loads.peak(&[0, 2, 1]), 4);
    }

    #[test]
    fn deliveries_or_pickups_beyond_the_capacity_are_refused() {
        let error = Loads::new(4, &[delivery(3), delivery(2), pickup(4)]).unwrap_err();
        assert_eq!(
            error,
            LoadError::Deliveries {
                load: 5,
                capacity: 4
            }
        );
        assert!(error.to_string().contains("deliveries"), "{error}");

        let error = Loads::new(4, &[delivery(4), pickup(3), pickup(2)]).unwrap_err();
        assert_eq!(
            error,
            LoadError::Pickups {
                load: 5,
                capacity: 4
            }
        );
        assert!(error.to_string().contains("pickups"), "{error}");

        // As much as the courier carries, each way, is not too much.
        Loads::new(4, &[delivery(4), pickup(4)]).unwrap();
        // Nor is a load that would not fit a `u32` when it is added up.
        let error = Loads::new(u32::MAX, &[delivery(u32::MAX), delivery(1)]).unwrap_err();
        assert_eq!(
            error,
            LoadError::Deliveries {
                load: u64::from(u32::MAX) + 1,
                capacity: u32::MAX
            }
        );
    }

    #[test]
    fn loads_that_fit_together_are_nothing_to_mind() {
        // 3 out and 2 back: 5 in all.
        let stops = [delivery(3), pickup(2)];
        assert!(Loads::new(5, &stops).unwrap().limit().is_none());
        assert!(Loads::new(4, &stops).unwrap().limit().is_some());
        assert!(Loads::new(3, &stops).unwrap().limit().is_some());

        let none = Loads::none(3);
        assert!(none.limit().is_none());
        assert!(none.fits(&[0, 3, 1, 2]));
        assert_eq!(none.peak(&[0, 3, 1, 2]), 0);
        assert!(Loads::none(0).fits(&[0]));
    }

    #[test]
    #[should_panic(expected = "another number of stops")]
    fn check_refuses_a_matrix_of_another_size() {
        let matrix = DistanceMatrix::from_fn(4, |_, _| 1);
        Loads::none(2).check(&matrix);
    }

    #[test]
    fn loads_of_no_stops_go_with_a_matrix_of_the_depot_or_of_nothing() {
        let loads = Loads::new(3, &[]).unwrap();
        loads.check(&DistanceMatrix::from_fn(1, |_, _| 0));
        loads.check(&DistanceMatrix::from_fn(0, |_, _| 0));
        assert!(loads.fits(&[]));
        assert!(loads.limit().is_none());
    }

    proptest! {
        /// Reckoned here step by step, apart from the two functions under test.
        #[test]
        fn a_tour_fits_exactly_when_its_peak_is_within_the_capacity(
            (matrix, order) in testing::matrix_with_tour(0..=12),
            parcels in prop::collection::vec((0..=6u32, 0..=6u32), 12),
            slack in 0..=8u32,
        ) {
            let stops = testing::demands(matrix.len(), &parcels);
            let out: u32 = stops.iter().map(|stop| stop.delivery).sum();
            let back: u32 = stops.iter().map(|stop| stop.pickup).sum();
            let capacity = out.max(back) + slack;
            let loads = Loads::new(capacity, &stops).unwrap();

            let mut on_board = u64::from(out);
            let mut most = on_board;
            for &node in order.iter().skip(1) {
                let stop = stops[node - 1];
                on_board = on_board + u64::from(stop.pickup) - u64::from(stop.delivery);
                most = most.max(on_board);
            }
            prop_assert_eq!(loads.peak(&order), most);
            prop_assert_eq!(loads.fits(&order), most <= u64::from(capacity));
            // Where everything fits together, every order does.
            if out + back <= capacity {
                prop_assert!(loads.fits(&order));
                prop_assert!(loads.limit().is_none());
            }
        }

        /// Every delivery before the first pickup: the tour that makes a route one
        /// that can be driven.
        #[test]
        fn deliveries_first_always_fits(
            parcels in prop::collection::vec((0..=6u32, 0..=6u32), 0..12),
        ) {
            let stops = testing::demands(parcels.len() + 1, &parcels);
            let out: u32 = stops.iter().map(|stop| stop.delivery).sum();
            let back: u32 = stops.iter().map(|stop| stop.pickup).sum();
            let loads = Loads::new(out.max(back), &stops).unwrap();

            // The stops that take more off than they put on come first.
            let mut order: Vec<usize> = (1..=stops.len()).collect();
            order.sort_by_key(|&node| stops[node - 1].pickup > stops[node - 1].delivery);
            prop_assert!(loads.fits(&order));
        }
    }
}
