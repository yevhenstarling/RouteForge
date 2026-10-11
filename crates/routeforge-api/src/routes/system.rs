//! The queue and the workers that drain it, for the load view. For operators and
//! admins.

use actix_web::{get, web};
use routeforge_domain::SystemStats;
use routeforge_domain::db::{queue, workers};
use utoipa_actix_web::service_config::ServiceConfig;

use crate::app::{AppState, NotAllowed, NotLoggedIn};
use crate::auth::Staff;
use crate::error::ApiError;

pub(crate) fn configure(config: &mut ServiceConfig<'_>) {
    config.service(get_system_stats);
}

/// Read the state of the queue and its workers
///
/// How many jobs stand in each status, and the workers that were seen lately, each
/// with the job it holds. A worker says it is there every few seconds, and one that
/// has been silent for a few of those is left out.
#[utoipa::path(
    tag = "system",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The queue and the workers, as they are now", body = SystemStats),
        NotLoggedIn,
        NotAllowed,
    )
)]
#[get("/api/system/stats")]
async fn get_system_stats(
    _: Staff,
    state: web::Data<AppState>,
) -> Result<web::Json<SystemStats>, ApiError> {
    let queue = queue::depth(&state.pool).await?;
    let workers = workers::list(&state.pool, state.worker_alive).await?;
    let busy = workers
        .iter()
        .filter(|worker| worker.current_job.is_some())
        .count();
    Ok(web::Json(SystemStats {
        queue,
        workers_alive: count(workers.len()),
        workers_busy: count(busy),
        workers,
    }))
}

/// There are never as many workers as a `u32` counts.
fn count(workers: usize) -> u32 {
    u32::try_from(workers).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use actix_web::http::StatusCode;
    use routeforge_domain::db::queue;
    use routeforge_domain::{Role, WorkerId};
    use serde_json::json;
    use sqlx::PgPool;

    use super::*;
    use crate::testing::{self, date};

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn the_stats_show_the_queue_and_who_is_working(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;

        let idle = api.get(Some(&operator), "/api/system/stats").await;
        assert_eq!(idle.status, StatusCode::OK);
        assert_eq!(
            idle.body,
            json!({
                "queue": { "queued": 0, "running": 0, "done": 0, "failed": 0, "cancelled": 0 },
                "workers_alive": 0,
                "workers_busy": 0,
                "workers": [],
            })
        );

        // Two plans waiting, and three workers: one holds a job, one is idle, and one
        // has not been heard of for longer than counts as alive.
        api.depot().await;
        for day in [12, 13] {
            let asked = api
                .post(
                    Some(&operator),
                    "/api/plans",
                    json!({ "service_date": date(day) }),
                )
                .await;
            assert_eq!(asked.status, StatusCode::CREATED);
        }
        let (busy, idle, silent) = (
            WorkerId::generate(),
            WorkerId::generate(),
            WorkerId::generate(),
        );
        for (worker, host) in [(busy, "solver-1"), (idle, "solver-2"), (silent, "solver-3")] {
            workers::register(&pool, worker, host).await.unwrap();
        }
        sqlx::query("UPDATE workers SET last_seen_at = now() - interval '1 minute' WHERE id = $1")
            .bind(silent.0)
            .execute(&pool)
            .await
            .unwrap();
        let held = queue::claim(&pool, busy, Duration::from_secs(600))
            .await
            .unwrap()
            .unwrap();

        let stats = api.get(Some(&operator), "/api/system/stats").await;
        assert_eq!(
            stats.body["queue"],
            json!({ "queued": 1, "running": 1, "done": 0, "failed": 0, "cancelled": 0 })
        );
        assert_eq!(
            (&stats.body["workers_alive"], &stats.body["workers_busy"]),
            (&json!(2), &json!(1))
        );
        let workers = stats.body["workers"].as_array().unwrap();
        assert_eq!(workers.len(), 2);
        assert_eq!(workers[0]["id"], json!(busy));
        assert_eq!(workers[0]["hostname"], "solver-1");
        assert_eq!(workers[0]["current_job"], json!(held.lease.job_id));
        assert_eq!(workers[1]["id"], json!(idle));
        assert_eq!(workers[1]["current_job"], json!(null));
    }
}
