//! Sweep clustering.

use std::cmp::Reverse;

use crate::problem::{Cluster, Clustering, Problem};

/// The most start positions round the depot that are tried.
const MOST_STARTS: usize = 36;

/// Splits the stops into one cluster per courier, by capacity (D7, D16).
///
/// The stops are put in order of their direction from the depot. A sweep walks round
/// that circle and fills one courier after another, the largest first, so every
/// cluster is a sector as seen from the depot. A stop the sweep had to pass over goes
/// into the cluster with room for it where it adds the least distance.
///
/// Several start positions are tried. The result is the one that leaves the fewest
/// stops unassigned and, among those, the shortest routes when each is driven in sweep
/// order.
///
/// A stop is unassigned only if it fits into no courier's remaining capacity. Every
/// stop ends up in exactly one cluster or in `unassigned`, whatever the input.
pub fn cluster(problem: &Problem) -> Clustering {
    let largest = problem.capacities.iter().copied().max();
    let fits_a_courier =
        |&stop: &usize| largest.is_some_and(|capacity| problem.stops[stop].demand <= capacity);
    let (mut ring, mut unassigned): (Vec<usize>, Vec<usize>) =
        (0..problem.stops.len()).partition(fits_a_courier);

    let directions: Vec<(f64, f64)> = problem
        .stops
        .iter()
        .map(|stop| {
            let (dx, dy) = (
                stop.point.x - problem.depot.x,
                stop.point.y - problem.depot.y,
            );
            (dy.atan2(dx), dx * dx + dy * dy)
        })
        .collect();
    ring.sort_by(|&a, &b| {
        let ((angle_a, radius_a), (angle_b, radius_b)) = (directions[a], directions[b]);
        angle_a
            .total_cmp(&angle_b)
            .then(radius_a.total_cmp(&radius_b))
            .then(a.cmp(&b))
    });

    let mut couriers: Vec<usize> = (0..problem.capacities.len()).collect();
    couriers.sort_by_key(|&courier| (Reverse(problem.capacities[courier]), courier));

    let starts = ring.len().min(MOST_STARTS);
    let best = (0..starts)
        .map(|nth| Candidate::sweep(problem, &ring, &couriers, nth * ring.len() / starts))
        .min_by(|a, b| {
            a.left_over
                .len()
                .cmp(&b.left_over.len())
                .then(a.length.total_cmp(&b.length))
        });

    let mut clusters = Vec::new();
    if let Some(best) = best {
        unassigned.extend(best.left_over);
        clusters.extend(best.routes.into_iter().map(|route| Cluster {
            courier: route.courier,
            stops: route.stops,
        }));
    }
    unassigned.sort_unstable();
    Clustering {
        clusters,
        unassigned,
    }
}

/// The stops of one courier while a clustering is being built.
struct Route {
    courier: usize,
    /// Capacity still free.
    room: u32,
    stops: Vec<usize>,
}

/// One complete clustering, from one start position.
struct Candidate {
    routes: Vec<Route>,
    left_over: Vec<usize>,
    /// Metres on the plane when every route is driven in the order of its stops.
    length: f64,
}

impl Candidate {
    /// Sweeps round `ring` from position `start`. `couriers` is sorted, largest first.
    fn sweep(problem: &Problem, ring: &[usize], couriers: &[usize], start: usize) -> Self {
        let mut routes: Vec<Route> = Vec::new();
        let mut passed_over = Vec::new();
        for &stop in ring[start..].iter().chain(&ring[..start]) {
            let demand = problem.stops[stop].demand;
            if let Some(route) = routes.last_mut()
                && route.room >= demand
            {
                route.room -= demand;
                route.stops.push(stop);
                continue;
            }
            // The couriers get no larger, so a stop too big for the next one is too big
            // for all that follow. The current courier stays open for smaller stops.
            match couriers
                .get(routes.len())
                .map(|&courier| (courier, problem.capacities[courier]))
            {
                Some((courier, capacity)) if capacity >= demand => {
                    routes.push(Route {
                        courier,
                        room: capacity - demand,
                        stops: vec![stop],
                    });
                }
                _ => passed_over.push(stop),
            }
        }

        // The largest first: it has the fewest places to go.
        passed_over.sort_by_key(|&stop| (Reverse(problem.stops[stop].demand), stop));
        let mut left_over = Vec::new();
        for stop in passed_over {
            let demand = problem.stops[stop].demand;
            let cheapest = routes
                .iter_mut()
                .filter(|route| route.room >= demand)
                .map(|route| (cheapest_insertion(problem, &route.stops, stop), route))
                .min_by(|((_, a), _), ((_, b), _)| a.total_cmp(b));
            match cheapest {
                Some(((position, _), route)) => {
                    route.room -= demand;
                    route.stops.insert(position, stop);
                }
                None => left_over.push(stop),
            }
        }

        let length = routes
            .iter()
            .map(|route| route_length(problem, &route.stops))
            .sum();
        Self {
            routes,
            left_over,
            length,
        }
    }
}

/// Where in `route` the stop adds the least distance, and how much it adds.
fn cheapest_insertion(problem: &Problem, route: &[usize], stop: usize) -> (usize, f64) {
    let point = problem.stops[stop].point;
    let at = |position: Option<usize>| {
        position
            .and_then(|position| route.get(position))
            .map_or(problem.depot, |&stop| problem.stops[stop].point)
    };
    (0..=route.len())
        .map(|position| {
            let (before, after) = (at(position.checked_sub(1)), at(Some(position)));
            (
                position,
                before.distance(point) + point.distance(after) - before.distance(after),
            )
        })
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
        .unwrap_or((0, f64::INFINITY))
}

/// The length of the closed route from the depot through `stops` in the given order.
fn route_length(problem: &Problem, stops: &[usize]) -> f64 {
    let mut at = problem.depot;
    let mut length = 0.0;
    for &stop in stops {
        let next = problem.stops[stop].point;
        length += at.distance(next);
        at = next;
    }
    length + at.distance(problem.depot)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::problem::{Point, Stop};

    fn problem(coordinate: impl Strategy<Value = f64> + Clone) -> impl Strategy<Value = Problem> {
        let point = (coordinate.clone(), coordinate).prop_map(|(x, y)| Point::new(x, y));
        let stop = (point.clone(), 0..=12u32).prop_map(|(point, demand)| Stop { point, demand });
        let stops = prop::collection::vec(stop, 0..40);
        let capacities = prop::collection::vec(0..=30u32, 0..6);
        (point, stops, capacities).prop_map(|(depot, stops, capacities)| Problem {
            depot,
            stops,
            capacities,
        })
    }

    /// The promises of [`cluster`] that must hold for every input.
    fn assert_sound(problem: &Problem) -> Result<(), TestCaseError> {
        let clustering = cluster(problem);

        let mut seen: Vec<usize> = clustering.unassigned.clone();
        let mut room = problem.capacities.clone();
        let mut used = vec![false; problem.capacities.len()];
        for cluster in &clustering.clusters {
            prop_assert!(
                !cluster.stops.is_empty(),
                "courier {} has an empty cluster",
                cluster.courier
            );
            prop_assert!(
                !used[cluster.courier],
                "courier {} has two clusters",
                cluster.courier
            );
            used[cluster.courier] = true;
            let load: u64 = cluster
                .stops
                .iter()
                .map(|&stop| u64::from(problem.stops[stop].demand))
                .sum();
            prop_assert!(
                load <= u64::from(problem.capacities[cluster.courier]),
                "courier {} carries {load}",
                cluster.courier
            );
            room[cluster.courier] -= load as u32;
            seen.extend(&cluster.stops);
        }

        seen.sort_unstable();
        prop_assert!(
            seen.into_iter().eq(0..problem.stops.len()),
            "not every stop exactly once"
        );
        prop_assert!(clustering.unassigned.is_sorted());
        for &stop in &clustering.unassigned {
            let demand = problem.stops[stop].demand;
            prop_assert!(
                room.iter().all(|&room| room < demand),
                "stop {stop} would still fit"
            );
        }
        prop_assert_eq!(cluster(problem), clustering);
        Ok(())
    }

    proptest! {
        #[test]
        fn every_clustering_is_sound(problem in problem(-5_000.0..5_000.0f64)) {
            assert_sound(&problem)?;
        }

        #[test]
        fn coordinates_that_are_not_numbers_lose_no_stop(problem in problem(prop::num::f64::ANY)) {
            assert_sound(&problem)?;
        }

        #[test]
        fn a_courier_big_enough_for_everything_takes_everything(
            mut problem in problem(-5_000.0..5_000.0f64),
        ) {
            problem.capacities.push(problem.stops.iter().map(|stop| stop.demand).sum());
            let clustering = cluster(&problem);
            prop_assert_eq!(clustering.unassigned, Vec::<usize>::new());
            prop_assert!(clustering.clusters.len() <= 1);
        }
    }

    fn stop(x: f64, y: f64, demand: u32) -> Stop {
        Stop {
            point: Point::new(x, y),
            demand,
        }
    }

    #[test]
    fn sectors_keep_neighbours_together() {
        // Two stops east of the depot and two west of it, for two couriers of two.
        let problem = Problem {
            depot: Point::new(0.0, 0.0),
            stops: vec![
                stop(100.0, 10.0, 1),
                stop(-100.0, 10.0, 1),
                stop(100.0, -10.0, 1),
                stop(-100.0, -10.0, 1),
            ],
            capacities: vec![2, 2],
        };
        let mut clusters: Vec<Vec<usize>> = cluster(&problem)
            .clusters
            .into_iter()
            .map(|cluster| cluster.stops)
            .collect();
        clusters.iter_mut().for_each(|stops| stops.sort_unstable());
        clusters.sort();
        assert_eq!(clusters, [[0, 2], [1, 3]]);
    }

    #[test]
    fn the_largest_courier_is_filled_first() {
        let problem = Problem {
            depot: Point::new(0.0, 0.0),
            stops: vec![stop(10.0, 0.0, 3), stop(0.0, 10.0, 3)],
            capacities: vec![2, 6, 4],
        };
        let clustering = cluster(&problem);
        assert_eq!(
            clustering.clusters,
            [Cluster {
                courier: 1,
                stops: vec![0, 1]
            }]
        );
        assert_eq!(clustering.unassigned, Vec::<usize>::new());
    }

    #[test]
    fn a_stop_too_big_for_every_courier_is_unassigned() {
        let problem = Problem {
            depot: Point::new(0.0, 0.0),
            stops: vec![stop(10.0, 0.0, 9), stop(0.0, 10.0, 2)],
            capacities: vec![5],
        };
        let clustering = cluster(&problem);
        assert_eq!(
            clustering.clusters,
            [Cluster {
                courier: 0,
                stops: vec![1]
            }]
        );
        assert_eq!(clustering.unassigned, [0]);
    }

    #[test]
    fn a_stop_passed_over_goes_where_there_is_room() {
        // Swept in this order the demands are 4, 7, 3, 6. The 7 opens the second
        // courier, the 3 joins it, and the 6 finds no courier left to open. The first
        // courier still has room for it.
        let problem = Problem {
            depot: Point::new(0.0, 0.0),
            stops: vec![
                stop(10.0, 1.0, 4),
                stop(1.0, 10.0, 7),
                stop(-10.0, 1.0, 3),
                stop(-1.0, -10.0, 6),
            ],
            capacities: vec![10, 10],
        };
        let candidate = Candidate::sweep(&problem, &[0, 1, 2, 3], &[0, 1], 0);
        assert!(candidate.left_over.is_empty());
        let stops: Vec<&[usize]> = candidate
            .routes
            .iter()
            .map(|route| &route.stops[..])
            .collect();
        // Before or after the only stop of a route costs the same; the first place wins.
        assert_eq!(stops, [&[3, 0][..], &[1, 2][..]]);
        assert!(candidate.routes.iter().all(|route| route.room == 0));
    }

    #[test]
    fn a_stop_passed_over_stays_out_when_nothing_has_room() {
        let problem = Problem {
            depot: Point::new(0.0, 0.0),
            stops: vec![stop(10.0, 1.0, 6), stop(1.0, 10.0, 6), stop(-10.0, 1.0, 6)],
            capacities: vec![10, 10],
        };
        let candidate = Candidate::sweep(&problem, &[0, 1, 2], &[0, 1], 0);
        assert_eq!(candidate.left_over, [2]);
        assert_eq!(cluster(&problem).unassigned.len(), 1);
    }

    #[test]
    fn without_couriers_or_stops_nothing_is_assigned() {
        let depot = Point::new(0.0, 0.0);
        let no_couriers = Problem {
            depot,
            stops: vec![stop(1.0, 1.0, 0)],
            capacities: Vec::new(),
        };
        assert_eq!(
            cluster(&no_couriers),
            Clustering {
                clusters: Vec::new(),
                unassigned: vec![0]
            }
        );
        let no_stops = Problem {
            depot,
            stops: Vec::new(),
            capacities: vec![5],
        };
        assert_eq!(cluster(&no_stops), Clustering::default());
    }
}
