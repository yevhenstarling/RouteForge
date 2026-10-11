//! The registry of solver processes, for the load view (D27).
//!
//! A worker adds itself when it starts and refreshes its row while it lives. The queue
//! never reads this table: it tells workers apart by the id they claim with, registered
//! or not. So the registry can be wrong or empty without a job being lost.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgExecutor;
use uuid::Uuid;

use super::{DbError, millis};
use crate::ids::{JobId, WorkerId};
use crate::job::WorkerInfo;

/// Adds a worker to the registry, or marks it as seen just now.
pub async fn register(
    executor: impl PgExecutor<'_>,
    id: WorkerId,
    hostname: &str,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO workers (id, hostname) VALUES ($1, $2)
         ON CONFLICT (id) DO UPDATE SET hostname = EXCLUDED.hostname, last_seen_at = now()",
        id.0,
        hostname,
    )
    .execute(executor)
    .await?;
    Ok(())
}

struct WorkerRow {
    id: Uuid,
    hostname: String,
    started_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    current_job: Option<Uuid>,
}

/// The workers that were seen within the last `seen_within`, oldest first, each with
/// the job it holds.
///
/// "Within" is measured on the database clock, like everything about leases.
pub async fn list(
    executor: impl PgExecutor<'_>,
    seen_within: Duration,
) -> Result<Vec<WorkerInfo>, DbError> {
    let rows = sqlx::query_as!(
        WorkerRow,
        r#"SELECT workers.id, workers.hostname, workers.started_at, workers.last_seen_at,
                  (
                      SELECT jobs.id
                      FROM jobs
                      WHERE jobs.claimed_by = workers.id
                        AND jobs.status = 'running'
                        AND jobs.lease_expires_at > now()
                      ORDER BY jobs.run_at, jobs.id
                      LIMIT 1
                  ) AS current_job
           FROM workers
           WHERE workers.last_seen_at > now() - $1::bigint * interval '1 millisecond'
           ORDER BY workers.started_at, workers.id"#,
        millis(seen_within),
    )
    .fetch_all(executor)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| WorkerInfo {
            id: row.id.into(),
            hostname: row.hostname,
            started_at: row.started_at,
            last_seen_at: row.last_seen_at,
            current_job: row.current_job.map(JobId),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;
    use crate::db::queue;
    use crate::db::testing::{self, LONG, date};

    const RECENTLY: Duration = Duration::from_secs(30);

    #[sqlx::test]
    async fn a_worker_registers_once_and_is_seen_again(pool: PgPool) {
        let worker = WorkerId::generate();
        assert!(list(&pool, RECENTLY).await.unwrap().is_empty());

        register(&pool, worker, "solver-1").await.unwrap();
        let first = list(&pool, RECENTLY).await.unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(
            (
                first[0].id,
                first[0].hostname.as_str(),
                first[0].current_job
            ),
            (worker, "solver-1", None)
        );
        assert_eq!(first[0].started_at, first[0].last_seen_at);

        register(&pool, worker, "solver-1").await.unwrap();
        let second = list(&pool, RECENTLY).await.unwrap();
        assert_eq!(second.len(), 1, "registering again adds no row");
        assert_eq!(second[0].started_at, first[0].started_at);
        assert!(second[0].last_seen_at > first[0].last_seen_at);
    }

    #[sqlx::test]
    async fn a_worker_that_fell_silent_drops_out_of_the_list(pool: PgPool) {
        let (silent, alive) = (WorkerId::generate(), WorkerId::generate());
        register(&pool, silent, "solver-1").await.unwrap();
        register(&pool, alive, "solver-2").await.unwrap();
        sqlx::query(
            "UPDATE workers SET last_seen_at = now() - interval '31 seconds' WHERE id = $1",
        )
        .bind(silent.0)
        .execute(&pool)
        .await
        .unwrap();

        let seen: Vec<WorkerId> = list(&pool, RECENTLY)
            .await
            .unwrap()
            .into_iter()
            .map(|worker| worker.id)
            .collect();
        assert_eq!(seen, [alive]);

        // With a longer memory both are there, in the order they started.
        let seen = list(&pool, Duration::from_secs(3_600)).await.unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!((seen[0].id, seen[1].id), (silent, alive));
    }

    #[sqlx::test]
    async fn the_list_shows_which_job_a_worker_holds(pool: PgPool) {
        let routing = testing::routing(&pool, date(12), 2, 1).await;
        let (busy, idle, lapsed) = (
            WorkerId::generate(),
            WorkerId::generate(),
            WorkerId::generate(),
        );
        for (worker, hostname) in [(busy, "solver-1"), (idle, "solver-2"), (lapsed, "solver-3")] {
            register(&pool, worker, hostname).await.unwrap();
        }
        let held = queue::claim(&pool, busy, LONG).await.unwrap().unwrap();
        // A job under a lease that has run out is not counted as held.
        queue::claim(&pool, lapsed, Duration::ZERO)
            .await
            .unwrap()
            .unwrap();

        let current: Vec<(WorkerId, Option<JobId>)> = list(&pool, RECENTLY)
            .await
            .unwrap()
            .into_iter()
            .map(|worker| (worker.id, worker.current_job))
            .collect();
        assert_eq!(
            current,
            [
                (busy, Some(held.lease.job_id)),
                (idle, None),
                (lapsed, None)
            ]
        );
        assert_eq!(held.lease.job_id, routing.routes[0].job_id);
    }

    /// The registry is for people to look at. The queue does not depend on it.
    #[sqlx::test]
    async fn a_worker_that_never_registered_can_still_work(pool: PgPool) {
        testing::routing(&pool, date(12), 1, 1).await;
        let unregistered = WorkerId::generate();
        let job = queue::claim(&pool, unregistered, LONG).await.unwrap();
        assert!(job.is_some());
        assert!(list(&pool, RECENTLY).await.unwrap().is_empty());
    }
}
