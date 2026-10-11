//! The job queue (D6). How it is built, and why, is in D22 to D26.
//!
//! A job is a row in `jobs`. Workers take jobs themselves, so nothing hands them out:
//!
//! 1. [`enqueue`] stores a job and notifies [`JOBS_CHANNEL`].
//! 2. [`claim`] gives the oldest job that is due to one worker, under a lease.
//! 3. While it works, the worker calls [`heartbeat`] to keep the lease and to learn
//!    whether it should stop.
//! 4. It ends with [`begin_complete`] and [`Completion::commit`], with [`fail`], or,
//!    when it is shutting down, with [`release`].
//!
//! # What the queue guarantees
//!
//! Delivery is at least once. A worker that dies stops sending heartbeats, its lease
//! expires, and another worker claims the job. So a slow worker and its replacement can
//! both be working on one job.
//!
//! Results are stored once all the same. Every claim increments the job's `attempt`,
//! and a [`Lease`] carries the value it was claimed with. Each call a worker makes
//! changes the job only while the job is running for that worker with that attempt
//! number. The results are written in the transaction that marks the job done, so the
//! worker that lost the job writes nothing.
//!
//! A lapsed lease alone does not shut a worker out. Until someone else claims the job
//! it is still the one meant to do it, and its heartbeat or completion is accepted.
//!
//! # Clocks
//!
//! A worker passes spans of time, how long a lease should run, and never a point in
//! time. Every timestamp is the database's `now()`, so the clocks of the workers do not
//! matter, and no call returns a timestamp a worker could compare with its own.
//!
//! # Plans
//!
//! Every job belongs to a plan, and three calls change the plan together with its jobs:
//! a completion, through what the caller writes; [`fail`], when the last attempt has
//! failed; and [`cancel_plan`]. A plan that ends as failed or cancelled takes its
//! outstanding jobs with it, deletes the routes it had stored and gives its orders
//! back as pending, all in one transaction.
//!
//! A plan that is done has no job left. [`discard_plan`] takes such a plan back: the
//! same undo, for a plan that got to its end (D30).
//!
//! # Watchers
//!
//! Whoever shows a plan to a person, which is the API with its event streams, learns of
//! a change the way a worker learns of a job. Every function here that changes a plan
//! or one of its jobs notifies [`PLANS_CHANNEL`] with the plan's id, in the statement
//! or the transaction of the change. So the change is heard when it can be read, and
//! not before. What changed is not said: a watcher reads the plan again (D48).
//!
//! [`announce_progress`] is the one thing that is sent and not stored: how far the
//! search for a route has got, a few times a second, on [`PROGRESS_CHANNEL`].
//!
//! # Lock order
//!
//! Those three lock the plan row before any job row. With the job first, a completing
//! route would hold its job and wait for the plan while a cancel held the plan and
//! waited for that job. [`claim`], [`heartbeat`], [`release`] and the retry of [`fail`]
//! lock a single job row in a single statement, so they cannot be part of a cycle.
//! [`discard_plan`] locks the plan row and no job.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::{PgConnection, PgExecutor, PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::{DbError, millis, parsed, unsigned};
use crate::ids::{JobId, PlanId, WorkerId};
use crate::job::{Job, JobPayload, JobProgress, JobStatus, QueueDepth, RouteProgress};
use crate::model::PlanStatus;

/// The channel a worker listens on to learn that a job may be waiting.
///
/// The payload is the kind of the job. A notification is a hint and no more: one can
/// be missed while a listener reconnects, and a job that becomes due because its retry
/// time has come or its lease has expired sends none. Workers poll as well.
pub const JOBS_CHANNEL: &str = "routeforge_jobs";

/// The channel that says a plan has changed: the plan itself, or one of its jobs (D48).
///
/// The payload is the plan's id and nothing more, so the notifications of one
/// transaction are equal and PostgreSQL folds them into one (D26). Like a notification
/// on [`JOBS_CHANNEL`] it is a hint. Whoever hears it reads the plan again, and whoever
/// may have missed one reads again anyway.
///
/// It is a channel of its own because the workers listen on the other. On theirs,
/// every change to a plan would wake every worker.
pub const PLANS_CHANNEL: &str = "routeforge_plans";

/// The channel a worker announces the progress of a route search on (D48).
///
/// The payload is a [`RouteProgress`] as JSON, sent by [`announce_progress`].
pub const PROGRESS_CHANNEL: &str = "routeforge_progress";

/// PostgreSQL refuses a notification whose payload is 8000 bytes or longer.
const LONGEST_PAYLOAD: usize = 7_999;

/// How many times a job may be claimed before a failure is final.
pub const MAX_ATTEMPTS: u32 = 3;

const RETRY_BASE: Duration = Duration::from_secs(2);
const RETRY_CAP: Duration = Duration::from_secs(60);

/// The error stored when a worker stopped responding during a job's last attempt.
const WORKER_LOST: &str = "the worker stopped responding during the last attempt";

/// How long a job waits after its `attempt`th attempt has failed: 2 s, then twice as
/// long each time, up to a minute.
pub fn retry_delay(attempt: u32) -> Duration {
    let doublings = attempt.saturating_sub(1).min(31);
    RETRY_BASE.saturating_mul(1 << doublings).min(RETRY_CAP)
}

/// A worker's hold on a job. [`claim`] hands it out and every later call shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lease {
    pub job_id: JobId,
    pub plan_id: PlanId,
    pub worker_id: WorkerId,
    /// Which claim of the job this is, counted from 1. This is the fencing token: once
    /// the job has been claimed again the number no longer matches, and the calls of
    /// the earlier claim change nothing.
    pub attempt: u32,
    pub max_attempts: u32,
}

impl Lease {
    /// Whether a failure of this attempt is final.
    pub fn is_last_attempt(&self) -> bool {
        self.attempt >= self.max_attempts
    }
}

/// A job as [`claim`] hands it to a worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimedJob {
    pub lease: Lease,
    pub payload: JobPayload,
    /// The id of the HTTP request that asked for the plan, for tracing.
    pub request_id: Option<String>,
}

/// What a [`heartbeat`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Heartbeat {
    /// The lease runs for the span asked for, from now.
    Extended,
    /// The job's plan was cancelled or has failed. The worker should stop.
    Cancelled,
    /// The job is no longer this worker's: another worker has claimed it, or it is
    /// finished. The worker should stop, and whatever it has computed is dropped.
    Lost,
}

/// Puts a job on the queue, due at once, and notifies the workers and the watchers.
///
/// It is one statement, and the notifications are sent when the transaction it runs in
/// commits. Inside a caller's transaction the job appears, and the workers hear of it,
/// exactly when the caller's other changes do.
pub async fn enqueue(
    executor: impl PgExecutor<'_>,
    payload: &JobPayload,
    request_id: Option<&str>,
) -> Result<JobId, DbError> {
    let id = JobId::generate();
    sqlx::query!(
        "WITH inserted AS (
             INSERT INTO jobs (id, plan_id, kind, payload, max_attempts, request_id)
             VALUES ($1, $2, $3, $4, $5, $6)
             RETURNING kind, plan_id
         )
         SELECT pg_notify($7, kind) AS workers, pg_notify($8, plan_id::text) AS watchers
         FROM inserted",
        id.0,
        payload.plan_id().0,
        payload.kind().as_str(),
        Json(payload) as _,
        i64::from(MAX_ATTEMPTS),
        request_id,
        JOBS_CHANNEL,
        PLANS_CHANNEL,
    )
    .execute(executor)
    .await?;
    Ok(id)
}

/// Takes the oldest job that is due, for `worker`, under a lease of the given length.
///
/// A job is due when it is queued and its time has come, or when it is running and
/// its lease has expired. `None` means nothing is due.
///
/// Concurrent claims never return the same job and never wait for each other: a job
/// that another transaction has locked is skipped.
///
/// It is one statement, the word to the watchers included.
pub async fn claim(
    pool: &PgPool,
    worker: WorkerId,
    lease: Duration,
) -> Result<Option<ClaimedJob>, DbError> {
    let claimed = sqlx::query!(
        r#"WITH next AS (
               SELECT id
               FROM jobs
               WHERE (status = 'queued' AND run_at <= now())
                  OR (status = 'running' AND lease_expires_at <= now() AND attempt < max_attempts)
               ORDER BY run_at, id
               LIMIT 1
               FOR UPDATE SKIP LOCKED
           ),
           claimed AS (
               UPDATE jobs
               SET status = 'running',
                   attempt = jobs.attempt + 1,
                   claimed_by = $1,
                   lease_expires_at = now() + $2::bigint * interval '1 millisecond',
                   progress = NULL
               FROM next
               WHERE jobs.id = next.id
               RETURNING jobs.id, jobs.plan_id, jobs.payload, jobs.attempt, jobs.max_attempts,
                         jobs.request_id
           )
           SELECT id AS "id!", plan_id AS "plan_id!", payload AS "payload!",
                  attempt AS "attempt!", max_attempts AS "max_attempts!", request_id,
                  pg_notify($3, plan_id::text) AS watchers
           FROM claimed"#,
        worker.0,
        millis(lease),
        PLANS_CHANNEL,
    )
    .fetch_optional(pool)
    .await?;

    let Some(job) = claimed else {
        // Nothing to do, so there is time to tidy up.
        bury_exhausted(pool).await?;
        return Ok(None);
    };
    let lease = Lease {
        job_id: job.id.into(),
        plan_id: job.plan_id.into(),
        worker_id: worker,
        attempt: unsigned(job.attempt, "jobs.attempt")?,
        max_attempts: unsigned(job.max_attempts, "jobs.max_attempts")?,
    };
    match serde_json::from_value(job.payload) {
        Ok(payload) => Ok(Some(ClaimedJob {
            lease,
            payload,
            request_id: job.request_id,
        })),
        Err(error) => {
            // A job written by a newer build. This one cannot do it, and must not leave
            // it under a lease that nobody is working behind.
            let unreadable = format!(
                "the payload of job {} cannot be read: {error}",
                lease.job_id
            );
            fail(pool, &lease, &unreadable).await?;
            Err(DbError::Unreadable(unreadable))
        }
    }
}

/// Keeps a job for another `extend_by` from now, and stores how far it has got.
///
/// `progress` replaces what was stored; `None` leaves it as it is. The answer tells
/// the worker whether to go on.
pub async fn heartbeat(
    executor: impl PgExecutor<'_>,
    lease: &Lease,
    extend_by: Duration,
    progress: Option<JobProgress>,
) -> Result<Heartbeat, DbError> {
    // The update and the look at the status are one statement and see one snapshot.
    let found = sqlx::query!(
        r#"WITH extended AS (
               UPDATE jobs
               SET lease_expires_at = now() + $4::bigint * interval '1 millisecond',
                   progress = COALESCE($5, progress)
               WHERE id = $1 AND status = 'running' AND claimed_by = $2 AND attempt = $3
               RETURNING id
           )
           SELECT status, EXISTS (SELECT 1 FROM extended) AS "extended!"
           FROM jobs
           WHERE id = $1"#,
        lease.job_id.0,
        lease.worker_id.0,
        i64::from(lease.attempt),
        millis(extend_by),
        progress.map(Json) as _,
    )
    .fetch_optional(executor)
    .await?;

    Ok(match found {
        Some(job) if job.extended => Heartbeat::Extended,
        Some(job) if job.status == JobStatus::Cancelled.as_str() => Heartbeat::Cancelled,
        _ => Heartbeat::Lost,
    })
}

/// The transaction that completes a job, open and waiting for the job's results.
///
/// Write the results through [`conn`](Self::conn), then [`commit`](Self::commit). The
/// results and the job being done become visible together. Dropping the completion
/// instead rolls everything back and leaves the job running under its lease.
#[must_use = "a completion that is not committed is rolled back"]
#[derive(Debug)]
pub struct Completion {
    tx: Transaction<'static, Postgres>,
    job_id: JobId,
    plan_id: PlanId,
}

impl Completion {
    /// The connection to write the job's results through.
    pub fn conn(&mut self) -> &mut PgConnection {
        &mut self.tx
    }

    /// Marks the job done and commits, together with everything written through
    /// [`conn`](Self::conn).
    pub async fn commit(mut self) -> Result<(), DbError> {
        sqlx::query!(
            "UPDATE jobs SET status = 'done', lease_expires_at = NULL, finished_at = now()
             WHERE id = $1",
            self.job_id.0,
        )
        .execute(&mut *self.tx)
        .await?;
        // One word for the job and for everything the caller wrote with it.
        announce(&mut self.tx, self.plan_id).await?;
        self.tx.commit().await?;
        Ok(())
    }
}

/// Starts completing a job: opens a transaction and locks the job in it.
///
/// `None` means the job is no longer this worker's, because another worker has claimed
/// it or its plan has ended. Nothing was changed, and the worker's results are to be
/// dropped.
///
/// From `Some` on the job cannot be taken away. The lock is on the row, so not even an
/// expired lease lets another worker claim it before this transaction ends.
pub async fn begin_complete(pool: &PgPool, lease: &Lease) -> Result<Option<Completion>, DbError> {
    let mut tx = pool.begin().await?;
    lock_plan(&mut tx, lease.plan_id).await?;
    let held = sqlx::query_scalar!(
        "SELECT id FROM jobs
         WHERE id = $1 AND status = 'running' AND claimed_by = $2 AND attempt = $3
         FOR UPDATE",
        lease.job_id.0,
        lease.worker_id.0,
        i64::from(lease.attempt),
    )
    .fetch_optional(&mut *tx)
    .await?;

    Ok(held.map(|_| Completion {
        tx,
        job_id: lease.job_id,
        plan_id: lease.plan_id,
    }))
}

/// Reports that an attempt has failed.
///
/// The job goes back on the queue and is due again after [`retry_delay`]. If this was
/// its last attempt the job is failed instead, and so is its plan: the plan's other
/// jobs are cancelled, its stored routes deleted and its orders pending again.
///
/// Returns `false`, and changes nothing, if the job is no longer this worker's.
pub async fn fail(pool: &PgPool, lease: &Lease, error: &str) -> Result<bool, DbError> {
    if !lease.is_last_attempt() {
        let retried = sqlx::query!(
            "WITH retried AS (
                 UPDATE jobs
                 SET status = 'queued',
                     run_at = now() + $4::bigint * interval '1 millisecond',
                     claimed_by = NULL,
                     lease_expires_at = NULL,
                     error = $5
                 WHERE id = $1 AND status = 'running' AND claimed_by = $2 AND attempt = $3
                 RETURNING plan_id
             )
             SELECT pg_notify($6, plan_id::text) FROM retried",
            lease.job_id.0,
            lease.worker_id.0,
            i64::from(lease.attempt),
            millis(retry_delay(lease.attempt)),
            error,
            PLANS_CHANNEL,
        )
        .execute(pool)
        .await?;
        return Ok(retried.rows_affected() == 1);
    }

    let mut tx = pool.begin().await?;
    lock_plan(&mut tx, lease.plan_id).await?;
    let failed = sqlx::query!(
        "UPDATE jobs
         SET status = 'failed', lease_expires_at = NULL, finished_at = now(), error = $4
         WHERE id = $1 AND status = 'running' AND claimed_by = $2 AND attempt = $3",
        lease.job_id.0,
        lease.worker_id.0,
        i64::from(lease.attempt),
        error,
    )
    .execute(&mut *tx)
    .await?;
    if failed.rows_affected() == 0 {
        return Ok(false);
    }
    end_plan(&mut tx, lease.plan_id, Ending::Failed(error)).await?;
    tx.commit().await?;
    Ok(true)
}

/// Hands a job back, for a worker that is shutting down.
///
/// The job is due at once, keeps its place in the queue, and the attempt is not
/// counted. The workers are notified, and so are the watchers.
///
/// Returns `false`, and changes nothing, if the job is no longer this worker's.
pub async fn release(executor: impl PgExecutor<'_>, lease: &Lease) -> Result<bool, DbError> {
    let released = sqlx::query!(
        "WITH released AS (
             UPDATE jobs
             SET status = 'queued', attempt = attempt - 1, claimed_by = NULL, lease_expires_at = NULL
             WHERE id = $1 AND status = 'running' AND claimed_by = $2 AND attempt = $3
             RETURNING kind, plan_id
         )
         SELECT pg_notify($4, kind) AS workers, pg_notify($5, plan_id::text) AS watchers
         FROM released",
        lease.job_id.0,
        lease.worker_id.0,
        i64::from(lease.attempt),
        JOBS_CHANNEL,
        PLANS_CHANNEL,
    )
    .execute(executor)
    .await?;
    Ok(released.rows_affected() == 1)
}

/// Cancels a plan that is queued or routing.
///
/// In one transaction the plan and its outstanding jobs become cancelled, the routes
/// it had stored are deleted and its orders are pending again. A worker in the middle
/// of one of the jobs learns it at its next [`heartbeat`], and cannot complete the job.
///
/// Returns `false`, and changes nothing, if the plan is over already or does not exist.
pub async fn cancel_plan(pool: &PgPool, plan_id: PlanId) -> Result<bool, DbError> {
    let mut tx = pool.begin().await?;
    // The first thing `end_plan` does is update the plan row, which locks it.
    let cancelled = end_plan(&mut tx, plan_id, Ending::Cancelled).await?;
    tx.commit().await?;
    Ok(cancelled)
}

/// Discards a plan that is done (D30).
///
/// In one transaction the plan becomes discarded, its routes are deleted and its
/// orders are pending again, as after a plan that failed or was cancelled. The date
/// can then be planned afresh, and the new plan takes every pending order of it.
///
/// A plan that is done has no job on the queue, so no worker has to be told.
///
/// Returns `false`, and changes nothing, if the plan is not done or does not exist. A
/// plan that is still queued or routing is cancelled instead: see [`cancel_plan`].
pub async fn discard_plan(pool: &PgPool, plan_id: PlanId) -> Result<bool, DbError> {
    let mut tx = pool.begin().await?;
    // The update locks the plan row, as every operation on a plan does first.
    let discarded = sqlx::query!(
        "UPDATE plans SET status = 'discarded' WHERE id = $1 AND status = 'done'",
        plan_id.0,
    )
    .execute(&mut *tx)
    .await?;
    if discarded.rows_affected() == 0 {
        return Ok(false);
    }
    undo_plan(&mut tx, plan_id).await?;
    announce(&mut tx, plan_id).await?;
    tx.commit().await?;
    Ok(true)
}

/// Announces how far the search for a route has got, to whoever watches its plan.
///
/// `order` is the best route so far: its stops in calling order, each as its position
/// in the job's list of orders. It is left out when the notification would be longer
/// than PostgreSQL takes, which a route of well over a thousand stops is. The numbers
/// are announced all the same.
///
/// Nothing is stored and nothing is checked, so a worker that has lost the job can
/// still announce. What is announced says which attempt it comes from, and a watcher
/// tells by that (D49).
pub async fn announce_progress(
    executor: impl PgExecutor<'_>,
    lease: &Lease,
    progress: JobProgress,
    order: Option<&[u32]>,
) -> Result<(), DbError> {
    let mut announced = RouteProgress {
        plan_id: lease.plan_id,
        job_id: lease.job_id,
        attempt: lease.attempt,
        iterations: progress.iterations,
        best_cost_m: progress.best_cost_m,
        order: order.map(<[u32]>::to_vec),
    };
    let mut payload = notification(&announced)?;
    if payload.len() > LONGEST_PAYLOAD {
        announced.order = None;
        payload = notification(&announced)?;
    }
    sqlx::query!("SELECT pg_notify($1, $2)", PROGRESS_CHANNEL, payload)
        .execute(executor)
        .await?;
    Ok(())
}

/// A progress report as the text of a notification.
fn notification(progress: &RouteProgress) -> Result<String, DbError> {
    serde_json::to_string(progress)
        .map_err(|error| DbError::Sqlx(sqlx::Error::Encode(error.into())))
}

struct JobRow {
    id: Uuid,
    plan_id: Uuid,
    payload: serde_json::Value,
    status: String,
    attempt: i64,
    max_attempts: i64,
    run_at: DateTime<Utc>,
    claimed_by: Option<Uuid>,
    lease_expires_at: Option<DateTime<Utc>>,
    progress: Option<serde_json::Value>,
    error: Option<String>,
    request_id: Option<String>,
    created_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

impl TryFrom<JobRow> for Job {
    type Error = DbError;

    fn try_from(row: JobRow) -> Result<Self, DbError> {
        let unreadable = |what: &str, error: serde_json::Error| {
            DbError::Unreadable(format!(
                "the {what} of job {} cannot be read: {error}",
                row.id
            ))
        };
        Ok(Self {
            id: row.id.into(),
            plan_id: row.plan_id.into(),
            payload: serde_json::from_value(row.payload)
                .map_err(|error| unreadable("payload", error))?,
            status: parsed(&row.status)?,
            attempt: unsigned(row.attempt, "jobs.attempt")?,
            max_attempts: unsigned(row.max_attempts, "jobs.max_attempts")?,
            run_at: row.run_at,
            claimed_by: row.claimed_by.map(WorkerId),
            lease_expires_at: row.lease_expires_at,
            progress: row
                .progress
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| unreadable("progress", error))?,
            error: row.error,
            request_id: row.request_id,
            created_at: row.created_at,
            finished_at: row.finished_at,
        })
    }
}

/// A job as it is stored now.
pub async fn get(executor: impl PgExecutor<'_>, id: JobId) -> Result<Option<Job>, DbError> {
    sqlx::query_as!(
        JobRow,
        "SELECT id, plan_id, payload, status, attempt, max_attempts, run_at, claimed_by,
                lease_expires_at, progress, error, request_id, created_at, finished_at
         FROM jobs
         WHERE id = $1",
        id.0,
    )
    .fetch_optional(executor)
    .await?
    .map(Job::try_from)
    .transpose()
}

/// The jobs of a plan, oldest first: its `plan` job, then its route jobs.
pub async fn list_for_plan(
    executor: impl PgExecutor<'_>,
    plan_id: PlanId,
) -> Result<Vec<Job>, DbError> {
    sqlx::query_as!(
        JobRow,
        "SELECT id, plan_id, payload, status, attempt, max_attempts, run_at, claimed_by,
                lease_expires_at, progress, error, request_id, created_at, finished_at
         FROM jobs
         WHERE plan_id = $1
         ORDER BY id",
        plan_id.0,
    )
    .fetch_all(executor)
    .await?
    .into_iter()
    .map(Job::try_from)
    .collect()
}

/// How many jobs stand in each status: the depth of the queue, and what became of the
/// jobs that have left it.
pub async fn depth(executor: impl PgExecutor<'_>) -> Result<QueueDepth, DbError> {
    let rows = sqlx::query!(r#"SELECT status, count(*) AS "jobs!" FROM jobs GROUP BY status"#)
        .fetch_all(executor)
        .await?;

    let mut depth = QueueDepth::default();
    for row in rows {
        // A count is never negative.
        let jobs = row.jobs.unsigned_abs();
        match parsed(&row.status)? {
            JobStatus::Queued => depth.queued = jobs,
            JobStatus::Running => depth.running = jobs,
            JobStatus::Done => depth.done = jobs,
            JobStatus::Failed => depth.failed = jobs,
            JobStatus::Cancelled => depth.cancelled = jobs,
        }
    }
    Ok(depth)
}

/// Locks a plan row until the transaction ends. See the lock order in the module docs.
async fn lock_plan(conn: &mut PgConnection, plan_id: PlanId) -> Result<(), DbError> {
    sqlx::query!("SELECT id FROM plans WHERE id = $1 FOR UPDATE", plan_id.0)
        .fetch_optional(conn)
        .await?;
    Ok(())
}

/// How a plan ends without being done.
#[derive(Clone, Copy)]
enum Ending<'a> {
    Cancelled,
    /// With the error of the job that ran out of attempts.
    Failed(&'a str),
}

/// Ends a plan that is queued or routing, and undoes what it had done so far.
///
/// The plan row is locked first, by this function's own update if the caller has not
/// locked it already. Returns `false`, and changes nothing, if the plan was over.
async fn end_plan(
    conn: &mut PgConnection,
    plan_id: PlanId,
    ending: Ending<'_>,
) -> Result<bool, DbError> {
    let (status, error) = match ending {
        Ending::Cancelled => (PlanStatus::Cancelled, None),
        Ending::Failed(error) => (PlanStatus::Failed, Some(error)),
    };
    let ended = sqlx::query!(
        "UPDATE plans SET status = $2, error = $3, finished_at = now()
         WHERE id = $1 AND status IN ('queued', 'routing')",
        plan_id.0,
        status.as_str(),
        error,
    )
    .execute(&mut *conn)
    .await?;
    if ended.rows_affected() == 0 {
        return Ok(false);
    }

    // A worker holding one of these finds out at its next heartbeat.
    sqlx::query!(
        "UPDATE jobs SET status = 'cancelled', lease_expires_at = NULL, finished_at = now()
         WHERE plan_id = $1 AND status IN ('queued', 'running')",
        plan_id.0,
    )
    .execute(&mut *conn)
    .await?;
    undo_plan(&mut *conn, plan_id).await?;
    announce(conn, plan_id).await?;
    Ok(true)
}

/// Tells whoever watches the plan that it, or one of its jobs, has changed (D48).
///
/// The id is turned into text by PostgreSQL, here as in the statements that notify by
/// themselves. So the notifications of one transaction are equal to the letter, and
/// fold into one.
async fn announce(conn: &mut PgConnection, plan_id: PlanId) -> Result<(), DbError> {
    sqlx::query!(
        "SELECT pg_notify($1, $2::uuid::text)",
        PLANS_CHANNEL,
        plan_id.0,
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Takes back what a plan has done: deletes the routes it has stored and makes its
/// orders pending again (D20). The caller holds the lock on the plan row.
async fn undo_plan(conn: &mut PgConnection, plan_id: PlanId) -> Result<(), DbError> {
    // The stops go with their routes.
    sqlx::query!("DELETE FROM routes WHERE plan_id = $1", plan_id.0)
        .execute(&mut *conn)
        .await?;
    sqlx::query!(
        "UPDATE orders SET status = 'pending', plan_id = NULL WHERE plan_id = $1",
        plan_id.0,
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Fails the jobs whose worker stopped responding during their last attempt, and
/// their plans with them.
///
/// Such a job cannot be claimed again, and nobody is left to report it. An idle worker
/// does it on its way out of [`claim`], so the queue needs no process of its own for it.
async fn bury_exhausted(pool: &PgPool) -> Result<(), DbError> {
    let dead = sqlx::query!(
        "SELECT id, plan_id FROM jobs
         WHERE status = 'running' AND lease_expires_at <= now() AND attempt >= max_attempts",
    )
    .fetch_all(pool)
    .await?;

    for job in dead {
        let plan_id = PlanId(job.plan_id);
        let mut tx = pool.begin().await?;
        lock_plan(&mut tx, plan_id).await?;
        // Checked again under the lock: the worker may have come back, or another idle
        // worker may have been here first.
        let failed = sqlx::query!(
            "UPDATE jobs
             SET status = 'failed', lease_expires_at = NULL, finished_at = now(), error = $2
             WHERE id = $1 AND status = 'running' AND lease_expires_at <= now()
               AND attempt >= max_attempts",
            job.id,
            WORKER_LOST,
        )
        .execute(&mut *tx)
        .await?;
        if failed.rows_affected() == 1 {
            end_plan(&mut tx, plan_id, Ending::Failed(WORKER_LOST)).await?;
            tx.commit().await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
