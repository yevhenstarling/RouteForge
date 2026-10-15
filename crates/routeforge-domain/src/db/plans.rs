//! Plans.
//!
//! A plan goes from queued to routing when its `plan` job has clustered the orders,
//! and from routing to done when the last of its route jobs is stored (D2). The two
//! steps, [`start_routing`] and [`route_finished`], are made inside the transaction
//! that completes the job, through [`Completion::conn`](super::queue::Completion::conn).
//! The other ways out, failed and cancelled, are taken by the queue: see
//! [`queue::fail`] and [`queue::cancel_plan`]. So is the one way on from done,
//! [`queue::discard_plan`].

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use super::{DbError, on_violation, parsed, queue, unsigned};
use crate::ids::PlanId;
use crate::job::JobPayload;
use crate::model::{NewPlan, Plan, PlanCounts, PlanFilter, PlanStatus, RouteBudget};

struct PlanRow {
    id: Uuid,
    service_date: NaiveDate,
    depot_id: Uuid,
    status: String,
    seed: i64,
    budget_time_ms: Option<i64>,
    budget_iterations: Option<i64>,
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
            budget: budget(row.budget_time_ms, row.budget_iterations)?,
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

/// Reads the two columns that hold a plan's [`RouteBudget`]. A limit that is not set
/// is a NULL.
fn budget(time_ms: Option<i64>, iterations: Option<i64>) -> Result<RouteBudget, DbError> {
    let time_ms = time_ms
        .map(|ms| unsigned(ms, "plans.budget_time_ms"))
        .transpose()?;
    let iterations = iterations
        .map(|moves| {
            u64::try_from(moves)
                .map_err(|_| DbError::Unreadable(format!("plans.budget_iterations is {moves}")))
        })
        .transpose()?;
    Ok(RouteBudget::new(time_ms, iterations)?)
}

/// Stores a new plan as queued, without a job. [`submit`] is the whole step.
///
/// A service date has one plan at a time that stands: one that is in progress, or one
/// that is done (D83). A plan that is done gives its couriers their round of the day,
/// and a second plan would give them another, with all they carry. So the date is
/// planned again once that plan is discarded, and then afresh, with every order of it
/// (D30).
///
/// The plans of the date are locked first, in the order of their ids, as every
/// operation on a plan locks its row first (D24). So a plan whose last route is stored
/// at this very moment is seen as done, and not as the plan in progress it was.
///
/// # Errors
///
/// [`DbError::ActivePlanExists`] if the service date has a plan that is queued or
/// routing, and [`DbError::DayIsPlanned`] if it has one that is done.
/// [`DbError::OutOfRange`] if the budget allows more moves than fit a signed 64-bit
/// number.
pub async fn create(executor: impl PgExecutor<'_>, new: &NewPlan) -> Result<Plan, DbError> {
    let budget_iterations = new
        .budget
        .iterations()
        .map(i64::try_from)
        .transpose()
        .map_err(|_| DbError::OutOfRange("the plan's iteration budget"))?;
    let created = sqlx::query_as!(
        PlanRow,
        r#"WITH day AS (
               SELECT status FROM plans WHERE service_date = $2 ORDER BY id FOR UPDATE
           )
           INSERT INTO plans (id, service_date, depot_id, seed, created_by, budget_time_ms,
                              budget_iterations)
           SELECT $1, $2, $3, $4, $5, $6, $7
           WHERE NOT EXISTS (SELECT 1 FROM day WHERE status = 'done')
           RETURNING id, service_date, depot_id, status, seed, budget_time_ms, budget_iterations,
                     created_by, orders_total, orders_unassigned, routes_total, routes_done,
                     error, created_at, finished_at"#,
        PlanId::generate().0,
        new.service_date,
        new.depot_id.0,
        i64::from(new.seed),
        new.created_by.0,
        new.budget.time_ms().map(i64::from),
        budget_iterations,
    )
    .fetch_optional(executor)
    .await
    .map_err(|error| {
        on_violation(
            error,
            "plans_one_active_per_date",
            DbError::ActivePlanExists,
        )
    })?;
    created.ok_or(DbError::DayIsPlanned)?.try_into()
}

/// Starts a plan: stores it and puts its `plan` job on the queue, in one transaction.
///
/// Either both exist afterwards or neither does, and the workers are notified when
/// the transaction commits. `request_id` is kept on the job, and on the route jobs that
/// follow from it, so the logs of every service can be searched for one request.
///
/// # Errors
///
/// As [`create`]: [`DbError::ActivePlanExists`] if the service date has a plan that is
/// queued or routing, and [`DbError::DayIsPlanned`] if it has one that is done.
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
        "SELECT id, service_date, depot_id, status, seed, budget_time_ms, budget_iterations,
                created_by, orders_total, orders_unassigned, routes_total, routes_done, error,
                created_at, finished_at
         FROM plans
         WHERE id = $1",
        id.0,
    )
    .fetch_optional(executor)
    .await?
    .map(Plan::try_from)
    .transpose()
}

/// The plans of a service date, newest first.
///
/// At most one of them is in progress. The others are those the date had before:
/// done, discarded, failed or cancelled.
pub async fn list(
    executor: impl PgExecutor<'_>,
    filter: &PlanFilter,
) -> Result<Vec<Plan>, DbError> {
    sqlx::query_as!(
        PlanRow,
        "SELECT id, service_date, depot_id, status, seed, budget_time_ms, budget_iterations,
                created_by, orders_total, orders_unassigned, routes_total, routes_done, error,
                created_at, finished_at
         FROM plans
         WHERE service_date = $1
         ORDER BY id DESC",
        filter.service_date,
    )
    .fetch_all(executor)
    .await?
    .into_iter()
    .map(Plan::try_from)
    .collect()
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
            budget: RouteBudget::new(Some(10_000), None).unwrap(),
            created_by: operator.id,
        };
        let created = create(&pool, &new).await.unwrap();

        assert_eq!(created.service_date, date(12));
        assert_eq!(created.depot_id, depot.id);
        assert_eq!(created.status, PlanStatus::Queued);
        assert_eq!(created.seed, u32::MAX);
        assert_eq!(created.budget, new.budget);
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
    async fn every_shape_of_budget_can_be_stored(pool: PgPool) {
        let new = testing::new_plan(&pool, date(12)).await;
        let budgets = [
            RouteBudget::new(Some(0), None),
            RouteBudget::new(Some(u32::MAX), None),
            RouteBudget::new(None, Some(0)),
            RouteBudget::new(None, Some(i64::MAX as u64)),
            RouteBudget::new(Some(2_500), Some(1_000_000)),
        ];
        // A date has one plan in progress, so each budget gets a date of its own.
        for (day, budget) in (12..).zip(budgets) {
            let new = NewPlan {
                service_date: date(day),
                budget: budget.unwrap(),
                ..new
            };
            let created = create(&pool, &new).await.unwrap();
            assert_eq!(created.budget, new.budget);
            let read_back = get(&pool, created.id).await.unwrap().unwrap();
            assert_eq!(read_back.budget, new.budget);
        }

        // More moves than the column can count.
        let too_many = NewPlan {
            service_date: date(20),
            budget: RouteBudget::new(None, Some(u64::MAX)).unwrap(),
            ..new
        };
        let error = create(&pool, &too_many).await.unwrap_err();
        assert!(matches!(error, DbError::OutOfRange(_)), "{error:?}");
    }

    #[sqlx::test]
    async fn the_schema_refuses_a_plan_without_a_usable_budget(pool: PgPool) {
        let plan = testing::plan(&pool, date(12)).await;
        let unusable: [(Option<i64>, Option<i64>); 4] = [
            (None, None),
            (Some(-1), None),
            (Some(i64::from(u32::MAX) + 1), None),
            (None, Some(-1)),
        ];
        for (time_ms, iterations) in unusable {
            let error = sqlx::query(
                "UPDATE plans SET budget_time_ms = $2, budget_iterations = $3 WHERE id = $1",
            )
            .bind(plan.id.0)
            .bind(time_ms)
            .bind(iterations)
            .execute(&pool)
            .await
            .unwrap_err();
            assert_eq!(
                error.as_database_error().unwrap().constraint(),
                Some("plans_budget_check"),
                "{time_ms:?}, {iterations:?}"
            );
        }
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

        // Another date is free. This one is not when its plan is done, which gives the
        // couriers their round of the day: it is free once that plan is discarded (D83).
        let other_day = NewPlan {
            service_date: date(13),
            ..new
        };
        submit(&pool, &other_day, None).await.unwrap();
        assert_eq!(
            route_finished(&pool, first.id).await.unwrap(),
            Some(PlanStatus::Done)
        );
        let error = submit(&pool, &new, None).await.unwrap_err();
        assert!(matches!(error, DbError::DayIsPlanned), "{error:?}");
        assert!(queue::discard_plan(&pool, first.id).await.unwrap());
        submit(&pool, &new, None).await.unwrap();
    }

    #[sqlx::test]
    async fn the_plans_of_a_date_are_listed_newest_first(pool: PgPool) {
        let new = testing::new_plan(&pool, date(12)).await;
        let on = async |day: u32| {
            let filter = PlanFilter {
                service_date: date(day),
            };
            list(&pool, &filter).await.unwrap()
        };
        assert!(on(12).await.is_empty());

        // A date has one plan in progress, so the first has to be over for a second.
        let first = create(&pool, &new).await.unwrap();
        assert!(queue::cancel_plan(&pool, first.id).await.unwrap());
        let second = create(&pool, &new).await.unwrap();
        let other_day = NewPlan {
            service_date: date(13),
            ..new
        };
        let elsewhere = create(&pool, &other_day).await.unwrap();

        let listed = on(12).await;
        let ids: Vec<PlanId> = listed.iter().map(|plan| plan.id).collect();
        assert_eq!(ids, [second.id, first.id]);
        // Each is the plan as it is read by its id.
        assert_eq!(listed[0], second);
        assert_eq!(listed[1].status, PlanStatus::Cancelled);
        assert_eq!(
            Some(&listed[1]),
            get(&pool, first.id).await.unwrap().as_ref()
        );

        assert_eq!(on(13).await, [elsewhere]);
        assert!(on(14).await.is_empty());
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
