//! Quality regression on TSPLIB instances whose optimal tour length is known.
//!
//! The instances, and where they are from, are in `data/`.

mod data;

use routeforge_tsp::{Control, optimize_route};

use crate::data::{INSTANCES, Instance, matrix, section};

const SEED: u64 = 2026;

/// Candidate moves each instance gets.
const BUDGET: u64 = 1_000_000;

/// How far above the optimum a route may end, in percent.
///
/// With this seed and budget all three come out optimal. Over other seeds the mean is
/// below half a percent, while local search alone ends 2.5 to 5 percent above on the
/// two larger instances. So the limit has room for another platform's `exp`, and still
/// fails if annealing stops contributing.
const TOLERANCE: u64 = 2;

/// The published optimal tour as nodes of the matrix.
fn optimal_tour(instance: &Instance) -> Vec<usize> {
    let published = match instance.name {
        "eil51" => include_str!("data/eil51.opt.tour"),
        "berlin52" => include_str!("data/berlin52.opt.tour"),
        "kroA100" => include_str!("data/kroA100.opt.tour"),
        other => panic!("{other} has no published tour here"),
    };
    section(published, "TOUR_SECTION")
        .map(|line| match line.parse::<usize>() {
            Ok(city) if city >= 1 => city - 1,
            _ => panic!("{}: `{line}` is not a city number", instance.name),
        })
        .collect()
}

/// Checks the data and the rounding of [`DistanceMatrix::euclidean`] in one go: only the
/// right coordinates, rounded the TSPLIB way, give the published length.
#[test]
fn the_published_tours_measure_the_published_optima() {
    for instance in &INSTANCES {
        let matrix = matrix(instance);
        let tour = optimal_tour(instance);
        assert_eq!(tour.len(), matrix.len(), "{}", instance.name);

        let way_back = (tour[tour.len() - 1], tour[0]);
        let legs = tour
            .windows(2)
            .map(|leg| (leg[0], leg[1]))
            .chain([way_back]);
        let length: u64 = legs.map(|(from, to)| u64::from(matrix.get(from, to))).sum();
        assert_eq!(length, instance.optimum, "{}", instance.name);
    }
}

#[test]
fn routes_end_close_to_the_optimum() {
    for instance in &INSTANCES {
        let matrix = matrix(instance);
        let mut control = Control::max_iterations(BUDGET);
        let tour = optimize_route(&matrix, SEED, &mut control);

        let above = 100.0 * (tour.cost() as f64 / instance.optimum as f64 - 1.0);
        println!(
            "{}: {} against an optimum of {} ({above:+.2} %)",
            instance.name,
            tour.cost(),
            instance.optimum
        );
        assert!(
            tour.cost() >= instance.optimum,
            "{}: shorter than the optimum",
            instance.name
        );
        assert!(
            tour.cost() * 100 <= instance.optimum * (100 + TOLERANCE),
            "{}: {} is more than {TOLERANCE} % above {}",
            instance.name,
            tour.cost(),
            instance.optimum
        );
    }
}
