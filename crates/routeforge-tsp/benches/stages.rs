//! Benchmarks of every stage at 50, 200 and 1000 stops, on seeded random instances.
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
    Control, DistanceMatrix, Point, Problem, Stop, anneal, cluster, local_search,
    nearest_neighbour, or_opt, two_opt,
};

const SIZES: [usize; 3] = [50, 200, 1000];
const SEED: u64 = 42;

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
        let stops_of_the_day: Vec<Stop> = points.map(|point| Stop { point, demand: 2 }).collect();
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

criterion_group!(
    stages,
    bench_nearest_neighbour,
    bench_two_opt,
    bench_or_opt,
    bench_anneal,
    bench_cluster
);
criterion_main!(stages);
