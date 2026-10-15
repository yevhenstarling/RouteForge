//! Benchmarks of every stage at 50, 200 and 1000 stops, on seeded random instances, and
//! of the whole search with and without loads to mind.
//!
//! ```text
//! cargo bench -p routeforge-tsp
//! ```

use std::hint::black_box;
use std::time::Duration;

use criterion::measurement::WallTime;
use criterion::{
    BatchSize, BenchmarkGroup, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main,
};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use routeforge_tsp::{
    Control, Demand, DistanceMatrix, Loads, Point, Problem, Stop, anneal, cluster, local_search,
    nearest_neighbour, optimize_route, optimize_route_within, or_opt, two_opt,
};

const SIZES: [usize; 3] = [50, 200, 1000];
const SEED: u64 = 42;

/// How many moves a whole search gets when what the loads cost is measured.
const SEARCH_MOVES: u64 = 10_000_000;

/// The side of the square the stops are scattered over, in metres.
const SIDE: f64 = 10_000.0;

/// How many moves one annealing measurement tries. Fewer than one epoch at every size,
/// so it times the annealing moves alone.
const ANNEALING_MOVES: u64 = 100_000;

/// A depot and `stops` stops, the same on every run.
fn scattered(stops: usize) -> Vec<Point> {
    let mut rng = ChaCha8Rng::seed_from_u64(SEED);
    let mut coordinate = || rng.random_range(0.0..SIDE);
    (0..=stops)
        .map(|_| Point::new(coordinate(), coordinate()))
        .collect()
}

/// Local search ends on its own, so it needs no limit.
fn unlimited() -> Control<'static> {
    Control::max_iterations(u64::MAX)
}

fn configured<'a>(criterion: &'a mut Criterion, name: &str) -> BenchmarkGroup<'a, WallTime> {
    let mut group = criterion.benchmark_group(name);
    group
        .sample_size(20)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(4));
    group
}

fn bench_nearest_neighbour(criterion: &mut Criterion) {
    let mut group = configured(criterion, "nearest_neighbour");
    for stops in SIZES {
        let matrix = DistanceMatrix::euclidean(&scattered(stops));
        group.bench_function(BenchmarkId::from_parameter(stops), |bencher| {
            bencher.iter(|| nearest_neighbour(black_box(&matrix)));
        });
    }
    group.finish();
}

fn bench_two_opt(criterion: &mut Criterion) {
    let mut group = configured(criterion, "two_opt");
    for stops in SIZES {
        let matrix = DistanceMatrix::euclidean(&scattered(stops));
        let constructed = nearest_neighbour(&matrix);
        group.bench_function(BenchmarkId::from_parameter(stops), |bencher| {
            bencher.iter_batched(
                || constructed.clone(),
                |mut tour| {
                    two_opt(&matrix, &mut tour, &mut unlimited());
                    tour
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_or_opt(criterion: &mut Criterion) {
    let mut group = configured(criterion, "or_opt");
    // At 1000 stops a run takes about a third of a second.
    group.sample_size(10);
    for stops in SIZES {
        let matrix = DistanceMatrix::euclidean(&scattered(stops));
        let mut uncrossed = nearest_neighbour(&matrix);
        two_opt(&matrix, &mut uncrossed, &mut unlimited());
        group.bench_function(BenchmarkId::from_parameter(stops), |bencher| {
            bencher.iter_batched(
                || uncrossed.clone(),
                |mut tour| {
                    or_opt(&matrix, &mut tour, &mut unlimited());
                    tour
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_anneal(criterion: &mut Criterion) {
    let mut group = configured(criterion, "anneal");
    group.throughput(Throughput::Elements(ANNEALING_MOVES));
    for stops in SIZES {
        let matrix = DistanceMatrix::euclidean(&scattered(stops));
        let mut settled = nearest_neighbour(&matrix);
        local_search(&matrix, &mut settled, &mut unlimited());
        group.bench_function(BenchmarkId::from_parameter(stops), |bencher| {
            bencher.iter_batched(
                || settled.clone(),
                |mut tour| {
                    anneal(
                        &matrix,
                        &mut tour,
                        SEED,
                        &mut Control::max_iterations(ANNEALING_MOVES),
                    );
                    tour
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_cluster(criterion: &mut Criterion) {
    let mut group = configured(criterion, "cluster");
    for stops in SIZES {
        let mut points = scattered(stops).into_iter();
        let depot = points.next().unwrap_or(Point::new(0.0, 0.0));
        let stops_of_the_day: Vec<Stop> = points
            .map(|point| Stop {
                point,
                demand: Demand::delivery(2),
            })
            .collect();
        // Couriers of 25 stops each, and one to spare.
        let problem = Problem {
            depot,
            stops: stops_of_the_day,
            capacities: vec![50; stops / 25 + 1],
        };
        group.bench_function(BenchmarkId::from_parameter(stops), |bencher| {
            bencher.iter(|| cluster(black_box(&problem)));
        });
    }
    group.finish();
}

/// What one courier's day asks for, as a demonstration's does: one stop in five a
/// pickup and the others deliveries, of one to three parcels each, on a courier with a
/// tenth more room than its deliveries need. The deliveries and the pickups do not fit
/// it together, so a search has the loads to mind (D65).
fn loaded(stops: usize) -> Loads {
    let mut rng = ChaCha8Rng::seed_from_u64(SEED + 1);
    let asked: Vec<Demand> = (0..stops)
        .map(|_| {
            let parcels = rng.random_range(1..=3);
            if rng.random_range(0..5) == 0 {
                Demand::pickup(parcels)
            } else {
                Demand::delivery(parcels)
            }
        })
        .collect();
    let out: u32 = asked.iter().map(|stop| stop.delivery).sum();
    let back: u32 = asked.iter().map(|stop| stop.pickup).sum();
    Loads::new(out.max(back) + out / 10, &asked).expect("the courier carries either")
}

/// What minding the loads costs: the whole search for one route, with nothing on board
/// and for a courier that can be overloaded. The budget is enough at both sizes for
/// local search to end and for annealing to do most of the work.
fn bench_loads(criterion: &mut Criterion) {
    let mut group = configured(criterion, "optimize_route");
    group.sample_size(10);
    group.throughput(Throughput::Elements(SEARCH_MOVES));
    for stops in [50, 300] {
        let matrix = DistanceMatrix::euclidean(&scattered(stops));
        let loads = loaded(stops);
        group.bench_function(BenchmarkId::new("no_loads", stops), |bencher| {
            bencher.iter(|| {
                optimize_route(
                    black_box(&matrix),
                    SEED,
                    &mut Control::max_iterations(SEARCH_MOVES),
                )
            });
        });
        group.bench_function(BenchmarkId::new("within_loads", stops), |bencher| {
            bencher.iter(|| {
                optimize_route_within(
                    black_box(&matrix),
                    &loads,
                    SEED,
                    &mut Control::max_iterations(SEARCH_MOVES),
                )
            });
        });
    }
    group.finish();
}

criterion_group!(
    stages,
    bench_nearest_neighbour,
    bench_two_opt,
    bench_or_opt,
    bench_anneal,
    bench_cluster,
    bench_loads
);
criterion_main!(stages);
