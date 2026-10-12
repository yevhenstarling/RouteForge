//! Prints how good a route is against how many moves its search was given, and how
//! long those moves take on this machine: for three TSPLIB instances whose shortest
//! tour is known, and for a seeded random day.
//!
//! ```text
//! cargo run -p routeforge-tsp --release --example quality -- [seeds] [largest budget]
//! ```
//!
//! The largest budget is in millions of moves, 100 unless it is given, and the lengths
//! are means over 8 seeds unless that is. With the defaults it runs for some minutes:
//! `-- 2 10` is done in seconds.
//!
//! A search is given a budget of moves and no deadline, so the lengths are the same
//! on every machine. The times are this machine's.

mod day;
#[path = "../tests/data/mod.rs"]
mod tsplib;

use std::iter;
use std::time::{Duration, Instant};

use routeforge_tsp::{
    Control, DistanceMatrix, cluster, local_search, nearest_neighbour, optimize_route,
};

use crate::day::{SIDE, argument, grouped, matrix_of, random_day};

/// The smallest budget a search is given, in moves. Each one after it is ten times
/// the one before.
const SMALLEST_BUDGET: u64 = 100_000;

/// The seed of the random days. The seeds of the searches count up from nought.
const DAY_SEED: u64 = 42;

/// The days that are planned: how many stops, on how many couriers.
const DAYS: [(usize, usize); 2] = [(300, 4), (1200, 4)];

/// What one way of searching gave for a set of routes.
struct Row {
    search: String,
    /// The length of all the routes together, as a mean over the seeds.
    length: f64,
    /// How long one route took, as a mean over the routes and the seeds.
    a_route_took: Duration,
}

fn main() -> Result<(), String> {
    let seeds = argument(1, "seeds", 8)?.max(1);
    let largest = argument(2, "largest budget", 100)?.saturating_mul(1_000_000);
    let budgets: Vec<u64> =
        iter::successors(Some(SMALLEST_BUDGET), |budget| budget.checked_mul(10))
            .take_while(|&budget| budget <= largest.max(SMALLEST_BUDGET))
            .collect();
    println!(
        "Length against the moves a search was given: means over {seeds} seeds. A budget \
         counts every move, those of local search included.\n"
    );

    for instance in &tsplib::INSTANCES {
        let matrix = tsplib::matrix(instance);
        println!(
            "{}: {} stops, shortest tour {} (published)",
            instance.name,
            matrix.len() - 1,
            grouped(instance.optimum)
        );
        let (rows, _) = measured(&[matrix], &budgets, seeds);
        print(&rows, instance.optimum as f64);
    }

    for (stops, couriers) in DAYS {
        let problem = random_day(stops, couriers, DAY_SEED);
        let clustering = cluster(&problem);
        let routes: Vec<DistanceMatrix> = clustering
            .clusters
            .iter()
            .map(|cluster| matrix_of(&problem, &cluster.stops))
            .collect();
        let sizes: Vec<String> = routes
            .iter()
            .map(|route| (route.len() - 1).to_string())
            .collect();
        let (rows, shortest) = measured(&routes, &budgets, seeds);
        println!(
            "a day of {stops} stops in a {} km square: routes of {} stops, {} m together at \
             the shortest any search found",
            SIDE / 1_000.0,
            sizes.join(", "),
            grouped(shortest)
        );
        print(&rows, shortest as f64);
    }
    Ok(())
}

/// Searches for every route in every way: built and no more, local search to its end,
/// and the whole search under each budget with each seed. Gives a row for each way,
/// and the length of the routes together when each is taken at the shortest any of
/// the searches found for it.
fn measured(routes: &[DistanceMatrix], budgets: &[u64], seeds: u64) -> (Vec<Row>, u64) {
    let mut shortest = vec![u64::MAX; routes.len()];
    let mut rows = Vec::new();
    let runs = u32::try_from(routes.len()).unwrap_or(u32::MAX).max(1);

    let (mut built, mut settled) = (0, 0);
    let (mut building, mut settling) = (Duration::ZERO, Duration::ZERO);
    for matrix in routes {
        let started = Instant::now();
        let mut tour = nearest_neighbour(matrix);
        building += started.elapsed();
        built += tour.cost();

        let started = Instant::now();
        // Local search ends on its own, so it needs no limit.
        local_search(matrix, &mut tour, &mut Control::max_iterations(u64::MAX));
        settling += started.elapsed();
        settled += tour.cost();
    }
    rows.push(Row {
        search: "nearest neighbour".to_owned(),
        length: built as f64,
        a_route_took: building / runs,
    });
    rows.push(Row {
        search: "2-opt + Or-opt, to the end".to_owned(),
        length: settled as f64,
        a_route_took: settling / runs,
    });

    for &budget in budgets {
        let (mut total, mut took) = (0, Duration::ZERO);
        for seed in 0..seeds {
            for (matrix, shortest) in routes.iter().zip(&mut shortest) {
                let started = Instant::now();
                let tour = optimize_route(matrix, seed, &mut Control::max_iterations(budget));
                took += started.elapsed();
                total += tour.cost();
                *shortest = (*shortest).min(tour.cost());
            }
        }
        let searches = runs.saturating_mul(u32::try_from(seeds).unwrap_or(u32::MAX));
        rows.push(Row {
            search: format!("{} moves", grouped(budget)),
            length: total as f64 / seeds as f64,
            a_route_took: took / searches,
        });
    }
    (rows, shortest.iter().sum())
}

/// Prints the rows, each with how far its length is above `shortest`.
fn print(rows: &[Row], shortest: f64) {
    println!(
        "  {:<28}{:>14}{:>10}{:>16}",
        "search", "length", "above", "a route took"
    );
    for row in rows {
        println!(
            "  {:<28}{:>14.1}{:>8.2} %{:>13.1} ms",
            row.search,
            row.length,
            100.0 * (row.length / shortest - 1.0),
            row.a_route_took.as_secs_f64() * 1_000.0
        );
    }
    println!();
}
