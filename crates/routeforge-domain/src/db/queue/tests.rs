//! The queue's test suite, against a real PostgreSQL.
//!
//! Nothing here waits for a lease to run out. A job that must stay held gets a long
//! lease, and one that must be expired gets a lease of no length, which the database
//! sees as over by the next statement.

use std::collections::HashSet;

use sqlx::postgres::PgListener;
use tokio::task::JoinSet;

use super::*;
use crate::db::testing::{self, LONG, Routing, complete_route, date, soon};
use crate::db::{orders, plans, routes};
use crate::ids::{CourierId, OrderId};
use crate::job::JobKind;
use crate::model::{NewRoute, OrderStatus, PlanCounts};

/// The database's clock, which is the only one the queue uses.
async fn db_now(pool: &PgPool) -> DateTime<Utc> {
    sqlx::query_scalar("SELECT now()")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Whether `at` lies `span` after a moment between two readings of the clock.
///
/// The readings are taken just before and just after the call under test. That pins
/// the time the call stored exactly, however long the machine took over it.
fn is_span_after(
    at: DateTime<Utc>,
    span: Duration,
    before: DateTime<Utc>,
    after: DateTime<Utc>,
) -> bool {
    before + span <= at && at <= after + span
}

/// Makes a queued job the oldest one that is due, so the next claim takes it.
async fn make_due(pool: &PgPool, job_id: JobId) {
    sqlx::query("UPDATE jobs SET run_at = now() - interval '1 day' WHERE id = $1")
        .bind(job_id.0)
        .execute(pool)
        .await
        .unwrap();
}

async fn stored(pool: &PgPool, job_id: JobId) -> Job {
    get(pool, job_id).await.unwrap().unwrap()
}

async fn statuses(pool: &PgPool, routing: &Routing) -> Vec<JobStatus> {
    let jobs = list_for_plan(pool, routing.plan.id).await.unwrap();
    jobs.into_iter().map(|job| job.status).collect()
}

/// Every order of the plan is pending and belongs to no plan.
async fn orders_are_free(pool: &PgPool, routing: &Routing) -> bool {
    for order_id in routing.order_ids() {
        let order = orders::get(pool, order_id).await.unwrap().unwrap();
        if (order.status, order.plan_id) != (OrderStatus::Pending, None) {
            return false;
        }
    }
    true
}

#[test]
fn the_retry_delay_doubles_up_to_a_minute() {
    let seconds: Vec<u64> = (1..=8)
        .map(|attempt| retry_delay(attempt).as_secs())
        .collect();
    assert_eq!(seconds, [2, 4, 8, 16, 32, 60, 60, 60]);
    // Attempts are counted from 1, and no count is too large.
    assert_eq!(retry_delay(0), retry_delay(1));
    assert_eq!(retry_delay(u32::MAX), Duration::from_secs(60));
}

#[sqlx::test]
async fn an_enqueued_job_is_queued_and_due(pool: PgPool) {
    let plan = testing::plan(&pool, date(12)).await;
    let payload = JobPayload::Route {
        plan_id: plan.id,
        courier_id: CourierId::generate(),
        order_ids: vec![OrderId::generate()],
        seed: u64::MAX,
    };
    let id = enqueue(&pool, &payload, Some("req-42")).await.unwrap();

    let job = stored(&pool, id).await;
    assert_eq!((job.id, job.plan_id), (id, plan.id));
    // The payload survives JSONB, a 64-bit seed included.
    assert_eq!(job.payload, payload);
    assert_eq!(job.status, JobStatus::Queued);
    assert_eq!((job.attempt, job.max_attempts), (0, MAX_ATTEMPTS));
    assert_eq!((job.claimed_by, job.lease_expires_at), (None, None));
    assert_eq!(
        (job.progress, &job.error, job.finished_at),
        (None, &None, None)
    );
    assert_eq!(job.request_id.as_deref(), Some("req-42"));
    assert!(job.run_at <= db_now(&pool).await);

    assert_eq!(get(&pool, JobId::generate()).await.unwrap(), None);
}

#[sqlx::test]
async fn the_schema_accepts_every_kind_and_status_of_job(pool: PgPool) {
    let plan = testing::plan(&pool, date(12)).await;
    let payloads = [
        JobPayload::Plan { plan_id: plan.id },
        JobPayload::Route {
            plan_id: plan.id,
            courier_id: CourierId::generate(),
            order_ids: Vec::new(),
            seed: 1,
        },
    ];
    let mut kinds = Vec::new();
    for payload in &payloads {
        let id = enqueue(&pool, payload, None).await.unwrap();
        kinds.push(stored(&pool, id).await.payload.kind());
    }
    assert_eq!(kinds, JobKind::ALL);

    let id = enqueue(&pool, &payloads[1], None).await.unwrap();
    for &status in JobStatus::ALL {
        sqlx::query(
            "UPDATE jobs
             SET status = $2,
                 claimed_by = $3,
                 lease_expires_at = CASE WHEN $2 = 'running' THEN now() END,
                 finished_at = CASE WHEN $2 IN ('done', 'failed', 'cancelled') THEN now() END
             WHERE id = $1",
        )
        .bind(id.0)
        .bind(status.as_str())
        .bind(WorkerId::generate().0)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(stored(&pool, id).await.status, status);
    }
}

#[sqlx::test]
async fn a_payload_must_agree_with_its_columns(pool: PgPool) {
    let plan = testing::plan(&pool, date(12)).await;
    let other = testing::plan(&pool, date(13)).await;
    let insert = "INSERT INTO jobs (id, plan_id, kind, payload, max_attempts)
                  VALUES ($1, $2, $3, $4, 3)";
    let payload = Json(JobPayload::Plan { plan_id: plan.id });

    for (plan_id, kind) in [(plan.id, "route"), (other.id, "plan")] {
        let error = sqlx::query(insert)
            .bind(JobId::generate().0)
            .bind(plan_id.0)
            .bind(kind)
            .bind(&payload)
            .execute(&pool)
            .await
            .unwrap_err();
        let database = error.as_database_error().unwrap();
        assert_eq!(database.constraint(), Some("jobs_payload_check"));
    }
}

#[sqlx::test]
async fn workers_hear_of_a_job_when_its_transaction_commits(pool: PgPool) {
    let plan = testing::plan(&pool, date(12)).await;
    let payload = JobPayload::Plan { plan_id: plan.id };
    let mut listener = PgListener::connect_with(&pool).await.unwrap();
    listener.listen(JOBS_CHANNEL).await.unwrap();
    let silence = Duration::from_millis(300);

    // Rolled back: no job, and no word of one.
    let mut tx = pool.begin().await.unwrap();
    enqueue(&mut *tx, &payload, None).await.unwrap();
    tx.rollback().await.unwrap();
    assert!(
        tokio::time::timeout(silence, listener.recv())
            .await
            .is_err()
    );
    assert!(list_for_plan(&pool, plan.id).await.unwrap().is_empty());

    // Committed: nothing before the commit, the notification after it.
    let mut tx = pool.begin().await.unwrap();
    enqueue(&mut *tx, &payload, None).await.unwrap();
    assert!(
        tokio::time::timeout(silence, listener.recv())
            .await
            .is_err()
    );
    tx.commit().await.unwrap();
    let notification = soon(listener.recv()).await.unwrap();
    assert_eq!(notification.channel(), JOBS_CHANNEL);
    assert_eq!(notification.payload(), "plan");
}

/// The payload is the kind of the job and nothing more, so that PostgreSQL folds the
/// notifications of one transaction into one (D26).
#[sqlx::test]
async fn a_plan_that_fans_out_wakes_a_worker_once(pool: PgPool) {
    let plan = testing::plan(&pool, date(12)).await;
    let mut listener = PgListener::connect_with(&pool).await.unwrap();
    listener.listen(JOBS_CHANNEL).await.unwrap();

    let mut tx = pool.begin().await.unwrap();
    for seed in 0..20 {
        let payload = JobPayload::Route {
            plan_id: plan.id,
            courier_id: CourierId::generate(),
            order_ids: Vec::new(),
            seed,
        };
        enqueue(&mut *tx, &payload, None).await.unwrap();
    }
    tx.commit().await.unwrap();

    let notification = soon(listener.recv()).await.unwrap();
    assert_eq!(notification.payload(), "route");
    let silence = Duration::from_millis(300);
    assert!(
        tokio::time::timeout(silence, listener.recv())
            .await
            .is_err(),
        "twenty jobs were announced more than once"
    );
    assert_eq!(list_for_plan(&pool, plan.id).await.unwrap().len(), 20);
}

#[sqlx::test]
async fn a_claim_takes_the_oldest_due_job_and_starts_its_lease(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 3, 1).await;
    let worker = WorkerId::generate();
    let not_yet = routing.routes[0].job_id;
    sqlx::query("UPDATE jobs SET run_at = now() + interval '1 hour' WHERE id = $1")
        .bind(not_yet.0)
        .execute(&pool)
        .await
        .unwrap();

    let before = db_now(&pool).await;
    let first = claim(&pool, worker, Duration::from_secs(60))
        .await
        .unwrap()
        .unwrap();
    let after = db_now(&pool).await;
    assert_eq!(
        first.lease,
        Lease {
            job_id: routing.routes[1].job_id,
            plan_id: routing.plan.id,
            worker_id: worker,
            attempt: 1,
            max_attempts: MAX_ATTEMPTS,
        }
    );
    assert!(!first.lease.is_last_attempt());
    assert_eq!(first.request_id.as_deref(), Some("req-1"));
    let JobPayload::Route { courier_id, .. } = first.payload else {
        panic!("a route job was enqueued");
    };
    assert_eq!(courier_id, routing.routes[1].courier_id);

    let job = stored(&pool, first.lease.job_id).await;
    assert_eq!(job.status, JobStatus::Running);
    assert_eq!((job.attempt, job.claimed_by), (1, Some(worker)));
    let expires = job.lease_expires_at.unwrap();
    assert!(
        is_span_after(expires, Duration::from_secs(60), before, after),
        "{expires}"
    );

    // A held job is not handed out again, and a job is not due before its time.
    let second = claim(&pool, worker, LONG).await.unwrap().unwrap();
    assert_eq!(second.lease.job_id, routing.routes[2].job_id);
    assert_eq!(claim(&pool, worker, LONG).await.unwrap(), None);
    assert_eq!(stored(&pool, not_yet).await.status, JobStatus::Queued);
}

/// The database adds the span to its own clock, and says so when it cannot.
#[sqlx::test]
async fn a_lease_the_database_cannot_add_to_its_clock_is_refused(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 1, 1).await;
    let worker = WorkerId::generate();

    let error = claim(&pool, worker, Duration::MAX).await.unwrap_err();
    assert!(
        matches!(&error, DbError::Sqlx(sqlx::Error::Database(_))),
        "{error:?}"
    );
    // The claim failed as a whole: the job was not taken.
    let job = stored(&pool, routing.routes[0].job_id).await;
    assert_eq!((job.status, job.attempt), (JobStatus::Queued, 0));

    let job = claim(&pool, worker, LONG).await.unwrap().unwrap();
    let error = heartbeat(&pool, &job.lease, Duration::MAX, None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, DbError::Sqlx(sqlx::Error::Database(_))),
        "{error:?}"
    );
    assert_eq!(
        heartbeat(&pool, &job.lease, LONG, None).await.unwrap(),
        Heartbeat::Extended
    );
}

#[sqlx::test]
async fn concurrent_claimers_each_get_different_jobs(pool: PgPool) {
    const JOBS: usize = 60;
    const CLAIMERS: usize = 8;
    let plan = testing::plan(&pool, date(12)).await;
    let mut enqueued = HashSet::new();
    for seed in 0..JOBS {
        let payload = JobPayload::Route {
            plan_id: plan.id,
            courier_id: CourierId::generate(),
            order_ids: Vec::new(),
            seed: seed as u64,
        };
        enqueued.insert(enqueue(&pool, &payload, None).await.unwrap());
    }

    let mut claimers = JoinSet::new();
    for _ in 0..CLAIMERS {
        let pool = pool.clone();
        claimers.spawn(async move {
            let worker = WorkerId::generate();
            let mut mine = Vec::new();
            while let Some(job) = claim(&pool, worker, LONG).await.unwrap() {
                assert_eq!((job.lease.worker_id, job.lease.attempt), (worker, 1));
                mine.push(job.lease.job_id);
            }
            (worker, mine)
        });
    }
    let claimed: Vec<(WorkerId, Vec<JobId>)> = soon(claimers.join_all()).await;

    let handed_out: usize = claimed.iter().map(|(_, mine)| mine.len()).sum();
    let distinct: HashSet<JobId> = claimed
        .iter()
        .flat_map(|(_, mine)| mine.iter().copied())
        .collect();
    assert_eq!(handed_out, JOBS, "a job was handed out twice");
    assert_eq!(distinct, enqueued);
    let busy = claimed.iter().filter(|(_, mine)| !mine.is_empty()).count();
    assert!(busy > 1, "the claimers did not run side by side");

    // The table agrees with what the claimers were told.
    for (worker, mine) in claimed {
        for job_id in mine {
            let job = stored(&pool, job_id).await;
            assert_eq!(
                (job.status, job.attempt, job.claimed_by),
                (JobStatus::Running, 1, Some(worker))
            );
        }
    }
}

#[sqlx::test]
async fn a_claim_does_not_wait_for_a_job_someone_else_has_locked(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 3, 1).await;
    let [oldest, middle, newest] = [0, 1, 2].map(|at: usize| routing.routes[at].job_id);
    let worker = WorkerId::generate();

    // What a concurrent claim holds while it is between its select and its commit.
    let mut other = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM jobs WHERE id = $1 FOR UPDATE")
        .bind(oldest.0)
        .execute(&mut *other)
        .await
        .unwrap();

    // A claim that waited for the lock would wait for as long as this test holds it,
    // which is for ever. So the limit can be generous: on a busy machine a claim may
    // spend seconds just getting a connection.
    for expected in [Some(middle), Some(newest), None] {
        let claimed = soon(claim(&pool, worker, LONG)).await.unwrap();
        assert_eq!(claimed.map(|job| job.lease.job_id), expected);
    }

    other.rollback().await.unwrap();
    let claimed = claim(&pool, worker, LONG).await.unwrap().unwrap();
    assert_eq!(claimed.lease.job_id, oldest);
}

#[sqlx::test]
async fn an_expired_lease_is_claimed_again_by_another_worker(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 1, 2).await;
    let (dead, alive) = (WorkerId::generate(), WorkerId::generate());

    let first = claim(&pool, dead, Duration::ZERO).await.unwrap().unwrap();
    let second = claim(&pool, alive, LONG).await.unwrap().unwrap();

    assert_eq!(second.lease.job_id, first.lease.job_id);
    assert_eq!((first.lease.attempt, second.lease.attempt), (1, 2));
    assert_eq!(second.payload, first.payload);
    let job = stored(&pool, routing.routes[0].job_id).await;
    assert_eq!(
        (job.status, job.attempt, job.claimed_by),
        (JobStatus::Running, 2, Some(alive))
    );
    // The new lease is the new worker's, and it holds.
    assert_eq!(claim(&pool, dead, LONG).await.unwrap(), None);
}

#[sqlx::test]
async fn a_worker_that_lost_its_job_can_do_nothing_to_it(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 1, 2).await;
    let (slow, quick) = (WorkerId::generate(), WorkerId::generate());
    let stale = claim(&pool, slow, Duration::ZERO).await.unwrap().unwrap();
    let fresh = claim(&pool, quick, LONG).await.unwrap().unwrap();
    let before = stored(&pool, fresh.lease.job_id).await;

    let progress = JobProgress {
        iterations: 1,
        best_cost_m: 1,
    };
    assert_eq!(
        heartbeat(&pool, &stale.lease, LONG, Some(progress))
            .await
            .unwrap(),
        Heartbeat::Lost
    );
    assert_eq!(complete_route(&pool, &stale).await, None);
    assert!(!fail(&pool, &stale.lease, "too late").await.unwrap());
    assert!(!release(&pool, &stale.lease).await.unwrap());

    // Nothing it tried has left a mark.
    assert_eq!(stored(&pool, fresh.lease.job_id).await, before);
    assert!(
        routes::list_for_plan(&pool, routing.plan.id)
            .await
            .unwrap()
            .is_empty()
    );
    let plan = plans::get(&pool, routing.plan.id).await.unwrap().unwrap();
    assert_eq!((plan.status, plan.routes_done), (PlanStatus::Routing, 0));

    // The worker that has the job stores its result, once.
    assert_eq!(complete_route(&pool, &fresh).await, Some(PlanStatus::Done));
    assert_eq!(complete_route(&pool, &stale).await, None);
    assert_eq!(complete_route(&pool, &fresh).await, None);
    let stored_routes = routes::list_for_plan(&pool, routing.plan.id).await.unwrap();
    assert_eq!(stored_routes.len(), 1);
    assert_eq!(stored_routes[0].stops.len(), 2);
}

/// The case the attempt number is there for. The worker's id is the same on both
/// claims, so nothing but the number tells the abandoned work from the current one.
#[sqlx::test]
async fn an_earlier_claim_is_void_even_for_the_worker_that_claims_the_job_again(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 1, 2).await;
    let worker = WorkerId::generate();
    let abandoned = claim(&pool, worker, Duration::ZERO).await.unwrap().unwrap();
    let current = claim(&pool, worker, LONG).await.unwrap().unwrap();
    assert_eq!(current.lease.job_id, abandoned.lease.job_id);
    assert_eq!(current.lease.worker_id, abandoned.lease.worker_id);
    assert_eq!((abandoned.lease.attempt, current.lease.attempt), (1, 2));
    let before = stored(&pool, current.lease.job_id).await;

    assert_eq!(
        heartbeat(&pool, &abandoned.lease, LONG, None)
            .await
            .unwrap(),
        Heartbeat::Lost
    );
    assert_eq!(complete_route(&pool, &abandoned).await, None);
    assert!(!fail(&pool, &abandoned.lease, "too late").await.unwrap());
    assert!(!release(&pool, &abandoned.lease).await.unwrap());
    assert_eq!(stored(&pool, current.lease.job_id).await, before);

    assert_eq!(
        heartbeat(&pool, &current.lease, LONG, None).await.unwrap(),
        Heartbeat::Extended
    );
    assert_eq!(
        complete_route(&pool, &current).await,
        Some(PlanStatus::Done)
    );
    let stored_routes = routes::list_for_plan(&pool, routing.plan.id).await.unwrap();
    assert_eq!(stored_routes.len(), 1);
}

#[sqlx::test]
async fn a_late_worker_keeps_a_job_nobody_else_has_claimed(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 1, 1).await;
    let (late, other) = (WorkerId::generate(), WorkerId::generate());
    let job = claim(&pool, late, Duration::ZERO).await.unwrap().unwrap();
    let lapsed = stored(&pool, job.lease.job_id).await.lease_expires_at;
    assert!(lapsed.unwrap() <= db_now(&pool).await);

    // Its lease has lapsed, but it is still the one meant to do the job.
    assert_eq!(
        heartbeat(&pool, &job.lease, LONG, None).await.unwrap(),
        Heartbeat::Extended
    );
    assert_eq!(claim(&pool, other, LONG).await.unwrap(), None);

    // The same holds for completing after the lease has lapsed.
    sqlx::query("UPDATE jobs SET lease_expires_at = now() WHERE id = $1")
        .bind(job.lease.job_id.0)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(complete_route(&pool, &job).await, Some(PlanStatus::Done));
    let done = stored(&pool, routing.routes[0].job_id).await;
    assert_eq!((done.status, done.attempt), (JobStatus::Done, 1));
}

#[sqlx::test]
async fn a_heartbeat_extends_the_lease_and_stores_progress(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 1, 1).await;
    let worker = WorkerId::generate();
    let job = claim(&pool, worker, Duration::from_secs(10))
        .await
        .unwrap()
        .unwrap();
    let job_id = routing.routes[0].job_id;
    let short = stored(&pool, job_id).await.lease_expires_at.unwrap();

    let progress = JobProgress {
        iterations: 16_384,
        best_cost_m: u64::MAX,
    };
    let before = db_now(&pool).await;
    let answer = heartbeat(&pool, &job.lease, Duration::from_secs(120), Some(progress))
        .await
        .unwrap();
    let after = db_now(&pool).await;
    assert_eq!(answer, Heartbeat::Extended);
    let extended = stored(&pool, job_id).await;
    let expires = extended.lease_expires_at.unwrap();
    assert!(
        is_span_after(expires, Duration::from_secs(120), before, after),
        "{expires}"
    );
    assert!(expires > short);
    assert_eq!(extended.progress, Some(progress));

    // A heartbeat without progress keeps what was stored.
    heartbeat(&pool, &job.lease, LONG, None).await.unwrap();
    assert_eq!(stored(&pool, job_id).await.progress, Some(progress));

    // A heartbeat for a job that does not exist has lost it.
    let nowhere = Lease {
        job_id: JobId::generate(),
        ..job.lease
    };
    assert_eq!(
        heartbeat(&pool, &nowhere, LONG, None).await.unwrap(),
        Heartbeat::Lost
    );
}

#[sqlx::test]
async fn a_completion_stores_the_results_together_with_the_job(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 2, 3).await;
    let worker = WorkerId::generate();
    let job = claim(&pool, worker, LONG).await.unwrap().unwrap();
    let job_id = job.lease.job_id;
    let JobPayload::Route {
        courier_id,
        order_ids,
        ..
    } = job.payload.clone()
    else {
        panic!("a route job was enqueued");
    };
    let route = NewRoute {
        plan_id: routing.plan.id,
        courier_id,
        distance_m: 4_200,
        stops: order_ids,
    };

    // Dropped before the commit: the route is not there and the job is still held.
    let mut completion = begin_complete(&pool, &job.lease).await.unwrap().unwrap();
    routes::insert(completion.conn(), &route).await.unwrap();
    plans::route_finished(completion.conn(), routing.plan.id)
        .await
        .unwrap();
    drop(completion);
    assert!(
        routes::list_for_plan(&pool, routing.plan.id)
            .await
            .unwrap()
            .is_empty()
    );
    let plan = plans::get(&pool, routing.plan.id).await.unwrap().unwrap();
    assert_eq!(plan.routes_done, 0);
    assert_eq!(stored(&pool, job_id).await.status, JobStatus::Running);
    assert_eq!(
        heartbeat(&pool, &job.lease, LONG, None).await.unwrap(),
        Heartbeat::Extended
    );

    // Committed: the route, the count and the job being done arrive together.
    let mut completion = begin_complete(&pool, &job.lease).await.unwrap().unwrap();
    routes::insert(completion.conn(), &route).await.unwrap();
    let status = plans::route_finished(completion.conn(), routing.plan.id)
        .await
        .unwrap();
    assert_eq!(status, Some(PlanStatus::Routing));
    completion.commit().await.unwrap();

    let stored_routes = routes::list_for_plan(&pool, routing.plan.id).await.unwrap();
    assert_eq!(stored_routes.len(), 1);
    assert_eq!(stored_routes[0].distance_m, 4_200);
    let plan = plans::get(&pool, routing.plan.id).await.unwrap().unwrap();
    assert_eq!((plan.status, plan.routes_done), (PlanStatus::Routing, 1));
    let done = stored(&pool, job_id).await;
    assert_eq!(done.status, JobStatus::Done);
    assert_eq!(
        (done.lease_expires_at, done.claimed_by),
        (None, Some(worker))
    );
    assert!(done.finished_at.is_some());

    // Done is final for every call.
    assert_eq!(
        heartbeat(&pool, &job.lease, LONG, None).await.unwrap(),
        Heartbeat::Lost
    );
    assert!(!fail(&pool, &job.lease, "after the fact").await.unwrap());
    assert!(!release(&pool, &job.lease).await.unwrap());
    assert!(begin_complete(&pool, &job.lease).await.unwrap().is_none());
}

#[sqlx::test]
async fn a_failed_attempt_is_retried_after_a_growing_delay(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 1, 1).await;
    let job_id = routing.routes[0].job_id;
    let worker = WorkerId::generate();

    for (attempt, delay) in [(1, 2), (2, 4)] {
        let job = claim(&pool, worker, LONG).await.unwrap().unwrap();
        assert_eq!(job.lease.attempt, attempt);
        let before = db_now(&pool).await;
        assert!(
            fail(&pool, &job.lease, "the matrix was too large")
                .await
                .unwrap()
        );
        let after = db_now(&pool).await;

        let retry = stored(&pool, job_id).await;
        assert_eq!((retry.status, retry.attempt), (JobStatus::Queued, attempt));
        assert_eq!((retry.claimed_by, retry.lease_expires_at), (None, None));
        assert_eq!(retry.error.as_deref(), Some("the matrix was too large"));
        assert_eq!(retry.finished_at, None);
        // Due again two seconds after the first failure and four after the second.
        assert!(
            is_span_after(retry.run_at, Duration::from_secs(delay), before, after),
            "{}",
            retry.run_at
        );

        // Not before its time. Pushed far out so that a slow machine cannot reach it.
        sqlx::query("UPDATE jobs SET run_at = now() + interval '1 hour' WHERE id = $1")
            .bind(job_id.0)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(claim(&pool, worker, LONG).await.unwrap(), None);
        make_due(&pool, job_id).await;
    }

    // A plan survives the retries of its jobs.
    let plan = plans::get(&pool, routing.plan.id).await.unwrap().unwrap();
    assert_eq!(plan.status, PlanStatus::Routing);
    let third = claim(&pool, worker, LONG).await.unwrap().unwrap();
    assert!(third.lease.is_last_attempt());
}

#[sqlx::test]
async fn the_last_failed_attempt_fails_the_job_and_its_plan(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 4, 2).await;
    let [stored_route, failing, held, waiting] =
        [0, 1, 2, 3].map(|at: usize| routing.routes[at].job_id);
    let (unlucky, bystander) = (WorkerId::generate(), WorkerId::generate());

    // One route is stored, one is being worked on, one is still waiting.
    let first = claim(&pool, unlucky, LONG).await.unwrap().unwrap();
    assert_eq!(first.lease.job_id, stored_route);
    assert_eq!(
        complete_route(&pool, &first).await,
        Some(PlanStatus::Routing)
    );
    let mut job = claim(&pool, unlucky, LONG).await.unwrap().unwrap();
    assert_eq!(job.lease.job_id, failing);
    let in_progress = claim(&pool, bystander, LONG).await.unwrap().unwrap();
    assert_eq!(in_progress.lease.job_id, held);

    for _ in 1..MAX_ATTEMPTS {
        assert!(fail(&pool, &job.lease, "out of memory").await.unwrap());
        make_due(&pool, failing).await;
        job = claim(&pool, unlucky, LONG).await.unwrap().unwrap();
        assert_eq!(job.lease.job_id, failing);
    }
    assert!(job.lease.is_last_attempt());
    assert!(
        fail(&pool, &job.lease, "out of memory again")
            .await
            .unwrap()
    );

    let failed = stored(&pool, failing).await;
    assert_eq!(
        (failed.status, failed.attempt),
        (JobStatus::Failed, MAX_ATTEMPTS)
    );
    assert_eq!(failed.error.as_deref(), Some("out of memory again"));
    assert!(failed.finished_at.is_some() && failed.lease_expires_at.is_none());

    // The plan fails with it, and its other outstanding jobs are stopped.
    let plan = plans::get(&pool, routing.plan.id).await.unwrap().unwrap();
    assert_eq!(plan.status, PlanStatus::Failed);
    assert_eq!(plan.error.as_deref(), Some("out of memory again"));
    assert!(plan.finished_at.is_some());
    assert_eq!(
        statuses(&pool, &routing).await,
        [
            JobStatus::Done,
            JobStatus::Failed,
            JobStatus::Cancelled,
            JobStatus::Cancelled
        ]
    );
    assert_eq!(stored(&pool, waiting).await.attempt, 0);
    assert_eq!(claim(&pool, bystander, LONG).await.unwrap(), None);
    assert_eq!(
        heartbeat(&pool, &in_progress.lease, LONG, None)
            .await
            .unwrap(),
        Heartbeat::Cancelled
    );
    assert_eq!(complete_route(&pool, &in_progress).await, None);

    // What the plan had done is undone, so the date can be planned again.
    assert!(
        routes::list_for_plan(&pool, routing.plan.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(orders_are_free(&pool, &routing).await);
    testing::plan(&pool, date(12)).await;

    // A failed job stays failed.
    assert!(!fail(&pool, &job.lease, "once more").await.unwrap());
    assert_eq!(stored(&pool, failing).await, failed);
}

#[sqlx::test]
async fn a_worker_that_dies_on_the_last_attempt_fails_the_plan_as_well(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 2, 1).await;
    let [doomed, other] = [0, 1].map(|at: usize| routing.routes[at].job_id);
    let (dying, idle) = (WorkerId::generate(), WorkerId::generate());

    // Two workers die holding the job, and a third one too, on the last attempt.
    for attempt in 1..=MAX_ATTEMPTS {
        make_due(&pool, doomed).await;
        let job = claim(&pool, dying, Duration::ZERO).await.unwrap().unwrap();
        assert_eq!((job.lease.job_id, job.lease.attempt), (doomed, attempt));
    }

    // Nobody may claim it a fourth time. The next worker takes the other job, and
    // the first claim that finds nothing buries the dead one.
    let job = claim(&pool, idle, LONG).await.unwrap().unwrap();
    assert_eq!(job.lease.job_id, other);
    assert_eq!(stored(&pool, doomed).await.status, JobStatus::Running);
    assert_eq!(claim(&pool, idle, LONG).await.unwrap(), None);

    let buried = stored(&pool, doomed).await;
    assert_eq!(
        (buried.status, buried.attempt),
        (JobStatus::Failed, MAX_ATTEMPTS)
    );
    assert_eq!(buried.error.as_deref(), Some(WORKER_LOST));
    let plan = plans::get(&pool, routing.plan.id).await.unwrap().unwrap();
    assert_eq!(plan.status, PlanStatus::Failed);
    assert_eq!(plan.error.as_deref(), Some(WORKER_LOST));
    assert_eq!(stored(&pool, other).await.status, JobStatus::Cancelled);
    assert_eq!(
        heartbeat(&pool, &job.lease, LONG, None).await.unwrap(),
        Heartbeat::Cancelled
    );
    assert!(orders_are_free(&pool, &routing).await);
}

#[sqlx::test]
async fn a_worker_that_comes_back_on_its_last_attempt_is_not_buried(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 1, 1).await;
    let job_id = routing.routes[0].job_id;
    let (slow, idle) = (WorkerId::generate(), WorkerId::generate());
    sqlx::query("UPDATE jobs SET max_attempts = 1 WHERE id = $1")
        .bind(job_id.0)
        .execute(&pool)
        .await
        .unwrap();
    let job = claim(&pool, slow, Duration::ZERO).await.unwrap().unwrap();
    assert!(job.lease.is_last_attempt());

    // Its heartbeat arrives before anyone has looked.
    assert_eq!(
        heartbeat(&pool, &job.lease, LONG, None).await.unwrap(),
        Heartbeat::Extended
    );
    assert_eq!(claim(&pool, idle, LONG).await.unwrap(), None);
    assert_eq!(stored(&pool, job_id).await.status, JobStatus::Running);
    assert_eq!(complete_route(&pool, &job).await, Some(PlanStatus::Done));
}

/// A solver that is older than the API can meet a job it does not understand. It must
/// say so, and the job must not sit under a lease that nobody is working behind.
#[sqlx::test]
async fn a_job_this_build_cannot_read_fails_openly(pool: PgPool) {
    let plan = testing::plan(&pool, date(12)).await;
    let job_id = JobId::generate();
    let from_a_newer_build = serde_json::json!({
        "kind": "route",
        "plan_id": plan.id,
        "vehicle": "cargo_bike",
    });
    sqlx::query(
        "INSERT INTO jobs (id, plan_id, kind, payload, max_attempts)
         VALUES ($1, $2, 'route', $3, 2)",
    )
    .bind(job_id.0)
    .bind(plan.id.0)
    .bind(&from_a_newer_build)
    .execute(&pool)
    .await
    .unwrap();
    let worker = WorkerId::generate();
    let state = async || -> (String, i64, Option<String>) {
        sqlx::query_as("SELECT status, attempt, error FROM jobs WHERE id = $1")
            .bind(job_id.0)
            .fetch_one(&pool)
            .await
            .unwrap()
    };

    let error = claim(&pool, worker, LONG).await.unwrap_err();
    assert!(
        matches!(&error, DbError::Unreadable(what) if what.contains("payload")),
        "{error:?}"
    );
    // The attempt was used and has failed, so the job waits for its retry.
    let (status, attempt, reason) = state().await;
    assert_eq!((status.as_str(), attempt), ("queued", 1));
    assert!(reason.unwrap().contains("cannot be read"));

    // Reading the job says the same.
    let error = get(&pool, job_id).await.unwrap_err();
    assert!(matches!(error, DbError::Unreadable(_)), "{error:?}");
    let error = list_for_plan(&pool, plan.id).await.unwrap_err();
    assert!(matches!(error, DbError::Unreadable(_)), "{error:?}");

    // After the last attempt the plan fails, with the reason for the operator to see.
    make_due(&pool, job_id).await;
    claim(&pool, worker, LONG).await.unwrap_err();
    let (status, attempt, _) = state().await;
    assert_eq!((status.as_str(), attempt), ("failed", 2));
    let plan = plans::get(&pool, plan.id).await.unwrap().unwrap();
    assert_eq!(plan.status, PlanStatus::Failed);
    assert!(plan.error.unwrap().contains("cannot be read"));
    assert_eq!(claim(&pool, worker, LONG).await.unwrap(), None);
}

#[sqlx::test]
async fn a_released_job_is_due_at_once_and_has_used_no_attempt(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 2, 1).await;
    let [first, second] = [0, 1].map(|at: usize| routing.routes[at].job_id);
    let (leaving, staying) = (WorkerId::generate(), WorkerId::generate());
    let job = claim(&pool, leaving, LONG).await.unwrap().unwrap();
    assert_eq!(job.lease.job_id, first);

    let mut listener = PgListener::connect_with(&pool).await.unwrap();
    listener.listen(JOBS_CHANNEL).await.unwrap();
    assert!(release(&pool, &job.lease).await.unwrap());
    let notification = soon(listener.recv()).await.unwrap();
    assert_eq!(notification.payload(), "route");
    drop(listener);

    let released = stored(&pool, first).await;
    assert_eq!((released.status, released.attempt), (JobStatus::Queued, 0));
    assert_eq!(
        (released.claimed_by, released.lease_expires_at),
        (None, None)
    );
    assert!(
        !release(&pool, &job.lease).await.unwrap(),
        "it is not held any more"
    );

    // It kept its place in front of the job that was enqueued after it.
    let again = claim(&pool, staying, LONG).await.unwrap().unwrap();
    assert_eq!((again.lease.job_id, again.lease.attempt), (first, 1));
    let next = claim(&pool, staying, LONG).await.unwrap().unwrap();
    assert_eq!(next.lease.job_id, second);
}

#[sqlx::test]
async fn cancelling_a_plan_stops_its_jobs_and_frees_its_orders(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 4, 2).await;
    let (finished, busy) = (WorkerId::generate(), WorkerId::generate());
    let first = claim(&pool, finished, LONG).await.unwrap().unwrap();
    assert_eq!(
        complete_route(&pool, &first).await,
        Some(PlanStatus::Routing)
    );
    let in_progress = claim(&pool, busy, LONG).await.unwrap().unwrap();

    assert!(cancel_plan(&pool, routing.plan.id).await.unwrap());

    let plan = plans::get(&pool, routing.plan.id).await.unwrap().unwrap();
    assert_eq!((plan.status, &plan.error), (PlanStatus::Cancelled, &None));
    assert!(plan.finished_at.is_some());
    assert_eq!(
        statuses(&pool, &routing).await,
        [
            JobStatus::Done,
            JobStatus::Cancelled,
            JobStatus::Cancelled,
            JobStatus::Cancelled
        ]
    );

    // Queued jobs are gone from the queue.
    assert_eq!(claim(&pool, busy, LONG).await.unwrap(), None);
    // The worker in the middle of one is told, and can no longer store its route.
    assert_eq!(
        heartbeat(&pool, &in_progress.lease, LONG, None)
            .await
            .unwrap(),
        Heartbeat::Cancelled
    );
    assert_eq!(complete_route(&pool, &in_progress).await, None);
    assert!(!fail(&pool, &in_progress.lease, "cancelled").await.unwrap());
    assert!(!release(&pool, &in_progress.lease).await.unwrap());
    let cancelled = stored(&pool, in_progress.lease.job_id).await;
    assert_eq!(cancelled.status, JobStatus::Cancelled);
    assert!(cancelled.finished_at.is_some() && cancelled.lease_expires_at.is_none());

    // The route that was stored is gone, and every order can be planned again.
    assert!(
        routes::list_for_plan(&pool, routing.plan.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(orders_are_free(&pool, &routing).await);
    let mut tx = pool.begin().await.unwrap();
    let pending = orders::lock_pending(&mut tx, date(12)).await.unwrap();
    assert_eq!(pending.len(), 8);
    tx.rollback().await.unwrap();
    testing::plan(&pool, date(12)).await;

    // Cancelling is done once.
    assert!(!cancel_plan(&pool, routing.plan.id).await.unwrap());
}

#[sqlx::test]
async fn a_plan_that_is_over_cannot_be_cancelled(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 1, 2).await;
    let job = claim(&pool, WorkerId::generate(), LONG)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(complete_route(&pool, &job).await, Some(PlanStatus::Done));

    assert!(!cancel_plan(&pool, routing.plan.id).await.unwrap());
    assert!(!cancel_plan(&pool, PlanId::generate()).await.unwrap());

    let plan = plans::get(&pool, routing.plan.id).await.unwrap().unwrap();
    assert_eq!(plan.status, PlanStatus::Done);
    assert_eq!(
        routes::list_for_plan(&pool, routing.plan.id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(!orders_are_free(&pool, &routing).await);
}

#[sqlx::test]
async fn a_queued_plan_is_cancelled_before_it_is_clustered(pool: PgPool) {
    let new = testing::new_plan(&pool, date(12)).await;
    let plan = plans::submit(&pool, &new, None).await.unwrap();
    let worker = WorkerId::generate();
    let job = claim(&pool, worker, LONG).await.unwrap().unwrap();
    assert_eq!(job.payload, JobPayload::Plan { plan_id: plan.id });

    assert!(cancel_plan(&pool, plan.id).await.unwrap());

    // The worker cannot fan the plan out any more.
    assert!(begin_complete(&pool, &job.lease).await.unwrap().is_none());
    let plan = plans::get(&pool, plan.id).await.unwrap().unwrap();
    assert_eq!(plan.status, PlanStatus::Cancelled);
    plans::submit(&pool, &new, None).await.unwrap();
}

/// Two transactions that are made to overlap: the second one starts while the
/// first holds its locks, and has to wait for it.
#[sqlx::test]
async fn two_routes_finishing_together_mark_the_plan_done_once(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 2, 2).await;
    let first = claim(&pool, WorkerId::generate(), LONG)
        .await
        .unwrap()
        .unwrap();
    let second = claim(&pool, WorkerId::generate(), LONG)
        .await
        .unwrap()
        .unwrap();
    let JobPayload::Route {
        courier_id,
        order_ids,
        ..
    } = first.payload.clone()
    else {
        panic!("a route job was enqueued");
    };

    let mut completion = begin_complete(&pool, &first.lease).await.unwrap().unwrap();
    let route = NewRoute {
        plan_id: routing.plan.id,
        courier_id,
        distance_m: 1_000,
        stops: order_ids,
    };
    routes::insert(completion.conn(), &route).await.unwrap();
    let seen_by_first = plans::route_finished(completion.conn(), routing.plan.id)
        .await
        .unwrap();

    let other = tokio::spawn({
        let pool = pool.clone();
        async move { complete_route(&pool, &second).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!other.is_finished(), "the second completion did not wait");

    completion.commit().await.unwrap();
    let seen_by_second = soon(other).await.unwrap();

    assert_eq!(seen_by_first, Some(PlanStatus::Routing));
    assert_eq!(seen_by_second, Some(PlanStatus::Done));
    let plan = plans::get(&pool, routing.plan.id).await.unwrap().unwrap();
    assert_eq!(
        (plan.status, plan.routes_done, plan.routes_total),
        (PlanStatus::Done, 2, 2)
    );
    assert_eq!(
        routes::list_for_plan(&pool, routing.plan.id)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[sqlx::test]
async fn many_routes_finishing_together_mark_the_plan_done_once(pool: PgPool) {
    const ROUTES: usize = 8;
    let routing = testing::routing(&pool, date(12), ROUTES, 2).await;
    let mut held = Vec::new();
    for _ in 0..ROUTES {
        let job = claim(&pool, WorkerId::generate(), LONG).await.unwrap();
        held.push(job.unwrap());
    }

    let mut finishing = JoinSet::new();
    for job in held {
        let pool = pool.clone();
        finishing.spawn(async move { complete_route(&pool, &job).await });
    }
    let seen: Vec<Option<PlanStatus>> = soon(finishing.join_all()).await;

    let done = seen
        .iter()
        .filter(|&&status| status == Some(PlanStatus::Done))
        .count();
    let routing_still = seen
        .iter()
        .filter(|&&status| status == Some(PlanStatus::Routing))
        .count();
    assert_eq!((done, routing_still), (1, ROUTES - 1));

    let plan = plans::get(&pool, routing.plan.id).await.unwrap().unwrap();
    assert_eq!(
        (plan.status, plan.routes_done),
        (PlanStatus::Done, ROUTES as u32)
    );
    assert_eq!(
        routes::list_for_plan(&pool, routing.plan.id)
            .await
            .unwrap()
            .len(),
        ROUTES
    );
    assert_eq!(statuses(&pool, &routing).await, [JobStatus::Done; ROUTES]);
}

/// What the lock order is for. Routes complete, one job fails for good and the plan
/// is cancelled, all at once and in every starting order. Nothing may deadlock, and
/// the plan must end whole: done with all its routes, or over with none.
#[sqlx::test]
async fn completions_a_final_failure_and_a_cancel_cannot_deadlock(pool: PgPool) {
    const ROUTES: usize = 4;
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Step {
        Complete(usize),
        FailForGood(usize),
        Cancel,
    }

    for round in 0..12 {
        let service_date = date(1 + round);
        let routing = testing::routing(&pool, service_date, ROUTES, 2).await;
        let one_fails = round % 2 == 1;
        if one_fails {
            sqlx::query("UPDATE jobs SET max_attempts = 1 WHERE id = $1")
                .bind(routing.routes[ROUTES - 1].job_id.0)
                .execute(&pool)
                .await
                .unwrap();
        }
        let mut held = Vec::new();
        for _ in 0..ROUTES {
            let job = claim(&pool, WorkerId::generate(), LONG).await.unwrap();
            held.push(job.unwrap());
        }

        let mut steps: Vec<Step> = (0..ROUTES).map(Step::Complete).collect();
        if one_fails {
            steps[ROUTES - 1] = Step::FailForGood(ROUTES - 1);
        }
        steps.push(Step::Cancel);
        steps.rotate_left(round as usize % (ROUTES + 1));

        let mut racing = JoinSet::new();
        for step in steps {
            let pool = pool.clone();
            let plan_id = routing.plan.id;
            let job = match step {
                Step::Complete(at) | Step::FailForGood(at) => Some(held[at].clone()),
                Step::Cancel => None,
            };
            racing.spawn(async move {
                match (step, job) {
                    (Step::Complete(_), Some(job)) => {
                        complete_route(&pool, &job).await;
                        false
                    }
                    (Step::FailForGood(_), Some(job)) => {
                        fail(&pool, &job.lease, "broken").await.unwrap();
                        false
                    }
                    _ => cancel_plan(&pool, plan_id).await.unwrap(),
                }
            });
        }
        let cancelled = soon(racing.join_all()).await.contains(&true);

        let plan = plans::get(&pool, routing.plan.id).await.unwrap().unwrap();
        let stored_routes = routes::list_for_plan(&pool, plan.id).await.unwrap();
        let jobs = statuses(&pool, &routing).await;
        assert!(
            jobs.iter()
                .all(|status| !matches!(status, JobStatus::Queued | JobStatus::Running)),
            "round {round}: {jobs:?}"
        );
        match plan.status {
            PlanStatus::Done => {
                assert!(!one_fails && !cancelled, "round {round}");
                assert_eq!(stored_routes.len(), ROUTES, "round {round}");
                assert_eq!(jobs, [JobStatus::Done; ROUTES], "round {round}");
                assert!(!orders_are_free(&pool, &routing).await, "round {round}");
            }
            PlanStatus::Cancelled | PlanStatus::Failed => {
                assert_eq!(
                    cancelled,
                    plan.status == PlanStatus::Cancelled,
                    "round {round}"
                );
                assert!(stored_routes.is_empty(), "round {round}");
                assert!(orders_are_free(&pool, &routing).await, "round {round}");
            }
            status => panic!("round {round}: the plan is still {status}"),
        }
    }
}

/// A plan as its last route job leaves it: done, with every route stored.
async fn done_plan(pool: &PgPool, day: u32, routes: usize, stops: usize) -> Routing {
    let routing = testing::routing(pool, date(day), routes, stops).await;
    let mut status = None;
    for _ in 0..routes {
        let job = claim(pool, WorkerId::generate(), LONG).await.unwrap();
        status = complete_route(pool, &job.unwrap()).await;
    }
    assert_eq!(status, Some(PlanStatus::Done));
    routing
}

#[sqlx::test]
async fn discarding_a_done_plan_frees_its_orders_and_keeps_the_row(pool: PgPool) {
    let routing = done_plan(&pool, 12, 3, 2).await;
    let done = plans::get(&pool, routing.plan.id).await.unwrap().unwrap();
    assert!(!orders_are_free(&pool, &routing).await);

    assert!(discard_plan(&pool, routing.plan.id).await.unwrap());

    let plan = plans::get(&pool, routing.plan.id).await.unwrap().unwrap();
    assert_eq!((plan.status, &plan.error), (PlanStatus::Discarded, &None));
    // What the plan had come to stays on record, and so does when it was done.
    assert_eq!(
        (plan.orders_total, plan.routes_total, plan.routes_done),
        (6, 3, 3)
    );
    assert_eq!(plan.finished_at, done.finished_at);

    assert!(
        routes::list_for_plan(&pool, routing.plan.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(orders_are_free(&pool, &routing).await);
    // Its jobs are history. None of them comes back to the queue.
    assert_eq!(statuses(&pool, &routing).await, [JobStatus::Done; 3]);
    assert_eq!(
        claim(&pool, WorkerId::generate(), LONG).await.unwrap(),
        None
    );

    // Discarding is done once.
    assert!(!discard_plan(&pool, routing.plan.id).await.unwrap());
}

#[sqlx::test]
async fn only_a_plan_that_is_done_can_be_discarded(pool: PgPool) {
    let status = async |plan_id: PlanId| {
        let plan = plans::get(&pool, plan_id).await.unwrap().unwrap();
        plan.status
    };

    // Queued.
    let queued = testing::plan(&pool, date(12)).await;
    assert!(!discard_plan(&pool, queued.id).await.unwrap());
    assert_eq!(status(queued.id).await, PlanStatus::Queued);

    // Routing, with one of its two routes stored. That is for a cancel to undo.
    let routing = testing::routing(&pool, date(13), 2, 2).await;
    let first = claim(&pool, WorkerId::generate(), LONG)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        complete_route(&pool, &first).await,
        Some(PlanStatus::Routing)
    );
    assert!(!discard_plan(&pool, routing.plan.id).await.unwrap());
    assert_eq!(status(routing.plan.id).await, PlanStatus::Routing);
    assert_eq!(
        routes::list_for_plan(&pool, routing.plan.id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(!orders_are_free(&pool, &routing).await);

    // Cancelled.
    assert!(cancel_plan(&pool, routing.plan.id).await.unwrap());
    assert!(!discard_plan(&pool, routing.plan.id).await.unwrap());
    assert_eq!(status(routing.plan.id).await, PlanStatus::Cancelled);

    // Failed.
    let failing = testing::routing(&pool, date(14), 1, 1).await;
    sqlx::query("UPDATE jobs SET max_attempts = 1 WHERE id = $1")
        .bind(failing.routes[0].job_id.0)
        .execute(&pool)
        .await
        .unwrap();
    let job = claim(&pool, WorkerId::generate(), LONG)
        .await
        .unwrap()
        .unwrap();
    assert!(fail(&pool, &job.lease, "broken").await.unwrap());
    assert!(!discard_plan(&pool, failing.plan.id).await.unwrap());
    assert_eq!(status(failing.plan.id).await, PlanStatus::Failed);

    // And a plan that does not exist.
    assert!(!discard_plan(&pool, PlanId::generate()).await.unwrap());
}

/// What a discard is for (D30): the date is planned again, and the new plan gets
/// every order of it.
#[sqlx::test]
async fn a_discarded_date_is_planned_afresh_with_every_order(pool: PgPool) {
    let first = done_plan(&pool, 12, 2, 3).await;
    let mut all = first.order_ids();
    all.sort_unstable();
    let pending = async || {
        let mut tx = pool.begin().await.unwrap();
        let locked = orders::lock_pending(&mut tx, date(12)).await.unwrap();
        tx.rollback().await.unwrap();
        locked.into_iter().map(|order| order.id).collect::<Vec<_>>()
    };

    // While the plan stands, a later plan is offered none of its orders (D20).
    assert!(pending().await.is_empty());

    assert!(discard_plan(&pool, first.plan.id).await.unwrap());
    assert_eq!(pending().await, all);

    // The new plan can put them on routes. The old stops went with the old routes,
    // so no order is on two routes.
    let again = testing::plan(&pool, date(12)).await;
    let courier = testing::courier(&pool, 100).await;
    assert_eq!(
        orders::mark_planned(&pool, again.id, &all).await.unwrap(),
        6
    );
    let route = NewRoute {
        plan_id: again.id,
        courier_id: courier.id,
        distance_m: 1_000,
        stops: all.clone(),
    };
    routes::insert(&pool, &route).await.unwrap();

    let listed = routes::list_for_plan(&pool, again.id).await.unwrap();
    assert_eq!(listed[0].stops.len(), 6);
    let discarded = plans::get(&pool, first.plan.id).await.unwrap().unwrap();
    assert_eq!(discarded.status, PlanStatus::Discarded);
    assert!(
        routes::list_for_plan(&pool, first.plan.id)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Two operators press the button at the same moment.
#[sqlx::test]
async fn discards_at_the_same_moment_succeed_once(pool: PgPool) {
    let routing = done_plan(&pool, 12, 2, 2).await;

    let mut racing = JoinSet::new();
    for _ in 0..4 {
        let pool = pool.clone();
        let plan_id = routing.plan.id;
        racing.spawn(async move { discard_plan(&pool, plan_id).await.unwrap() });
    }
    let discarded: Vec<bool> = soon(racing.join_all()).await;

    assert_eq!(discarded.iter().filter(|&&did| did).count(), 1);
    assert!(orders_are_free(&pool, &routing).await);
}

#[sqlx::test]
async fn the_depth_of_the_queue_counts_the_jobs_of_each_status(pool: PgPool) {
    assert_eq!(depth(&pool).await.unwrap(), QueueDepth::default());

    let routing = testing::routing(&pool, date(12), 4, 1).await;
    let finished = claim(&pool, WorkerId::generate(), LONG)
        .await
        .unwrap()
        .unwrap();
    complete_route(&pool, &finished).await.unwrap();
    claim(&pool, WorkerId::generate(), LONG)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        depth(&pool).await.unwrap(),
        QueueDepth {
            queued: 2,
            running: 1,
            done: 1,
            failed: 0,
            cancelled: 0,
        }
    );

    assert!(cancel_plan(&pool, routing.plan.id).await.unwrap());
    let failing = testing::routing(&pool, date(13), 1, 1).await;
    sqlx::query("UPDATE jobs SET max_attempts = 1 WHERE id = $1")
        .bind(failing.routes[0].job_id.0)
        .execute(&pool)
        .await
        .unwrap();
    let job = claim(&pool, WorkerId::generate(), LONG)
        .await
        .unwrap()
        .unwrap();
    assert!(fail(&pool, &job.lease, "broken").await.unwrap());
    assert_eq!(
        depth(&pool).await.unwrap(),
        QueueDepth {
            queued: 0,
            running: 0,
            done: 1,
            failed: 1,
            cancelled: 3,
        }
    );
}

/// Someone who watches plans, as the API does for its event streams (D48).
async fn watching(pool: &PgPool) -> PgListener {
    let mut watcher = PgListener::connect_with(pool).await.unwrap();
    watcher.listen(PLANS_CHANNEL).await.unwrap();
    watcher
}

/// The plans a watcher was told of since it last looked. It waits until nothing more
/// has come for a while, so that one word too many shows up as well.
async fn told_of(watcher: &mut PgListener) -> Vec<PlanId> {
    let silence = Duration::from_millis(300);
    let mut plans = Vec::new();
    while let Ok(notification) = tokio::time::timeout(silence, watcher.recv()).await {
        let notification = notification.unwrap();
        assert_eq!(notification.channel(), PLANS_CHANNEL);
        plans.push(notification.payload().parse().unwrap());
    }
    plans
}

/// Every change to a plan or to one of its jobs is announced, once, and when it can be
/// read: at the commit of the transaction it was made in.
#[sqlx::test]
async fn a_watcher_hears_of_every_change_to_a_plan_once_it_is_made(pool: PgPool) {
    let new = testing::new_plan(&pool, date(12)).await;
    let mut watcher = watching(&pool).await;
    let worker = WorkerId::generate();

    // Submitted. The plan and its job are one transaction, and one word.
    let plan = plans::submit(&pool, &new, None).await.unwrap();
    assert_eq!(told_of(&mut watcher).await, [plan.id]);

    let job = claim(&pool, worker, LONG).await.unwrap().unwrap();
    assert_eq!(told_of(&mut watcher).await, [plan.id], "claimed");

    // A heartbeat changes nothing that a watcher shows.
    heartbeat(&pool, &job.lease, LONG, None).await.unwrap();
    assert!(told_of(&mut watcher).await.is_empty(), "a heartbeat");

    // Clustered: twenty route jobs, the plan moved on and its job done. Nothing of it
    // is said before the commit, and afterwards it is said once.
    let mut completion = begin_complete(&pool, &job.lease).await.unwrap().unwrap();
    for seed in 0..20 {
        let route = JobPayload::Route {
            plan_id: plan.id,
            courier_id: CourierId::generate(),
            order_ids: Vec::new(),
            seed,
        };
        enqueue(completion.conn(), &route, None).await.unwrap();
    }
    let counts = PlanCounts {
        orders_total: 0,
        orders_unassigned: 0,
        routes_total: 20,
    };
    plans::start_routing(completion.conn(), plan.id, counts)
        .await
        .unwrap();
    assert!(told_of(&mut watcher).await.is_empty(), "before the commit");
    completion.commit().await.unwrap();
    assert_eq!(told_of(&mut watcher).await, [plan.id], "clustered");

    // A completion that is dropped has changed nothing, and says nothing.
    let route = claim(&pool, worker, LONG).await.unwrap().unwrap();
    assert_eq!(told_of(&mut watcher).await, [plan.id]);
    let mut completion = begin_complete(&pool, &route.lease).await.unwrap().unwrap();
    plans::route_finished(completion.conn(), plan.id)
        .await
        .unwrap();
    drop(completion);
    assert!(told_of(&mut watcher).await.is_empty(), "rolled back");

    // An attempt that failed and will be tried again.
    assert!(fail(&pool, &route.lease, "out of memory").await.unwrap());
    assert_eq!(told_of(&mut watcher).await, [plan.id], "retried");
    // What changes nothing says nothing: the job is no longer that worker's.
    assert!(!fail(&pool, &route.lease, "once more").await.unwrap());
    assert!(!release(&pool, &route.lease).await.unwrap());
    assert!(told_of(&mut watcher).await.is_empty());

    // A job given back by a worker that is stopping.
    let other = claim(&pool, worker, LONG).await.unwrap().unwrap();
    assert_eq!(told_of(&mut watcher).await, [plan.id]);
    assert!(release(&pool, &other.lease).await.unwrap());
    assert_eq!(told_of(&mut watcher).await, [plan.id], "released");

    // Cancelled: the plan, its jobs and its orders, in one word.
    assert!(cancel_plan(&pool, plan.id).await.unwrap());
    assert_eq!(told_of(&mut watcher).await, [plan.id], "cancelled");
    assert!(!cancel_plan(&pool, plan.id).await.unwrap());
    assert!(told_of(&mut watcher).await.is_empty());
}

#[sqlx::test]
async fn a_watcher_hears_how_a_plan_ends(pool: PgPool) {
    let last_attempt = async |routing: &Routing| {
        sqlx::query("UPDATE jobs SET max_attempts = 1 WHERE plan_id = $1")
            .bind(routing.plan.id.0)
            .execute(&pool)
            .await
            .unwrap();
    };
    let mut watcher = watching(&pool).await;

    // Done with its last route, and then discarded.
    let done = testing::routing(&pool, date(12), 1, 1).await;
    let job = claim(&pool, WorkerId::generate(), LONG).await.unwrap();
    told_of(&mut watcher).await;
    assert_eq!(
        complete_route(&pool, &job.unwrap()).await,
        Some(PlanStatus::Done)
    );
    assert_eq!(told_of(&mut watcher).await, [done.plan.id], "done");
    assert!(discard_plan(&pool, done.plan.id).await.unwrap());
    assert_eq!(told_of(&mut watcher).await, [done.plan.id], "discarded");
    assert!(!discard_plan(&pool, done.plan.id).await.unwrap());
    assert!(told_of(&mut watcher).await.is_empty());

    // Failed for good: the job, the plan and its other job, in one word.
    let failing = testing::routing(&pool, date(13), 2, 1).await;
    last_attempt(&failing).await;
    let job = claim(&pool, WorkerId::generate(), LONG)
        .await
        .unwrap()
        .unwrap();
    told_of(&mut watcher).await;
    assert!(fail(&pool, &job.lease, "broken").await.unwrap());
    assert_eq!(told_of(&mut watcher).await, [failing.plan.id], "failed");

    // Its worker died on the last attempt, and an idle worker buried the job.
    let abandoned = testing::routing(&pool, date(14), 1, 1).await;
    last_attempt(&abandoned).await;
    claim(&pool, WorkerId::generate(), Duration::ZERO)
        .await
        .unwrap()
        .unwrap();
    told_of(&mut watcher).await;
    assert_eq!(
        claim(&pool, WorkerId::generate(), LONG).await.unwrap(),
        None
    );
    assert_eq!(told_of(&mut watcher).await, [abandoned.plan.id], "buried");
    let plan = plans::get(&pool, abandoned.plan.id).await.unwrap().unwrap();
    assert_eq!(plan.status, PlanStatus::Failed);
}

/// The watchers have a channel of their own. On the channel of the workers, every claim
/// and every stored route would wake every worker, to find a queue it has just looked at.
#[sqlx::test]
async fn what_the_watchers_are_told_does_not_wake_the_workers(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 3, 1).await;
    let mut workers = PgListener::connect_with(&pool).await.unwrap();
    workers.listen(JOBS_CHANNEL).await.unwrap();
    let worker = WorkerId::generate();

    let job = claim(&pool, worker, LONG).await.unwrap().unwrap();
    let progress = JobProgress {
        iterations: 16_384,
        best_cost_m: 4_200,
    };
    heartbeat(&pool, &job.lease, LONG, Some(progress))
        .await
        .unwrap();
    announce_progress(&pool, &job.lease, progress, Some(&[0]))
        .await
        .unwrap();
    assert_eq!(complete_route(&pool, &job).await, Some(PlanStatus::Routing));
    let job = claim(&pool, worker, LONG).await.unwrap().unwrap();
    assert!(fail(&pool, &job.lease, "out of memory").await.unwrap());
    assert!(cancel_plan(&pool, routing.plan.id).await.unwrap());

    let silence = Duration::from_millis(300);
    assert!(
        tokio::time::timeout(silence, workers.recv()).await.is_err(),
        "a worker was woken for something it has no use for"
    );
}

#[sqlx::test]
async fn progress_is_announced_with_the_claim_it_comes_from_and_stored_nowhere(pool: PgPool) {
    let routing = testing::routing(&pool, date(12), 1, 3).await;
    let worker = WorkerId::generate();
    // Claimed twice, so that the attempt is not the 1 it would be by accident.
    let first = claim(&pool, worker, Duration::ZERO).await.unwrap().unwrap();
    let job = claim(&pool, worker, LONG).await.unwrap().unwrap();
    assert_eq!((first.lease.attempt, job.lease.attempt), (1, 2));

    let mut listener = PgListener::connect_with(&pool).await.unwrap();
    listener.listen(PROGRESS_CHANNEL).await.unwrap();
    let mut said = async || -> RouteProgress {
        let notification = soon(listener.recv()).await.unwrap();
        assert_eq!(notification.channel(), PROGRESS_CHANNEL);
        serde_json::from_str(notification.payload()).unwrap()
    };
    let progress = JobProgress {
        iterations: 32_768,
        best_cost_m: 4_200,
    };

    announce_progress(&pool, &job.lease, progress, Some(&[2, 0, 1]))
        .await
        .unwrap();
    assert_eq!(
        said().await,
        RouteProgress {
            plan_id: routing.plan.id,
            job_id: job.lease.job_id,
            attempt: 2,
            iterations: 32_768,
            best_cost_m: 4_200,
            order: Some(vec![2, 0, 1]),
        }
    );

    // The worker that lost the job can still be heard, and is told apart by its attempt.
    announce_progress(&pool, &first.lease, progress, None)
        .await
        .unwrap();
    let stale = said().await;
    assert_eq!((stale.attempt, stale.order), (1, None));

    // What a job has stored is what its last heartbeat said, and it has sent none.
    assert_eq!(stored(&pool, job.lease.job_id).await.progress, None);
}

/// A notification has a longest payload, and a route of thousands of stops is longer.
#[sqlx::test]
async fn a_route_too_long_for_a_notification_is_announced_by_its_numbers(pool: PgPool) {
    // The limit is the one PostgreSQL has, to the byte.
    let notify = async |length: usize| {
        sqlx::query("SELECT pg_notify('routeforge_test', repeat('x', $1))")
            .bind(length as i32)
            .execute(&pool)
            .await
    };
    notify(LONGEST_PAYLOAD).await.unwrap();
    let error = notify(LONGEST_PAYLOAD + 1).await.unwrap_err();
    assert!(error.to_string().contains("too long"), "{error}");

    testing::routing(&pool, date(12), 1, 1).await;
    let job = claim(&pool, WorkerId::generate(), LONG)
        .await
        .unwrap()
        .unwrap();
    let progress = JobProgress {
        iterations: u64::MAX,
        best_cost_m: u64::MAX,
    };
    let in_order = |stops: u32| -> Vec<u32> { (0..stops).collect() };
    let fits = |stops: u32| {
        let announced = RouteProgress {
            plan_id: job.lease.plan_id,
            job_id: job.lease.job_id,
            attempt: job.lease.attempt,
            iterations: progress.iterations,
            best_cost_m: progress.best_cost_m,
            order: Some(in_order(stops)),
        };
        notification(&announced).unwrap().len() <= LONGEST_PAYLOAD
    };
    let most = (1..10_000).take_while(|&stops| fits(stops)).last().unwrap();
    assert!(most > 1_000, "only {most} stops fit");

    let mut listener = PgListener::connect_with(&pool).await.unwrap();
    listener.listen(PROGRESS_CHANNEL).await.unwrap();
    let mut said = async |stops: u32| -> RouteProgress {
        announce_progress(&pool, &job.lease, progress, Some(&in_order(stops)))
            .await
            .unwrap();
        let notification = soon(listener.recv()).await.unwrap();
        serde_json::from_str(notification.payload()).unwrap()
    };

    // The longest route that fits is announced whole.
    assert_eq!(said(most).await.order, Some(in_order(most)));
    // One stop more, and the route is left out. The numbers are still said.
    let numbers = said(most + 1).await;
    assert_eq!(numbers.order, None);
    assert_eq!(
        (numbers.job_id, numbers.iterations, numbers.best_cost_m),
        (job.lease.job_id, u64::MAX, u64::MAX)
    );
    assert_eq!(said(3 * most).await.order, None);
}
