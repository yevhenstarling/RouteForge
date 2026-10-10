//! Tour construction.

use crate::matrix::DistanceMatrix;
use crate::tour::Tour;

/// Builds a tour by always driving on to the closest stop not visited yet.
///
/// A tie goes to the lower node index, so the tour depends on the matrix alone.
pub fn nearest_neighbour(matrix: &DistanceMatrix) -> Tour {
    let nodes = matrix.len();
    let mut visited = vec![false; nodes];
    let mut order = Vec::with_capacity(nodes);
    let mut next = (nodes > 0).then_some(0);
    while let Some(current) = next {
        visited[current] = true;
        order.push(current);
        next = (0..nodes)
            .filter(|&node| !visited[node])
            .min_by_key(|&node| matrix.get(current, node));
    }
    Tour::new(matrix, order)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
