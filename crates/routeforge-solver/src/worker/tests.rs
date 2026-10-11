//! Workers against a real PostgreSQL, from a plan on the queue to routes in the
//! database.
//!
//! The workers of a test run inside it, on its pool. A test that needs a search to
//! be still running when something happens gives the plan a budget of time, far more
//! than the test takes, and holds [`one_at_a_time`]. Every other test budgets by
//! moves, so its searches take the same course on any machine.

use std::io;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use routeforge_domain::db::queue::{MAX_ATTEMPTS, PLANS_CHANNEL, PROGRESS_CHANNEL};
use routeforge_domain::db::{orders, plans, routes};
use routeforge_domain::{
    CourierId, Job, JobKind, JobStatus, Order, OrderId, OrderStatus, Plan, PlanCounts, PlanId,
    PlanStatus, RouteProgress,
};
use sqlx::postgres::PgListener;

use super::*;
use crate::testing::{
    self, Fleet, LONG, assert_routed_once, date, eventually, millis, moves, one_at_a_time,
};

/// A budget no test waits out. A search that is not stopped fails its test.
const ENDLESS: u32 = 120_000;

/// Waits until the plan is done. Says so at once if it has failed instead.
async fn done(pool: &PgPool, plan_id: PlanId) -> Plan {
    eventually("the plan to be done", async || {
        let plan = plans::get(pool, plan_id).await.unwrap().unwrap();
        assert_ne!(plan.status, PlanStatus::Failed, "{:?}", plan.error);
        (plan.status == PlanStatus::Done).then_some(plan)
    })
    .await
}

fn is_being_searched(job: &Job) -> bool {
    // Progress is stored by a heartbeat, so the search has been going for a while.
    job.payload.kind() == JobKind::Route
        && job.status == JobStatus::Running
        && job.progress.is_some()
}

/// Waits until a route job of the plan is in the middle of its search.
async fn a_route_being_searched(pool: &PgPool, plan_id: PlanId) -> Job {
    eventually("a route to be searched for", async || {
        let jobs = queue::list_for_plan(pool, plan_id).await.unwrap();
        jobs.into_iter().find(is_being_searched)
    })
    .await
}

#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn three_workers_route_every_order_once_within_capacity(pool: PgPool) {
    let city = testing::city(&pool, &[40, 40, 40, 40, 40]).await;
    let placed = city.mixed_orders(&pool, date(12), 60, 7).await;
    let too_large = city.order(&pool, date(12), 41).await;
    let tomorrow = city.orders(&pool, date(13), 3, 8).await;

    // Workers that would not look at the queue by themselves for most of a minute.
    let config = Config {
        poll: Duration::from_secs(60),
        ..testing::config()
    };
    let fleet = Fleet::with_workers(&pool, config, 3);
    eventually("three workers to register", async || {
        let seen = workers::list(&pool, LONG).await.unwrap();
        (seen.len() == 3).then_some(())
    })
    .await;
    // By now each has found the queue empty and is waiting.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Nothing but the notification can make them take this up in time.
    let new = city.new_plan(date(12), moves(100_000));
    let plan = plans::submit(&pool, &new, Some("req-e2e")).await.unwrap();
    let plan = done(&pool, plan.id).await;

    assert_eq!((plan.orders_total, plan.orders_unassigned), (61, 1));
    assert!(plan.routes_total >= 3, "{} routes", plan.routes_total);
    assert_routed_once(&pool, &city, plan.id, &placed).await;

    // What did not fit a courier, and what was for another day, is as it was.
    for order in tomorrow.iter().chain([&too_large]) {
        let stored = orders::get(&pool, order.id).await.unwrap().unwrap();
        assert_eq!(
            (stored.status, stored.plan_id),
            (OrderStatus::Pending, None)
        );
    }

    let jobs = queue::list_for_plan(&pool, plan.id).await.unwrap();
    assert_eq!(jobs.len(), 1 + plan.routes_total as usize);
    for job in jobs {
        assert_eq!((job.status, job.attempt), (JobStatus::Done, 1));
        assert_eq!(job.request_id.as_deref(), Some("req-e2e"));
    }
    fleet.stop().await;
}

/// The same orders, couriers, seed and budget of moves give the same routes, whichever
/// worker searches which (D5).
///
/// One route has a hundred stops and the budget ends in its second round of
/// annealing, so what the search has found by then depends on its seed. With routes
/// of a handful of stops any seed finds the same best tour, and the test would pass
/// whatever the seeds were.
#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn the_same_day_planned_twice_gives_the_same_routes(pool: PgPool) {
    let city = testing::city(&pool, &[100, 100]).await;
    let monday = city.orders(&pool, date(12), 130, 11).await;
    let tuesday = city.orders(&pool, date(13), 130, 11).await;

    let fleet = Fleet::with_workers(&pool, testing::config(), 3);
    let budget = moves(500_000);
    let first = plans::submit(&pool, &city.new_plan(date(12), budget), None)
        .await
        .unwrap();
    let second = plans::submit(&pool, &city.new_plan(date(13), budget), None)
        .await
        .unwrap();
    done(&pool, first.id).await;
    done(&pool, second.id).await;
    fleet.stop().await;

    // A route as its courier, its length and which orders of the day it calls at.
    let shape = async |plan_id: PlanId, placed: &[Order]| -> Vec<(CourierId, u64, Vec<usize>)> {
        let number = |id: OrderId| placed.iter().position(|order| order.id == id).unwrap();
        let stored = routes::list_for_plan(&pool, plan_id).await.unwrap();
        let mut shapes: Vec<_> = stored
            .into_iter()
            .map(|route| {
                let calls = route.stops.iter().map(|stop| number(stop.order_id));
                (route.courier_id, route.distance_m, calls.collect())
            })
            .collect();
        shapes.sort();
        shapes
    };
    let on_monday = shape(first.id, &monday).await;
    let stops: Vec<usize> = on_monday.iter().map(|(_, _, calls)| calls.len()).collect();
    assert_eq!(stops.iter().sum::<usize>(), 130);
    assert!(stops.contains(&100), "one courier is full: {stops:?}");
    assert_eq!(on_monday, shape(second.id, &tuesday).await);
}

/// What the heartbeat is for. The search takes more than twice as long as a lease
/// lasts, and all the while an idle worker is looking for something to take.
#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn a_route_that_outlasts_its_lease_is_not_taken_over(pool: PgPool) {
    let _turn = one_at_a_time().await;
    let city = testing::city(&pool, &[6]).await;
    let placed = city.orders(&pool, date(12), 6, 10).await;
    let config = Config {
        lease: Duration::from_secs(1),
        ..testing::config()
    };
    let fleet = Fleet::with_workers(&pool, config, 2);

    let new = city.new_plan(date(12), millis(2_500));
    let plan = plans::submit(&pool, &new, None).await.unwrap();
    done(&pool, plan.id).await;
    fleet.stop().await;

    let jobs = queue::list_for_plan(&pool, plan.id).await.unwrap();
    assert_eq!(jobs.len(), 2);
    let route = &jobs[1];
    assert_eq!(
        (route.status, route.attempt),
        (JobStatus::Done, 1),
        "the job was claimed again while its worker was at it"
    );
    // The last progress the heartbeat stored is that of a search well under way.
    assert!(
        route
            .progress
            .is_some_and(|progress| progress.iterations > 0)
    );
    assert_routed_once(&pool, &city, plan.id, &placed).await;
}

#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn a_plan_survives_a_worker_that_dies_in_the_middle_of_a_route(pool: PgPool) {
    let _turn = one_at_a_time().await;
    let city = testing::city(&pool, &[6, 6]).await;
    let placed = city.orders(&pool, date(12), 12, 2).await;
    // A route takes one second, and a lease outlasts its worker by three.
    let config = Config {
        lease: Duration::from_secs(3),
        ..testing::config()
    };
    let new = city.new_plan(date(12), millis(1_000));

    // One worker alone: it clusters the plan and starts on the first route.
    let mut fleet = Fleet::new(&pool, config);
    let doomed = fleet.start_worker();
    let plan = plans::submit(&pool, &new, None).await.unwrap();
    let held = a_route_being_searched(&pool, plan.id).await;
    assert_eq!((held.claimed_by, held.attempt), (Some(doomed), 1));

    fleet.kill(doomed).await;
    fleet.start_worker();
    fleet.start_worker();

    // The two finish the other route and find nothing more to do, two seconds before
    // the dead worker's lease runs out. Nobody is told when it does: an expired lease
    // sends no notification. They find out by looking, every poll interval.
    done(&pool, plan.id).await;
    let taken_over = queue::get(&pool, held.id).await.unwrap().unwrap();
    assert_eq!(
        (taken_over.status, taken_over.attempt),
        (JobStatus::Done, 2)
    );
    assert!(taken_over.claimed_by.is_some_and(|worker| worker != doomed));
    assert_routed_once(&pool, &city, plan.id, &placed).await;
    fleet.stop().await;
}

#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn cancelling_a_plan_stops_its_searches_within_a_heartbeat(pool: PgPool) {
    let _turn = one_at_a_time().await;
    let city = testing::city(&pool, &[6, 6, 6]).await;
    let placed = city.orders(&pool, date(12), 18, 3).await;
    let fleet = Fleet::with_workers(&pool, testing::config(), 3);

    let new = city.new_plan(date(12), millis(ENDLESS));
    let plan = plans::submit(&pool, &new, None).await.unwrap();
    eventually("three routes to be searched for", async || {
        let jobs = queue::list_for_plan(&pool, plan.id).await.unwrap();
        (jobs.iter().filter(|job| is_being_searched(job)).count() == 3).then_some(())
    })
    .await;

    assert!(queue::cancel_plan(&pool, plan.id).await.unwrap());
    let cancelled = Instant::now();

    // Every worker is in a search that has two minutes to go. Only if those stop
    // can the date be planned again, and that takes a quick plan no time.
    let new = city.new_plan(date(12), moves(20_000));
    let again = plans::submit(&pool, &new, None).await.unwrap();
    done(&pool, again.id).await;
    let took = cancelled.elapsed();
    assert!(
        took < Duration::from_secs(10),
        "the searches went on for {took:?}"
    );
    assert_routed_once(&pool, &city, again.id, &placed).await;

    // The cancelled plan left nothing behind.
    let plan = plans::get(&pool, plan.id).await.unwrap().unwrap();
    assert_eq!((plan.status, plan.routes_done), (PlanStatus::Cancelled, 0));
    assert!(
        routes::list_for_plan(&pool, plan.id)
            .await
            .unwrap()
            .is_empty()
    );
    let jobs = queue::list_for_plan(&pool, plan.id).await.unwrap();
    let statuses: Vec<JobStatus> = jobs.iter().map(|job| job.status).collect();
    assert_eq!(
        statuses,
        [
            JobStatus::Done,
            JobStatus::Cancelled,
            JobStatus::Cancelled,
            JobStatus::Cancelled
        ]
    );
    fleet.stop().await;
}

#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn a_job_that_another_worker_has_taken_is_given_up(pool: PgPool) {
    let _turn = one_at_a_time().await;
    let city = testing::city(&pool, &[6]).await;
    city.orders(&pool, date(12), 6, 4).await;
    let tomorrow = city.orders(&pool, date(13), 6, 5).await;
    let fleet = Fleet::with_workers(&pool, testing::config(), 1);

    let new = city.new_plan(date(12), millis(ENDLESS));
    let plan = plans::submit(&pool, &new, None).await.unwrap();
    let held = a_route_being_searched(&pool, plan.id).await;

    // Another worker claims the job, as one would after a lease that ran out while
    // this worker could not reach the database.
    let other = WorkerId::generate();
    sqlx::query(
        "UPDATE jobs
         SET attempt = attempt + 1, claimed_by = $2, lease_expires_at = now() + interval '1 hour'
         WHERE id = $1",
    )
    .bind(held.id.0)
    .bind(other.0)
    .execute(&pool)
    .await
    .unwrap();

    // The worker has two minutes of search ahead of it. That it takes up the next
    // day's plan shows it has let go.
    let new = city.new_plan(date(13), moves(20_000));
    let next = plans::submit(&pool, &new, None).await.unwrap();
    done(&pool, next.id).await;
    assert_routed_once(&pool, &city, next.id, &tomorrow).await;

    // It stored nothing of what it had found, and the job is the other worker's.
    assert!(
        routes::list_for_plan(&pool, plan.id)
            .await
            .unwrap()
            .is_empty()
    );
    let job = queue::get(&pool, held.id).await.unwrap().unwrap();
    assert_eq!(
        (job.status, job.attempt, job.claimed_by),
        (JobStatus::Running, 2, Some(other))
    );
    fleet.stop().await;
}

#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn a_worker_told_to_stop_gives_its_job_back(pool: PgPool) {
    let _turn = one_at_a_time().await;
    let city = testing::city(&pool, &[6]).await;
    city.orders(&pool, date(12), 6, 6).await;
    let fleet = Fleet::with_workers(&pool, testing::config(), 1);

    let new = city.new_plan(date(12), millis(ENDLESS));
    let plan = plans::submit(&pool, &new, None).await.unwrap();
    let held = a_route_being_searched(&pool, plan.id).await;
    assert_eq!(held.attempt, 1);

    let told = Instant::now();
    fleet.stop().await;
    let took = told.elapsed();
    assert!(took < Duration::from_secs(10), "stopping took {took:?}");

    // Due at once, for whoever is free, and the attempt is not held against the job.
    let given_back = queue::get(&pool, held.id).await.unwrap().unwrap();
    assert_eq!(
        (given_back.status, given_back.attempt),
        (JobStatus::Queued, 0)
    );
    assert_eq!(
        (given_back.claimed_by, given_back.lease_expires_at),
        (None, None)
    );
    let plan = plans::get(&pool, plan.id).await.unwrap().unwrap();
    assert_eq!(plan.status, PlanStatus::Routing);
    assert!(
        routes::list_for_plan(&pool, plan.id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn a_job_that_keeps_failing_fails_its_plan_with_the_reason(pool: PgPool) {
    let city = testing::city(&pool, &[6]).await;
    let plan = plans::create(&pool, &city.new_plan(date(12), moves(1_000)))
        .await
        .unwrap();
    // A route job as the `plan` job would leave it, but for an order that is not there.
    let missing = OrderId::generate();
    let route = JobPayload::Route {
        plan_id: plan.id,
        courier_id: city.couriers[0].id,
        order_ids: vec![missing],
        seed: 1,
    };
    let job_id = queue::enqueue(&pool, &route, None).await.unwrap();
    let counts = PlanCounts {
        orders_total: 1,
        orders_unassigned: 0,
        routes_total: 1,
    };
    plans::start_routing(&pool, plan.id, counts).await.unwrap();

    let fleet = Fleet::with_workers(&pool, testing::config(), 1);
    let failed = eventually("the plan to fail", async || {
        // A failed attempt is due again after two seconds, then four. The test does
        // not sit them out.
        sqlx::query("UPDATE jobs SET run_at = now() WHERE status = 'queued'")
            .execute(&pool)
            .await
            .unwrap();
        let plan = plans::get(&pool, plan.id).await.unwrap().unwrap();
        (plan.status == PlanStatus::Failed).then_some(plan)
    })
    .await;
    fleet.stop().await;

    let reason = failed.error.unwrap();
    assert!(reason.contains(&missing.to_string()), "{reason}");
    let job = queue::get(&pool, job_id).await.unwrap().unwrap();
    assert_eq!((job.status, job.attempt), (JobStatus::Failed, MAX_ATTEMPTS));
    assert_eq!(job.error.as_deref(), Some(reason.as_str()));
}

/// What the workers log, as the JSON lines a solver writes with `LOG_FORMAT=json`.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<u8>>>);

impl io::Write for Log {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Log {
    fn lines(&self) -> Vec<serde_json::Value> {
        let written = self.0.lock().unwrap();
        String::from_utf8_lossy(&written)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

/// Starts capturing what every test of this binary logs, and gives the capture.
///
/// The subscriber is the default of the whole process and not of one test's thread.
/// `tracing` remembers for every place that logs whether anybody listens, and with a
/// subscriber on one thread only it may ask a thread that has none. Beside tests on
/// other threads that loses lines, now and then.
fn captured_log() -> &'static Log {
    static LOG: OnceLock<Log> = OnceLock::new();
    LOG.get_or_init(|| {
        let log = Log::default();
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("no other test sets a subscriber");
        log
    })
}

/// Verification step 6 of the plan follows one request through the logs of every
/// service. This is the solver's part of it.
#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn what_a_job_logs_carries_the_ids_to_follow_it_by(pool: PgPool) {
    let log = captured_log();
    let city = testing::city(&pool, &[6]).await;
    city.orders(&pool, date(12), 5, 9).await;
    let mut fleet = Fleet::new(&pool, testing::config());
    let worker = fleet.start_worker();
    let new = city.new_plan(date(12), moves(20_000));
    let plan = plans::submit(&pool, &new, Some("req-log")).await.unwrap();
    done(&pool, plan.id).await;
    fleet.stop().await;

    // A line names the spans it was logged in: the worker's, and inside it the job's.
    let field = |line: &serde_json::Value, name: &str| -> Option<String> {
        let value = line["spans"]
            .as_array()?
            .iter()
            .find_map(|span| span.get(name))?;
        Some(
            value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned),
        )
    };
    // Other tests log into the same capture. This plan's lines are told by its id.
    let lines = log.lines();
    let said = |message: &str| {
        let about_this_plan = |line: &&serde_json::Value| {
            line["fields"]["message"] == message
                && field(line, "plan_id") == Some(plan.id.to_string())
        };
        lines
            .iter()
            .find(about_this_plan)
            .unwrap_or_else(|| panic!("nothing logged `{message}` for the plan"))
    };

    let jobs = queue::list_for_plan(&pool, plan.id).await.unwrap();
    for (message, job) in [
        ("clustered the plan", &jobs[0]),
        ("stored the route", &jobs[1]),
    ] {
        let line = said(message);
        assert_eq!(field(line, "worker_id"), Some(worker.to_string()));
        assert_eq!(field(line, "job_id"), Some(job.id.to_string()));
        assert_eq!(field(line, "request_id").as_deref(), Some("req-log"));
        assert_eq!(
            field(line, "kind"),
            Some(job.payload.kind().to_string()),
            "{message}"
        );
        assert_eq!(field(line, "attempt").as_deref(), Some("1"));
    }
}

/// What the event streams of the API are fed with (D48, D49): every step of a plan is
/// announced to those who watch it, and so is each of its searches while it runs.
#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn a_watcher_can_follow_a_plan_from_the_queue_to_its_routes(pool: PgPool) {
    let _turn = one_at_a_time().await;
    let city = testing::city(&pool, &[6, 6]).await;
    city.orders(&pool, date(12), 12, 2).await;
    let config = Config {
        progress: Duration::from_millis(100),
        ..testing::config()
    };
    let fleet = Fleet::with_workers(&pool, config, 2);
    let mut watcher = PgListener::connect_with(&pool).await.unwrap();
    watcher
        .listen_all([PLANS_CHANNEL, PROGRESS_CHANNEL])
        .await
        .unwrap();

    let new = city.new_plan(date(12), millis(600));
    let plan = plans::submit(&pool, &new, None).await.unwrap();
    done(&pool, plan.id).await;
    fleet.stop().await;

    let mut changes = 0;
    let mut searches: Vec<RouteProgress> = Vec::new();
    let silence = Duration::from_millis(300);
    while let Ok(notification) = tokio::time::timeout(silence, watcher.recv()).await {
        let notification = notification.unwrap();
        if notification.channel() == PLANS_CHANNEL {
            assert_eq!(notification.payload(), plan.id.to_string());
            changes += 1;
        } else {
            searches.push(serde_json::from_str(notification.payload()).unwrap());
        }
    }
    // The plan submitted, its `plan` job claimed and done, and each of the two route
    // jobs claimed and done. Each is a transaction, and is said once.
    assert_eq!(changes, 7);

    let jobs = queue::list_for_plan(&pool, plan.id).await.unwrap();
    let stored = routes::list_for_plan(&pool, plan.id).await.unwrap();
    assert_eq!((jobs.len(), stored.len()), (3, 2));
    for job in &jobs[1..] {
        let JobPayload::Route {
            courier_id,
            order_ids,
            ..
        } = &job.payload
        else {
            panic!("{:?} is not a route job", job.payload);
        };
        let of_this_job: Vec<&RouteProgress> = searches
            .iter()
            .filter(|progress| progress.job_id == job.id)
            .collect();
        assert!(
            of_this_job.len() >= 2,
            "{} announcements of a search of 600 ms",
            of_this_job.len()
        );
        let route = stored
            .iter()
            .find(|route| route.courier_id == *courier_id)
            .expect("the job stored a route for its courier");
        for progress in of_this_job {
            assert_eq!((progress.plan_id, progress.attempt), (plan.id, 1));
            // Its route calls at every stop of the job once.
            let mut places = progress.order.clone().unwrap();
            places.sort_unstable();
            assert!(places.into_iter().eq(0..order_ids.len() as u32));
            // And the route that was stored is no longer than any announced on the way.
            assert!(progress.best_cost_m >= route.distance_m);
        }
    }
    let known = |progress: &RouteProgress| jobs.iter().any(|job| job.id == progress.job_id);
    assert!(searches.iter().all(known));
}
