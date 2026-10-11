//! Plans.
//!
//! A plan goes from queued to routing when its `plan` job has clustered the orders,
//! and from routing to done when the last of its route jobs is stored (D2). The two
//! steps, [`start_routing`] and [`route_finished`], are made inside the transaction
//! that completes the job, through [`Completion::conn`](super::queue::Completion::conn).
//! The other ways out, failed and cancelled, are taken by the queue: see
//! [`queue::fail`] and [`queue::cancel_plan`].

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use super::{DbError, on_violation, parsed, queue, unsigned};
use crate::ids::PlanId;
use crate::job::JobPayload;
use crate::model::{NewPlan, Plan, PlanCounts, PlanStatus};

struct PlanRow {
    id: Uuid,
    service_date: NaiveDate,
    depot_id: Uuid,
    status: String,
    seed: i64,
    created_by: Uuid,
    orders_total: i64,
    orders_unassigned: i64,
    routes_total: i64,
    routes_done: i64,
    error: Option<String>,
    created_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

impl TryFrom<PlanRow> for Plan {
    type Error = DbError;

    fn try_from(row: PlanRow) -> Result<Self, DbError> {
        Ok(Self {
            id: row.id.into(),
            service_date: row.service_date,
            depot_id: row.depot_id.into(),
            status: parsed(&row.status)?,
            seed: unsigned(row.seed, "plans.seed")?,
            created_by: row.created_by.into(),
            orders_total: unsigned(row.orders_total, "plans.orders_total")?,
            orders_unassigned: unsigned(row.orders_unassigned, "plans.orders_unassigned")?,
            routes_total: unsigned(row.routes_total, "plans.routes_total")?,
            routes_done: unsigned(row.routes_done, "plans.routes_done")?,
            error: row.error,
            created_at: row.created_at,
            finished_at: row.finished_at,
        })
    }
}

/// Stores a new plan as queued, without a job. [`submit`] is the whole step.
///
/// # Errors
///
/// [`DbError::ActivePlanExists`] if the service date has a plan that is queued or
/// routing.
pub async fn create(executor: impl PgExecutor<'_>, new: &NewPlan) -> Result<Plan, DbError> {
    sqlx::query_as!(
        PlanRow,
        "INSERT INTO plans (id, service_date, depot_id, seed, created_by)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id, service_date, depot_id, status, seed, created_by, orders_total,
                   orders_unassigned, routes_total, routes_done, error, created_at, finished_at",
        PlanId::generate().0,
        new.service_date,
        new.depot_id.0,
        i64::from(new.seed),
        new.created_by.0,
    )
    .fetch_one(executor)
    .await
    .map_err(|error| {
        on_violation(
            error,
            "plans_one_active_per_date",
            DbError::ActivePlanExists,
        )
    })?
    .try_into()
}

/// Starts a plan: stores it and puts its `plan` job on the queue, in one transaction.
///
/// Either both exist afterwards or neither does, and the workers are notified when
/// the transaction commits. `request_id` is kept on the job, and on the route jobs that
/// follow from it, so the logs of every service can be searched for one request.
///
/// # Errors
///
/// [`DbError::ActivePlanExists`] if the service date has a plan that is queued or
/// routing.
pub async fn submit(
    pool: &PgPool,
    new: &NewPlan,
    request_id: Option<&str>,
) -> Result<Plan, DbError> {
    let mut tx = pool.begin().await?;
    let plan = create(&mut *tx, new).await?;
    queue::enqueue(&mut *tx, &JobPayload::Plan { plan_id: plan.id }, request_id).await?;
    tx.commit().await?;
    Ok(plan)
}

pub async fn get(executor: impl PgExecutor<'_>, id: PlanId) -> Result<Option<Plan>, DbError> {
    sqlx::query_as!(
        PlanRow,
        "SELECT id, service_date, depot_id, status, seed, created_by, orders_total,
                orders_unassigned, routes_total, routes_done, error, created_at, finished_at
         FROM plans
         WHERE id = $1",
        id.0,
    )
    .fetch_optional(executor)
    .await?
    .map(Plan::try_from)
    .transpose()
}

/// Records what clustering found and moves a queued plan on.
///
/// The plan is routing afterwards. With no route to wait for, because the date has no
/// order that fits a courier, it is done at once. Returns the new status, or `None`
/// if the plan was not queued and nothing was changed.
pub async fn start_routing(
    executor: impl PgExecutor<'_>,
    id: PlanId,
    counts: PlanCounts,
) -> Result<Option<PlanStatus>, DbError> {
    sqlx::query_scalar!(
        "UPDATE plans
         SET orders_total = $2,
             orders_unassigned = $3,
             routes_total = $4,
             status = CASE WHEN $4::bigint = 0 THEN 'done' ELSE 'routing' END,
             finished_at = CASE WHEN $4::bigint = 0 THEN now() END
         WHERE id = $1 AND status = 'queued'
         RETURNING status",
        id.0,
        i64::from(counts.orders_total),
        i64::from(counts.orders_unassigned),
        i64::from(counts.routes_total),
    )
    .fetch_optional(executor)
    .await?
    .as_deref()
    .map(parsed)
    .transpose()
}

/// Counts one more stored route and marks the plan done if it was the last (D2).
///
/// The update locks the plan row, so two route jobs that finish at the same moment
/// count one after the other and exactly one of them sees the plan become done.
/// Returns the new status, or `None` if the plan was not routing and nothing was
/// changed.
pub async fn route_finished(
    executor: impl PgExecutor<'_>,
    id: PlanId,
) -> Result<Option<PlanStatus>, DbError> {
    sqlx::query_scalar!(
        "UPDATE plans
         SET routes_done = routes_done + 1,
             status = CASE WHEN routes_done + 1 = routes_total THEN 'done' ELSE status END,
             finished_at = CASE WHEN routes_done + 1 = routes_total THEN now() END
         WHERE id = $1 AND status = 'routing'
         RETURNING status",
        id.0,
    )
    .fetch_optional(executor)
    .await?
    .as_deref()
    .map(parsed)
    .transpose()
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;
    use crate::db::testing::{self, date};
    use crate::job::{JobKind, JobStatus};
    use crate::model::Role;

    fn counts(orders_total: u32, orders_unassigned: u32, routes_total: u32) -> PlanCounts {
        PlanCounts {
            orders_total,
            orders_unassigned,
            routes_total,
        }
    }

    #[sqlx::test]
    async fn a_plan_can_be_stored_and_read_back(pool: PgPool) {
        let operator = testing::user(&pool, Role::Operator).await;
        let depot = testing::depot(&pool).await;
        let new = NewPlan {
            service_date: date(12),
            depot_id: depot.id,
            seed: u32::MAX,
            created_by: operator.id,
        };
        let created = create(&pool, &new).await.unwrap();

        assert_eq!(created.service_date, date(12));
        assert_eq!(created.depot_id, depot.id);
        assert_eq!(created.status, PlanStatus::Queued);
        assert_eq!(created.seed, u32::MAX);
        assert_eq!(created.created_by, operator.id);
        assert_eq!(
            (
                created.orders_total,
                created.orders_unassigned,
                created.routes_total,
                created.routes_done
            ),
            (0, 0, 0, 0)
        );
        assert_eq!((&created.error, created.finished_at), (&None, None));
        assert_eq!(get(&pool, created.id).await.unwrap(), Some(created));
        assert_eq!(get(&pool, PlanId::generate()).await.unwrap(), None);
    }

    #[sqlx::test]
    async fn submitting_a_plan_puts_its_job_on_the_queue(pool: PgPool) {
        let new = testing::new_plan(&pool, date(12)).await;
        let plan = submit(&pool, &new, Some("req-7")).await.unwrap();

        let jobs = queue::list_for_plan(&pool, plan.id).await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].payload, JobPayload::Plan { plan_id: plan.id });
        assert_eq!(jobs[0].payload.kind(), JobKind::Plan);
        assert_eq!(jobs[0].status, JobStatus::Queued);
        assert_eq!(jobs[0].request_id.as_deref(), Some("req-7"));
    }

    #[sqlx::test]
    async fn a_date_has_one_plan_in_progress_at_a_time(pool: PgPool) {
        let new = testing::new_plan(&pool, date(12)).await;
        let first = submit(&pool, &new, None).await.unwrap();

        // While the first is queued, and while it is routing.
        let error = submit(&pool, &new, None).await.unwrap_err();
        assert!(matches!(error, DbError::ActivePlanExists), "{error:?}");
        start_routing(&pool, first.id, counts(4, 0, 1))
            .await
            .unwrap();
        let error = create(&pool, &new).await.unwrap_err();
        assert!(matches!(error, DbError::ActivePlanExists), "{error:?}");

        // The refused plan left no job behind.
        let all_jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(all_jobs, 1);

        // Another date is free, and so is this one once its plan is done.
        let other_day = NewPlan {
            service_date: date(13),
            ..new
        };
        submit(&pool, &other_day, None).await.unwrap();
        assert_eq!(
            route_finished(&pool, first.id).await.unwrap(),
            Some(PlanStatus::Done)
        );
        submit(&pool, &new, None).await.unwrap();
    }

    #[sqlx::test]
    async fn a_plan_counts_its_routes_and_is_done_with_the_last(pool: PgPool) {
        let plan = testing::plan(&pool, date(12)).await;

        // Routes cannot be counted before the plan is clustered.
        assert_eq!(route_finished(&pool, plan.id).await.unwrap(), None);

        assert_eq!(
            start_routing(&pool, plan.id, counts(10, 2, 3))
                .await
                .unwrap(),
            Some(PlanStatus::Routing)
        );
        // A plan is clustered once.
        assert_eq!(
            start_routing(&pool, plan.id, counts(1, 1, 1))
                .await
                .unwrap(),
            None
        );

        for expected in [PlanStatus::Routing, PlanStatus::Routing, PlanStatus::Done] {
            let routing = get(&pool, plan.id).await.unwrap().unwrap();
            assert_eq!(routing.status, PlanStatus::Routing);
            assert_eq!(routing.finished_at, None);
            assert_eq!(
                route_finished(&pool, plan.id).await.unwrap(),
                Some(expected)
            );
        }

        let done = get(&pool, plan.id).await.unwrap().unwrap();
        assert_eq!(done.status, PlanStatus::Done);
        assert_eq!(
            (
                done.orders_total,
                done.orders_unassigned,
                done.routes_total,
                done.routes_done
            ),
            (10, 2, 3, 3)
        );
        assert!(done.finished_at.is_some());

        // A done plan takes no more routes.
        assert_eq!(route_finished(&pool, plan.id).await.unwrap(), None);
    }

    #[sqlx::test]
    async fn a_plan_with_nothing_to_route_is_done_when_it_is_clustered(pool: PgPool) {
        let plan = testing::plan(&pool, date(12)).await;
        assert_eq!(
            start_routing(&pool, plan.id, counts(3, 3, 0))
                .await
                .unwrap(),
            Some(PlanStatus::Done)
        );
        let done = get(&pool, plan.id).await.unwrap().unwrap();
        assert_eq!(done.status, PlanStatus::Done);
        assert_eq!((done.orders_total, done.orders_unassigned), (3, 3));
        assert!(done.finished_at.is_some());
    }

    #[sqlx::test]
    async fn more_unassigned_orders_than_orders_is_refused(pool: PgPool) {
        let plan = testing::plan(&pool, date(12)).await;
        let error = start_routing(&pool, plan.id, counts(2, 3, 1))
            .await
            .unwrap_err();
        assert!(
            matches!(&error, DbError::Sqlx(sqlx::Error::Database(database))
                if database.constraint() == Some("plans_counts_check")),
            "{error:?}"
        );
    }

    /// Failed and cancelled are reached through the queue, and tested there. Here every
    /// status is written once, to keep the schema's CHECK and the enumeration in step.
    #[sqlx::test]
    async fn the_schema_accepts_every_plan_status(pool: PgPool) {
        let plan = testing::plan(&pool, date(12)).await;
        for &status in PlanStatus::ALL {
            sqlx::query(
                "UPDATE plans
                 SET status = $2, finished_at = CASE WHEN $3 THEN NULL ELSE now() END
                 WHERE id = $1",
            )
            .bind(plan.id.0)
            .bind(status.as_str())
            .bind(status.is_active())
            .execute(&pool)
            .await
            .unwrap();
            assert_eq!(get(&pool, plan.id).await.unwrap().unwrap().status, status);
        }
    }
}
