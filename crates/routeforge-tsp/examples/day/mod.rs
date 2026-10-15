//! What the examples share: a seeded random day, and how they read their arguments
//! and write their numbers.

use std::env;
use std::iter;

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use routeforge_tsp::{Demand, DistanceMatrix, Point, Problem, Stop};

/// The side of the square the stops are scattered over, in metres.
pub const SIDE: f64 = 10_000.0;

/// The whole number at `position` on the command line, or `default` if it is missing.
pub fn argument(position: usize, name: &str, default: u64) -> Result<u64, String> {
    match env::args().nth(position) {
        None => Ok(default),
        Some(text) => text
            .parse()
            .map_err(|_| format!("{name} must be a whole number, got `{text}`")),
    }
}

/// A depot in the middle of the square, stops scattered over it with one to three
/// parcels to deliver to each, and couriers that together have a tenth more capacity
/// than the day needs.
///
/// Nothing is picked up, so every order of a route's stops is one its courier can
/// drive, and a search has no loads to mind.
pub fn random_day(stops: usize, couriers: usize, seed: u64) -> Problem {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let stops: Vec<Stop> = (0..stops)
        .map(|_| Stop {
            point: Point::new(rng.random_range(0.0..SIDE), rng.random_range(0.0..SIDE)),
            demand: Demand::delivery(rng.random_range(1..=3)),
        })
        .collect();
    let demand: u64 = stops
        .iter()
        .map(|stop| u64::from(stop.demand.delivery))
        .sum();
    let capacity = (demand * 11).div_ceil(10 * couriers.max(1) as u64);
    Problem {
        depot: Point::new(SIDE / 2.0, SIDE / 2.0),
        stops,
        capacities: vec![u32::try_from(capacity).unwrap_or(u32::MAX); couriers],
    }
}

/// The distances a search for one route works on: between the depot, which is node 0,
/// and these stops of the day.
pub fn matrix_of(problem: &Problem, stops: &[usize]) -> DistanceMatrix {
    let points: Vec<Point> = iter::once(problem.depot)
        .chain(stops.iter().map(|&stop| problem.stops[stop].point))
        .collect();
    DistanceMatrix::euclidean(&points)
}

/// `1234567` as `1 234 567`.
pub fn grouped(number: u64) -> String {
    let digits = number.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(' ');
        }
        grouped.push(digit);
    }
    grouped
}
