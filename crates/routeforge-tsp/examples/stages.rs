//! Plans a seeded random day and prints the total route length after every stage.
//!
//! ```text
//! cargo run -p routeforge-tsp --release --example stages -- [stops] [couriers] [seed]
//! ```

mod day;

use std::time::{Duration, Instant};

use routeforge_tsp::{Control, anneal, cluster, local_search, nearest_neighbour, two_opt};

use crate::day::{SIDE, argument, grouped, matrix_of, random_day};

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
        let matrix = matrix_of(&problem, &cluster.stops);
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
