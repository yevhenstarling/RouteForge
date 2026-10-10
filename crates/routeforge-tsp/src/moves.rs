//! The moves that turn one tour into a neighbouring one.
//!
//! Local search and annealing share these. A move is described by positions in the
//! tour's order; position 0 holds the depot and never moves. `delta` reads only the
//! edges that change, and the tests check it against a full recomputation.

use crate::matrix::DistanceMatrix;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Move {
    /// 2-opt: reverse the positions `first..=last`.
    ///
    /// Needs `1 <= first <= last`.
    Reverse { first: usize, last: usize },
    /// Or-opt: take the `len` nodes from position `start` out and put them back right
    /// after the node at position `anchor`, back to front if `reversed`.
    ///
    /// Needs `1 <= start`, `1 <= len`, and an anchor that is neither in the segment nor
    /// directly before it: `anchor + 1 < start` or `start + len <= anchor`.
    Shift {
        start: usize,
        len: usize,
        anchor: usize,
        reversed: bool,
    },
    /// Exchange the nodes at positions `first` and `second`.
    ///
    /// Needs `1 <= first < second`.
    Swap { first: usize, second: usize },
}

impl Move {
    /// The change in tour length the move would make, in metres.
    #[inline]
    pub(crate) fn delta(self, matrix: &DistanceMatrix, order: &[usize]) -> i64 {
        let edge = |from: usize, to: usize| i64::from(matrix.get(from, to));
        match self {
            Self::Reverse { first, last } => {
                let before = order[first - 1];
                let head = order[first];
                let tail = order[last];
                let after = following(order, last);
                edge(before, tail) + edge(head, after) - edge(before, head) - edge(tail, after)
            }
            Self::Shift {
                start,
                len,
                anchor,
                reversed,
            } => {
                let end = start + len - 1;
                let before = order[start - 1];
                let head = order[start];
                let tail = order[end];
                let after = following(order, end);
                let left = order[anchor];
                let right = following(order, anchor);
                let (entry, exit) = if reversed { (tail, head) } else { (head, tail) };
                let closing_the_gap = edge(before, after) - edge(before, head) - edge(tail, after);
                let opening_the_slot = edge(left, entry) + edge(exit, right) - edge(left, right);
                closing_the_gap + opening_the_slot
            }
            Self::Swap { first, second } => {
                let (a, b) = (order[first], order[second]);
                let before = order[first - 1];
                let after = following(order, second);
                if second == first + 1 {
                    return edge(before, b) + edge(a, after) - edge(before, a) - edge(b, after);
                }
                let after_a = order[first + 1];
                let before_b = order[second - 1];
                edge(before, b) + edge(b, after_a) + edge(before_b, a) + edge(a, after)
                    - edge(before, a)
                    - edge(a, after_a)
                    - edge(before_b, b)
                    - edge(b, after)
            }
        }
    }

    #[inline]
    pub(crate) fn apply(self, order: &mut [usize]) {
        match self {
            Self::Reverse { first, last } => order[first..=last].reverse(),
            Self::Shift {
                start,
                len,
                anchor,
                reversed,
            } => {
                let landed = if anchor < start {
                    // The nodes between the anchor and the segment slide right.
                    order[anchor + 1..start + len].rotate_right(len);
                    anchor + 1
                } else {
                    // The nodes between the segment and the anchor slide left.
                    order[start..=anchor].rotate_left(len);
                    anchor + 1 - len
                };
                if reversed {
                    order[landed..landed + len].reverse();
                }
            }
            Self::Swap { first, second } => order.swap(first, second),
        }
    }
}

/// The node after position `at`. After the last position the tour is back at the depot.
#[inline]
fn following(order: &[usize], at: usize) -> usize {
    order.get(at + 1).copied().unwrap_or(order[0])
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use proptest::sample::Index;

    use super::*;
    use crate::testing;
    use crate::tour::cycle_cost;

    /// A move must leave a tour behind, and report exactly the change it makes.
    fn assert_exact(
        matrix: &DistanceMatrix,
        order: &[usize],
        change: Move,
    ) -> Result<(), TestCaseError> {
        let mut moved = order.to_vec();
        change.apply(&mut moved);
        prop_assert!(
            testing::is_tour(matrix, &moved),
            "{change:?} left {moved:?}"
        );
        let recomputed =
            i128::from(cycle_cost(matrix, &moved)) - i128::from(cycle_cost(matrix, order));
        prop_assert_eq!(
            i128::from(change.delta(matrix, order)),
            recomputed,
            "{:?}",
            change
        );
        Ok(())
    }

    proptest! {
        #[test]
        fn a_reversal_reports_its_exact_change(
            (matrix, order) in testing::matrix_with_tour(2..=12),
            (a, b) in any::<(Index, Index)>(),
        ) {
            let stops = order.len() - 1;
            let (a, b) = (1 + a.index(stops), 1 + b.index(stops));
            assert_exact(&matrix, &order, Move::Reverse { first: a.min(b), last: a.max(b) })?;
        }

        #[test]
        fn a_shift_reports_its_exact_change(
            (matrix, order) in testing::matrix_with_tour(3..=12),
            (start, len, anchor) in any::<(Index, Index, Index)>(),
            reversed in any::<bool>(),
        ) {
            let nodes = order.len();
            let start = 1 + start.index(nodes - 1);
            // Leave at least one position, besides the one before the segment, to anchor to.
            let longest = (nodes - start).min(nodes - 2).min(3);
            let len = 1 + len.index(longest);
            let anchors: Vec<usize> = (0..start - 1).chain(start + len..nodes).collect();
            let anchor = *anchor.get(&anchors);
            assert_exact(&matrix, &order, Move::Shift { start, len, anchor, reversed })?;
        }

        #[test]
        fn a_swap_reports_its_exact_change(
            (matrix, order) in testing::matrix_with_tour(3..=12),
            (a, b) in any::<(Index, Index)>(),
        ) {
            let stops = order.len() - 1;
            let first = 1 + a.index(stops - 1);
            let second = first + 1 + b.index(stops - first);
            assert_exact(&matrix, &order, Move::Swap { first, second })?;
        }
    }

    #[test]
    fn a_shift_can_land_on_either_side_and_turn_round() {
        let shifted = |change: Move| {
            let mut order = vec![0, 1, 2, 3, 4, 5];
            change.apply(&mut order);
            order
        };
        let forward = Move::Shift {
            start: 1,
            len: 2,
            anchor: 4,
            reversed: false,
        };
        assert_eq!(shifted(forward), [0, 3, 4, 1, 2, 5]);
        let backward = Move::Shift {
            start: 4,
            len: 2,
            anchor: 0,
            reversed: false,
        };
        assert_eq!(shifted(backward), [0, 4, 5, 1, 2, 3]);
        let turned = Move::Shift {
            start: 2,
            len: 3,
            anchor: 5,
            reversed: true,
        };
        assert_eq!(shifted(turned), [0, 1, 5, 4, 3, 2]);
    }
}
