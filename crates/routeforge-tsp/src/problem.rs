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

/// One stop: where it is and how much of a courier's capacity it takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stop {
    pub point: Point,
    pub demand: u32,
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
