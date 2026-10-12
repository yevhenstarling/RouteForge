//! Tour construction.

use crate::loads::Loads;
use crate::matrix::DistanceMatrix;
use crate::tour::Tour;

/// Builds a tour by always driving on to the closest stop not visited yet.
///
/// A tie goes to the lower node index, so the tour depends on the matrix alone.
pub fn nearest_neighbour(matrix: &DistanceMatrix) -> Tour {
    nearest_neighbour_within(matrix, None)
}

/// Builds a tour by always driving on to the closest stop not visited yet that the
/// courier has room for (D65).
///
/// There always is one. A stop that takes more off than it puts on can be called at
/// with any load. When only the others are left, the load rises from stop to stop up
/// to what all the pickups come to, which a courier that has [`Loads`] can carry.
pub(crate) fn nearest_neighbour_within(matrix: &DistanceMatrix, loads: Option<&Loads>) -> Tour {
    let nodes = matrix.len();
    let mut visited = vec![false; nodes];
    let mut order = Vec::with_capacity(nodes);
    let mut on_board = loads.map_or(0, Loads::start);
    let mut next = (nodes > 0).then_some(0);
    while let Some(current) = next {
        visited[current] = true;
        order.push(current);
        // `after` has said that there is room, when this node was chosen.
        on_board = loads
            .and_then(|loads| loads.after(on_board, current))
            .unwrap_or(on_board);
        next = (0..nodes)
            .filter(|&node| !visited[node])
            .filter(|&node| loads.is_none_or(|loads| loads.after(on_board, node).is_some()))
            .min_by_key(|&node| matrix.get(current, node));
    }
    debug_assert_eq!(
        order.len(),
        nodes,
        "a stop was left that the courier had no room for"
    );
    Tour::new(matrix, order)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::testing;

    #[test]
    fn follows_the_closest_unvisited_stop() {
        // Stops on a line at 10, 30 and 20 metres from the depot.
        let at = [0u32, 10, 30, 20];
        let matrix = DistanceMatrix::from_fn(4, |from, to| at[from].abs_diff(at[to]));
        let tour = nearest_neighbour(&matrix);
        assert_eq!(tour.stops(), [1, 3, 2]);
        assert_eq!(tour.cost(), 60);
    }

    #[test]
    fn a_tie_goes_to_the_lower_index() {
        let matrix = DistanceMatrix::from_fn(4, |_, _| 7);
        assert_eq!(nearest_neighbour(&matrix).stops(), [1, 2, 3]);
    }

    #[test]
    fn handles_matrices_without_stops() {
        let empty = DistanceMatrix::from_fn(0, |_, _| 0);
        assert!(nearest_neighbour(&empty).stops().is_empty());
        let depot_only = DistanceMatrix::from_fn(1, |_, _| 0);
        assert!(nearest_neighbour(&depot_only).stops().is_empty());
    }

    #[test]
    fn passes_by_a_stop_the_courier_has_no_room_for_yet() {
        let (matrix, loads) = testing::square_with_the_delivery_opposite();
        // Without loads: round the square, the lower index first.
        assert_eq!(nearest_neighbour(&matrix).stops(), [1, 2, 3]);
        // The pickups are the closest, and neither can be first. After the delivery
        // they are equally far, and the lower index goes first.
        let tour = nearest_neighbour_within(&matrix, Some(&loads));
        assert_eq!(tour.stops(), [2, 1, 3]);
        assert_eq!(tour.cost(), 48);
    }

    proptest! {
        #[test]
        fn the_tour_is_one_the_courier_can_drive(
            matrix in testing::matrix(0..=12),
            numbers in prop::collection::vec((0..=6u32, 0..=6u32), 12),
            slack in 0..=4u32,
        ) {
            let stops = testing::demands(matrix.len(), &numbers);
            let loads = testing::loads(&stops, slack);
            let tour = nearest_neighbour_within(&matrix, Some(&loads));
            prop_assert!(testing::is_tour(&matrix, &tour.order));
            prop_assert!(loads.fits(&tour.order));
        }

        #[test]
        fn loads_that_cannot_be_exceeded_change_nothing(
            matrix in testing::matrix(0..=12),
            numbers in prop::collection::vec((0..=6u32, 0..=6u32), 12),
        ) {
            let stops = testing::demands(matrix.len(), &numbers);
            // Room for everything at once.
            let loads = testing::loads(&stops, 12 * 6);
            prop_assert_eq!(
                nearest_neighbour_within(&matrix, Some(&loads)),
                nearest_neighbour(&matrix)
            );
        }
    }
}
