//! Plans a seeded random day and prints the total route length after every stage.
//!
//! ```text
//! cargo run -p routeforge-tsp --release --example stages -- [stops] [couriers] [seed]
//! ```

use std::env;
use std::iter;
use std::time::{Duration, Instant};

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use routeforge_tsp::{
    Control, DistanceMatrix, Point, Problem, Stop, anneal, cluster, local_search,
    nearest_neighbour, two_opt,
};

/// The side of the square the stops are scattered over, in metres.
const SIDE: f64 = 10_000.0;

/// How many moves annealing gets for each route.
const ANNEALING_MOVES: u64 = 2_000_000;

const STAGES: [&str; 4] = [
    "nearest neighbour",
    "2-opt",
    "2-opt + Or-opt",
    "simulated annealing",
];

fn main() -> Result<(), String> {
    let stops = argument(1, "stops", 300)? as usize;
    let couriers = argument(2, "couriers", 6)? as usize;
    let seed = argument(3, "seed", 42)?;

    let problem = random_day(stops, couriers, seed);
    let capacity = problem.capacities.first().copied().unwrap_or(0);
    println!(
        "{stops} stops in a {} km square, {couriers} couriers of capacity {capacity}, seed {seed}",
        SIDE / 1_000.0
    );

    let clustering = cluster(&problem);
    println!(
        "sweep clustering: {} routes, {} stops unassigned\n",
        clustering.clusters.len(),
        clustering.unassigned.len()
    );

    let mut totals = [0u64; STAGES.len()];
    let mut times = [Duration::ZERO; STAGES.len()];
    for (route, cluster) in clustering.clusters.iter().enumerate() {
        let stops_of_the_route = cluster.stops.iter().map(|&stop| problem.stops[stop].point);
        let points: Vec<Point> = iter::once(problem.depot)
            .chain(stops_of_the_route)
            .collect();
        let matrix = DistanceMatrix::euclidean(&points);
        // Local search ends on its own, so it needs no limit.
        let mut unlimited = Control::max_iterations(u64::MAX);
        let mut annealing = Control::max_iterations(ANNEALING_MOVES);
        let mut started = Instant::now();
        let mut record = |stage: usize, cost: u64| {
            totals[stage] += cost;
            times[stage] += started.elapsed();
            started = Instant::now();
        };

        let mut tour = nearest_neighbour(&matrix);
        record(0, tour.cost());
        two_opt(&matrix, &mut tour, &mut unlimited);
        record(1, tour.cost());
        local_search(&matrix, &mut tour, &mut unlimited);
        record(2, tour.cost());
        anneal(
            &matrix,
            &mut tour,
            seed.wrapping_add(route as u64),
            &mut annealing,
        );
        record(3, tour.cost());
    }

    println!(
        "{:<22}{:>14}{:>10}{:>12}",
        "after", "total, m", "change", "stage took"
    );
    let mut previous = None;
    for ((stage, total), time) in STAGES.iter().zip(totals).zip(times) {
        let change = match previous {
            Some(previous) if previous > 0 => {
                format!(
                    "{:+.1} %",
                    100.0 * (total as f64 - previous as f64) / previous as f64
                )
            }
            _ => String::new(),
        };
        println!(
            "{stage:<22}{:>14}{change:>10}{:>9.1} ms",
            grouped(total),
            time.as_secs_f64() * 1_000.0
        );
        previous = Some(total);
    }
    println!(
        "\nannealing tried {} moves on each route",
        grouped(ANNEALING_MOVES)
    );
    Ok(())
}

/// The whole number at `position` on the command line, or `default` if it is missing.
fn argument(position: usize, name: &str, default: u64) -> Result<u64, String> {
    match env::args().nth(position) {
        None => Ok(default),
        Some(text) => text
            .parse()
            .map_err(|_| format!("{name} must be a whole number, got `{text}`")),
    }
}

/// A depot in the middle of the square, stops scattered over it with a demand of one to
/// three, and couriers that together have a tenth more capacity than the day needs.
fn random_day(stops: usize, couriers: usize, seed: u64) -> Problem {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let stops: Vec<Stop> = (0..stops)
        .map(|_| Stop {
            point: Point::new(rng.random_range(0.0..SIDE), rng.random_range(0.0..SIDE)),
            demand: rng.random_range(1..=3),
        })
        .collect();
    let demand: u64 = stops.iter().map(|stop| u64::from(stop.demand)).sum();
    let capacity = (demand * 11).div_ceil(10 * couriers.max(1) as u64);
    Problem {
        depot: Point::new(SIDE / 2.0, SIDE / 2.0),
        stops,
        capacities: vec![u32::try_from(capacity).unwrap_or(u32::MAX); couriers],
    }
}

/// `1234567` as `1 234 567`.
fn grouped(number: u64) -> String {
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
