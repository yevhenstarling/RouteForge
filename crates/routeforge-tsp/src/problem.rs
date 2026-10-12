//! The clustering problem and its answer.

/// A point on the local plane, in metres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// The straight-line distance to `other` in metres.
    pub fn distance(self, other: Point) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
}

/// What a stop asks of the courier that calls at it (D65).
///
/// A delivery is on board from the depot to its stop, and a pickup from its stop back
/// to the depot. The two take up room at different times, so they are counted apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Demand {
    /// What the courier brings here from the depot.
    pub delivery: u32,
    /// What the courier takes from here back to the depot.
    pub pickup: u32,
}

impl Demand {
    /// So much to bring, and nothing to take away.
    pub const fn delivery(parcels: u32) -> Self {
        Self {
            delivery: parcels,
            pickup: 0,
        }
    }

    /// So much to take away, and nothing to bring.
    pub const fn pickup(parcels: u32) -> Self {
        Self {
            delivery: 0,
            pickup: parcels,
        }
    }

    /// The larger of the two: the least a courier has to carry to call here at all.
    pub(crate) fn larger(self) -> u32 {
        self.delivery.max(self.pickup)
    }
}

/// One stop: where it is and how much of a courier's capacity it takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stop {
    pub point: Point,
    pub demand: Demand,
}

/// A day's stops and the couriers available for them.
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub depot: Point,
    pub stops: Vec<Stop>,
    /// The capacity of each courier.
    pub capacities: Vec<u32>,
}

/// The stops of one courier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cluster {
    /// Index into [`Problem::capacities`].
    pub courier: usize,
    /// Indexes into [`Problem::stops`], roughly in sweep order. The order to drive them
    /// in is for [`optimize_route`](crate::optimize_route) to find.
    pub stops: Vec<usize>,
}

/// The answer of [`cluster`](crate::cluster).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Clustering {
    /// One entry per courier that got at least one stop.
    pub clusters: Vec<Cluster>,
    /// Stops that fit into no courier's remaining capacity, in ascending order.
    pub unassigned: Vec<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_demand_is_a_delivery_or_a_pickup() {
        assert_eq!(
            Demand::delivery(3),
            Demand {
                delivery: 3,
                pickup: 0
            }
        );
        assert_eq!(
            Demand::pickup(2),
            Demand {
                delivery: 0,
                pickup: 2
            }
        );
        assert_eq!(Demand::default(), Demand::delivery(0));
        assert_eq!(Demand::delivery(3).larger(), 3);
        assert_eq!(Demand::pickup(2).larger(), 2);
        let both = Demand {
            delivery: 1,
            pickup: 4,
        };
        assert_eq!(both.larger(), 4);
    }
}
