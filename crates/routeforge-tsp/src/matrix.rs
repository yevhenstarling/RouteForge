//! The distance matrix the route search works on.

use std::fmt;

use crate::problem::Point;

/// Distances between the nodes of one route, in whole metres (D4).
///
/// Node 0 is the depot and the other nodes are stops. The matrix is symmetric: the
/// search reverses parts of a tour, and only a symmetric matrix keeps the length of a
/// reversed part unchanged (D14).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DistanceMatrix {
    nodes: usize,
    /// Row-major, `nodes * nodes` entries.
    metres: Vec<u32>,
}

/// Why a table of distances was not accepted as a [`DistanceMatrix`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatrixError {
    /// The table does not have `nodes * nodes` entries.
    WrongLength { nodes: usize, len: usize },
    /// The distance from `from` to `to` differs from the distance back.
    Asymmetric { from: usize, to: usize },
}

impl fmt::Display for MatrixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::WrongLength { nodes, len } => {
                write!(
                    f,
                    "a matrix of {nodes} nodes needs {nodes}x{nodes} distances, got {len}"
                )
            }
            Self::Asymmetric { from, to } => {
                write!(
                    f,
                    "the distance from node {from} to node {to} differs from the distance back"
                )
            }
        }
    }
}

impl std::error::Error for MatrixError {}

impl DistanceMatrix {
    /// Wraps a row-major table of `nodes * nodes` distances.
    pub fn new(nodes: usize, metres: Vec<u32>) -> Result<Self, MatrixError> {
        if nodes.checked_mul(nodes) != Some(metres.len()) {
            return Err(MatrixError::WrongLength {
                nodes,
                len: metres.len(),
            });
        }
        let matrix = Self { nodes, metres };
        for from in 0..nodes {
            for to in from + 1..nodes {
                if matrix.get(from, to) != matrix.get(to, from) {
                    return Err(MatrixError::Asymmetric { from, to });
                }
            }
        }
        Ok(matrix)
    }

    /// Builds the matrix from a distance function.
    ///
    /// The function is asked once per pair of nodes, with `from < to` and in row order,
    /// and its answer is used for both directions.
    pub fn from_fn(nodes: usize, mut distance: impl FnMut(usize, usize) -> u32) -> Self {
        let mut metres = vec![0; nodes * nodes];
        for from in 0..nodes {
            for to in from + 1..nodes {
                let between = distance(from, to);
                metres[from * nodes + to] = between;
                metres[to * nodes + from] = between;
            }
        }
        Self { nodes, metres }
    }

    /// Straight-line distances between points on a plane, rounded to the nearest metre.
    /// The first point is the depot.
    pub fn euclidean(points: &[Point]) -> Self {
        // A float-to-integer `as` saturates, so a distance beyond `u32::MAX` is capped.
        Self::from_fn(points.len(), |from, to| {
            points[from].distance(points[to]).round() as u32
        })
    }

    /// The number of nodes, the depot included.
    pub fn len(&self) -> usize {
        self.nodes
    }

    pub fn is_empty(&self) -> bool {
        self.nodes == 0
    }

    /// The distance between two nodes in metres.
    ///
    /// # Panics
    ///
    /// If a node is not below [`len`](Self::len), like indexing a slice.
    #[inline]
    pub fn get(&self, from: usize, to: usize) -> u32 {
        assert!(
            to < self.nodes,
            "node {to} is outside a matrix of {} nodes",
            self.nodes
        );
        self.metres[from * self.nodes + to]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_accepts_a_symmetric_table() {
        let matrix = DistanceMatrix::new(3, vec![0, 5, 7, 5, 0, 2, 7, 2, 0]).unwrap();
        assert_eq!(matrix.len(), 3);
        assert_eq!(matrix.get(0, 2), 7);
        assert_eq!(matrix.get(2, 1), 2);
    }

    #[test]
    fn new_rejects_a_table_of_the_wrong_length() {
        let error = DistanceMatrix::new(3, vec![0; 8]).unwrap_err();
        assert_eq!(error, MatrixError::WrongLength { nodes: 3, len: 8 });
    }

    #[test]
    fn new_rejects_a_node_count_whose_square_overflows() {
        let error = DistanceMatrix::new(usize::MAX, Vec::new()).unwrap_err();
        assert_eq!(
            error,
            MatrixError::WrongLength {
                nodes: usize::MAX,
                len: 0
            }
        );
    }

    #[test]
    fn new_rejects_an_asymmetric_table() {
        let error = DistanceMatrix::new(3, vec![0, 5, 7, 5, 0, 2, 7, 3, 0]).unwrap_err();
        assert_eq!(error, MatrixError::Asymmetric { from: 1, to: 2 });
    }

    #[test]
    fn an_empty_matrix_is_valid() {
        assert!(DistanceMatrix::new(0, Vec::new()).unwrap().is_empty());
    }

    #[test]
    fn from_fn_asks_once_per_pair_and_mirrors_the_answer() {
        let mut asked = Vec::new();
        let matrix = DistanceMatrix::from_fn(3, |from, to| {
            asked.push((from, to));
            (10 * from + to) as u32
        });
        assert_eq!(asked, [(0, 1), (0, 2), (1, 2)]);
        assert_eq!(
            matrix,
            DistanceMatrix::new(3, vec![0, 1, 2, 1, 0, 12, 2, 12, 0]).unwrap()
        );
    }

    #[test]
    fn euclidean_rounds_to_the_nearest_metre() {
        let points = [
            Point::new(0.0, 0.0),
            Point::new(3.0, 4.0),
            Point::new(1.0, 1.0),
        ];
        let matrix = DistanceMatrix::euclidean(&points);
        assert_eq!(matrix.get(0, 1), 5);
        // The square root of 2 is 1.41 and rounds down; the square root of 13 is 3.61 and rounds up.
        assert_eq!(matrix.get(0, 2), 1);
        assert_eq!(matrix.get(1, 2), 4);
    }

    #[test]
    #[should_panic(expected = "outside a matrix")]
    fn get_refuses_a_node_outside_the_matrix() {
        DistanceMatrix::from_fn(2, |_, _| 1).get(0, 2);
    }
}
