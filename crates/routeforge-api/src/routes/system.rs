//! The queue and the workers that drain it, for the load view. For operators and
//! admins.

use std::time::Duration;

use actix_web::web::Bytes;
use actix_web::{HttpResponse, get, web};
use routeforge_domain::db::{queue, workers};
use routeforge_domain::{OrdersChanged, SystemStats};
use utoipa_actix_web::service_config::ServiceConfig;

use crate::app::{AppState, NotAllowed, NotLoggedIn};
use crate::auth::Staff;
use crate::error::ApiError;
use crate::events::Signal;
use crate::stream::{self, Heard, Pace, Watch};

pub(crate) fn configure(config: &mut ServiceConfig<'_>) {
    config.service(get_system_stats).service(watch_system_stats);
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
    Ok(web::Json(stats(&state).await?))
}

/// Watch the queue and its workers
///
/// An event stream (`text/event-stream`) that stays open. It opens with a `stats`
/// event, which is what `GET /api/system/stats` gives, and sends another whenever that
/// has changed: at once when a job is queued, claimed or finished, and within a couple
/// of seconds when a worker has come or has fallen silent.
///
/// It also sends an `orders` event, an `OrdersChanged`, whenever an order was placed
/// or withdrawn. It names the day and no order: whoever shows that day reads its
/// orders again. Nothing makes up for one that was lost, so a reader also reads now
/// and then without being told.
///
/// A line that starts with a colon is a comment, sent to keep the connection open.
/// The stream ends when the token it was opened with expires.
#[utoipa::path(
    tag = "system",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The events, for as long as the stream is read",
            body = String, content_type = "text/event-stream"),
        NotLoggedIn,
        NotAllowed,
    )
)]
#[get("/api/system/stats/events")]
async fn watch_system_stats(
    Staff(user): Staff,
    state: web::Data<AppState>,
) -> Result<HttpResponse, ApiError> {
    stream::open(state, user, StatsWatch { sent: None }).await
}

/// The queue and the workers, as they are now.
async fn stats(state: &AppState) -> Result<SystemStats, ApiError> {
    let queue = queue::depth(&state.pool).await?;
    let workers = workers::list(&state.pool, state.worker_alive).await?;
    let busy = workers
        .iter()
        .filter(|worker| worker.current_job.is_some())
        .count();
    Ok(SystemStats {
        queue,
        workers_alive: count(workers.len()),
        workers_busy: count(busy),
        workers,
    })
}

/// What the stream of the queue and its workers shows.
struct StatsWatch {
    /// What was last sent, so that the same is not sent twice.
    sent: Option<SystemStats>,
}

impl Watch for StatsWatch {
    fn hears(&self, signal: &Signal) -> Heard {
        match signal {
            // A change to any plan is a job queued, claimed or finished.
            Signal::Plan(_) | Signal::Missed => Heard::Changed,
            Signal::Progress(_) => Heard::Nothing,
            // Nothing that is counted here. This is the one stream a page of an
            // operator always has open, so it is where the word is passed on (D63).
            Signal::Orders(service_date) => {
                let changed = OrdersChanged {
                    service_date: *service_date,
                };
                stream::frame("orders", &changed).map_or(Heard::Nothing, Heard::Frame)
            }
        }
    }

    async fn read(&mut self, state: &AppState) -> Result<Vec<Bytes>, ApiError> {
        let stats = stats(state).await?;
        if self.sent.as_ref() == Some(&stats) {
            return Ok(Vec::new());
        }
        let event = stream::frame("stats", &stats)?;
        self.sent = Some(stats);
        Ok(vec![event])
    }

    fn unprompted(&self, pace: &Pace) -> Duration {
        pace.stats_every
    }
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
    use routeforge_domain::{JobProgress, Role, WorkerId};
    use serde_json::{Value, json};
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

    /// How long a stream is listened to before it is taken to have nothing to say.
    const QUIET: Duration = Duration::from_millis(300);

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn the_stats_are_sent_again_whenever_the_queue_changes(pool: PgPool) {
        let api = testing::api(&pool);
        let _listening = api.listen().await;
        let operator = api.actor(Role::Operator).await;
        api.depot().await;

        // It opens with what the endpoint beside it answers.
        let mut stream = api.open(&operator, "/api/system/stats/events").await;
        let first = stream.next_of("stats").await;
        assert_eq!(
            first,
            api.get(Some(&operator), "/api/system/stats").await.body
        );
        assert_eq!(first["queue"]["queued"], 0);
        assert!(stream.is_quiet_for(QUIET).await, "nothing has changed");

        // A plan is asked for, which puts a job on the queue.
        let asked = api
            .post(
                Some(&operator),
                "/api/plans",
                json!({ "service_date": date(12) }),
            )
            .await;
        assert_eq!(asked.status, StatusCode::CREATED);
        assert_eq!(stream.next_of("stats").await["queue"]["queued"], 1);

        // A worker takes it.
        let worker = WorkerId::generate();
        workers::register(&pool, worker, "solver-1").await.unwrap();
        let held = queue::claim(&pool, worker, testing::LONG)
            .await
            .unwrap()
            .unwrap();
        let stats = stream.next_of("stats").await;
        assert_eq!(
            stats["queue"],
            json!({ "queued": 0, "running": 1, "done": 0, "failed": 0, "cancelled": 0 })
        );
        assert_eq!(
            (&stats["workers_alive"], &stats["workers_busy"]),
            (&json!(1), &json!(1))
        );
        assert_eq!(stats["workers"][0]["current_job"], json!(held.lease.job_id));

        // How far a search has got changes nothing that is shown here.
        let progress = JobProgress {
            iterations: 16_384,
            best_cost_m: 900,
        };
        queue::announce_progress(&pool, &held.lease, progress, None)
            .await
            .unwrap();
        assert!(stream.is_quiet_for(QUIET).await);
    }

    /// What lets the map show an order that somebody placed elsewhere: by chat, or on
    /// another page (D63).
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn an_order_that_is_placed_or_withdrawn_is_passed_on_with_its_day(pool: PgPool) {
        let api = testing::api(&pool);
        let _listening = api.listen().await;
        let operator = api.actor(Role::Operator).await;
        let client = api.actor(Role::Client).await;
        let mut stream = api.open(&operator, "/api/system/stats/events").await;
        stream.next_of("stats").await;

        // Placed through the API, as the bot places one.
        let placed = api.order(&client, 13).await;
        assert_eq!(
            stream.next_of("orders").await,
            json!({ "service_date": "2026-10-13" })
        );
        // The queue is as it was, so there is nothing else to send.
        assert!(stream.is_quiet_for(QUIET).await);

        let cancel = format!("/api/orders/{placed}/cancel");
        let withdrawn = api.post(Some(&client), &cancel, Value::Null).await;
        assert_eq!(withdrawn.status, StatusCode::OK);
        assert_eq!(
            stream.next_of("orders").await,
            json!({ "service_date": "2026-10-13" })
        );
        // A withdrawal that was refused changed nothing, and nothing is said of it.
        let again = api.post(Some(&client), &cancel, Value::Null).await;
        assert_eq!(again.status, StatusCode::CONFLICT);
        assert!(stream.is_quiet_for(QUIET).await);

        // A day of demo orders is stored in one transaction, and is one word.
        let admin = api.actor(Role::Admin).await;
        api.depot().await;
        let demo = json!({ "service_date": date(14), "count": 25, "seed": 7 });
        let made = api.post(Some(&admin), "/api/demo/orders", demo).await;
        assert_eq!(made.status, StatusCode::CREATED, "{}", made.text);
        assert_eq!(
            stream.next_of("orders").await,
            json!({ "service_date": "2026-10-14" })
        );
        assert!(stream.is_quiet_for(QUIET).await);
    }

    /// A worker comes, and falls silent, without a word to anybody (D27). So the
    /// stream looks now and then without having been given a reason.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn the_stats_are_read_now_and_then_without_a_reason(pool: PgPool) {
        let pace = Pace {
            stats_every: Duration::from_millis(100),
            ..testing::PACE
        };
        let api = testing::api_at(&pool, pace);
        let operator = api.actor(Role::Operator).await;

        let mut stream = api.open(&operator, "/api/system/stats/events").await;
        assert_eq!(stream.next_of("stats").await["workers_alive"], 0);
        // What it finds unchanged it does not send again.
        assert!(stream.is_quiet_for(QUIET).await);

        workers::register(&pool, WorkerId::generate(), "solver-1")
            .await
            .unwrap();
        let stats = stream.next_of("stats").await;
        assert_eq!(stats["workers_alive"], 1);
        assert_eq!(stats["workers"][0]["hostname"], "solver-1");
    }
}
