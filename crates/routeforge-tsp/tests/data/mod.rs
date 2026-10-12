//! TSPLIB instances whose shortest tour is known: for the quality test, and for the
//! example that prints quality against moves.
//!
//! The files beside this one are unchanged copies from TSPLIB95 by Gerhard Reinelt,
//! <https://comopt.ifi.uni-heidelberg.de/software/TSPLIB95/>.

use routeforge_tsp::{DistanceMatrix, Point};

pub struct Instance {
    pub name: &'static str,
    pub problem: &'static str,
    /// The length of the shortest tour, as TSPLIB lists it.
    pub optimum: u64,
}

pub const INSTANCES: [Instance; 3] = [
    Instance {
        name: "eil51",
        problem: include_str!("eil51.tsp"),
        optimum: 426,
    },
    Instance {
        name: "berlin52",
        problem: include_str!("berlin52.tsp"),
        optimum: 7_542,
    },
    Instance {
        name: "kroA100",
        problem: include_str!("kroA100.tsp"),
        optimum: 21_282,
    },
];

/// The lines of one section of a TSPLIB file, up to the `-1` or `EOF` that ends it.
pub fn section<'a>(file: &'a str, name: &str) -> impl Iterator<Item = &'a str> {
    file.lines()
        .map(str::trim)
        .skip_while(move |&line| line != name)
        .skip(1)
        .take_while(|&line| line != "-1" && line != "EOF")
}

/// The matrix of a problem with `EDGE_WEIGHT_TYPE: EUC_2D`. City 1 becomes node 0, the depot.
pub fn matrix(instance: &Instance) -> DistanceMatrix {
    let cities: Vec<Point> = section(instance.problem, "NODE_COORD_SECTION")
        .map(|line| {
            let mut numbers = line.split_whitespace().skip(1).map(|number| {
                number
                    .parse::<f64>()
                    .unwrap_or_else(|_| panic!("{}: `{line}` is not a city", instance.name))
            });
            match (numbers.next(), numbers.next()) {
                (Some(x), Some(y)) => Point::new(x, y),
                _ => panic!("{}: `{line}` is not a city", instance.name),
            }
        })
        .collect();
    DistanceMatrix::euclidean(&cities)
}
