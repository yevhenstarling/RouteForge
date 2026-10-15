//! What the API hears from the database, passed on to its event streams (D50).
//!
//! One connection listens on the two channels the queue notifies (D48), that a plan
//! has changed and how far the search for a route has got, and on the one the orders
//! notify, that an order of a day was placed or withdrawn (D63). What it hears goes
//! into a broadcast channel, and every open stream holds a receiver of that. So the
//! API has one listening connection however many streams are open.
//!
//! A notification is a hint and can be missed (D26). When the listener has had to
//! reconnect it says [`Signal::Missed`], and a stream that fell behind is told as much
//! by the channel. Either way the stream reads what it shows again.

use std::sync::Arc;

use chrono::NaiveDate;
use routeforge_domain::db::orders::ORDERS_CHANNEL;
use routeforge_domain::db::queue::{PLANS_CHANNEL, PROGRESS_CHANNEL};
use routeforge_domain::service::Backoff;
use routeforge_domain::{PlanId, RouteProgress};
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

/// How many signals a stream may fall behind by before it has missed some. A plan
/// with every worker searching announces a few dozen a second.
pub(crate) const BACKLOG: usize = 1024;

/// What the listener passes on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Signal {
    /// A plan has changed, or one of its jobs has. What changed is not said (D48).
    Plan(PlanId),
    /// How far the search for a route has got.
    Progress(Arc<RouteProgress>),
    /// An order of this day was placed or withdrawn (D63).
    Orders(NaiveDate),
    /// Notifications may have been lost, so whatever a stream shows may be out of date.
    Missed,
}

/// The hub between the one connection that listens and the streams that are open.
pub(crate) struct Events {
    signals: broadcast::Sender<Signal>,
    /// `true` from the moment the API is told to stop.
    closed: watch::Sender<bool>,
}

/// One stream's end of the hub.
pub(crate) struct Subscription {
    pub(crate) signals: broadcast::Receiver<Signal>,
    pub(crate) closed: watch::Receiver<bool>,
}

impl Events {
    /// A hub that nothing feeds yet: see [`listen`](Self::listen).
    pub(crate) fn new() -> Self {
        let (signals, _) = broadcast::channel(BACKLOG);
        let (closed, _) = watch::channel(false);
        Self { signals, closed }
    }

    pub(crate) fn subscribe(&self) -> Subscription {
        Subscription {
            signals: self.signals.subscribe(),
            closed: self.closed.subscribe(),
        }
    }

    /// Ends every stream that is open, and every one that is opened from now on.
    ///
    /// The server lets a response finish before it stops, and a stream has no end of
    /// its own. Without this the API would not stop while anybody had the page open.
    pub(crate) fn close(&self) {
        self.closed.send_replace(true);
    }

    /// Passes a signal on to every open stream, as the listener does. For a test that
    /// says by hand what the database would announce.
    #[cfg(test)]
    pub(crate) fn send(&self, signal: Signal) {
        pass_on(&self.signals, signal);
    }

    /// Starts listening to the database, on a connection of its own from `pool`.
    ///
    /// The task ends when the pool is closed.
    pub(crate) fn listen(&self, pool: PgPool) -> JoinHandle<()> {
        tokio::spawn(listener(pool, self.signals.clone()))
    }
}

fn pass_on(signals: &broadcast::Sender<Signal>, signal: Signal) {
    // The only error there is means that no stream is open, which is no error.
    let _ = signals.send(signal);
}

async fn listener(pool: PgPool, signals: broadcast::Sender<Signal>) {
    // As the solver waits for its database (D37).
    let mut retry = Backoff::STANDARD;
    loop {
        let error = match subscribe(&pool).await {
            Ok(mut listener) => {
                retry.reset();
                // What was announced while nobody listened is not known.
                pass_on(&signals, Signal::Missed);
                loop {
                    match listener.try_recv().await {
                        Ok(Some(notification)) => {
                            let said = read(notification.channel(), notification.payload());
                            if let Some(signal) = said {
                                pass_on(&signals, signal);
                            }
                        }
                        // The connection was lost and is back, and so is the listening.
                        Ok(None) => pass_on(&signals, Signal::Missed),
                        Err(error) => break error,
                    }
                }
            }
            Err(error) => error,
        };
        if matches!(error, sqlx::Error::PoolClosed) {
            return;
        }
        let retry_in = retry.pause();
        warn!(%error, ?retry_in, "not listening for changes for now; the streams go on reading at their own pace");
        tokio::time::sleep(retry_in).await;
    }
}

async fn subscribe(pool: &PgPool) -> Result<PgListener, sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener
        .listen_all([PLANS_CHANNEL, PROGRESS_CHANNEL, ORDERS_CHANNEL])
        .await?;
    Ok(listener)
}

/// What a notification says. `None` if this build cannot read it.
///
/// That takes a solver newer than the API, which is treated as a job that a solver
/// cannot read is treated (D22): it is passed over, and nothing stops for it.
fn read(channel: &str, payload: &str) -> Option<Signal> {
    let signal = match channel {
        PLANS_CHANNEL => payload.parse().ok().map(Signal::Plan),
        PROGRESS_CHANNEL => serde_json::from_str(payload)
            .ok()
            .map(|progress| Signal::Progress(Arc::new(progress))),
        // The day, written as `2026-10-12`.
        ORDERS_CHANNEL => payload.parse().ok().map(Signal::Orders),
        _ => None,
    };
    if signal.is_none() {
        debug!(channel, "passed over a notification this build cannot read");
    }
    signal
}

#[cfg(test)]
mod tests {
    use routeforge_domain::db::{orders, plans, queue};
    use routeforge_domain::{JobId, JobProgress, WorkerId};
    use serde_json::json;

    use super::*;
    use crate::testing::{self, soon};

    #[test]
    fn a_notification_is_read_by_the_channel_it_came_on() {
        let plan_id = PlanId::generate();
        assert_eq!(
            read(PLANS_CHANNEL, &plan_id.to_string()),
            Some(Signal::Plan(plan_id))
        );

        let progress = RouteProgress {
            plan_id,
            job_id: JobId::generate(),
            attempt: 2,
            iterations: 32_768,
            best_cost_m: 4_200,
            order: Some(vec![1, 0]),
        };
        let payload = serde_json::to_string(&progress).unwrap();
        assert_eq!(
            read(PROGRESS_CHANNEL, &payload),
            Some(Signal::Progress(Arc::new(progress)))
        );

        assert_eq!(
            read(ORDERS_CHANNEL, "2026-10-12"),
            Some(Signal::Orders(testing::date(12)))
        );

        // What a newer build might send, and what was never meant for the API.
        let from_a_newer_build = json!({ "plan_id": plan_id, "moves": 7 }).to_string();
        for (channel, payload) in [
            (PLANS_CHANNEL, "not an id"),
            (PLANS_CHANNEL, ""),
            (PLANS_CHANNEL, "2026-10-12"),
            (PROGRESS_CHANNEL, from_a_newer_build.as_str()),
            (PROGRESS_CHANNEL, "progress"),
            (ORDERS_CHANNEL, "12.10.2026"),
            (ORDERS_CHANNEL, "2026-13-01"),
            (ORDERS_CHANNEL, ""),
            ("routeforge_jobs", "route"),
        ] {
            assert_eq!(read(channel, payload), None, "{channel}: {payload}");
        }
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn what_the_database_announces_is_passed_on_to_every_subscriber(pool: PgPool) {
        let events = Events::new();
        let (mut first, mut second) = (events.subscribe(), events.subscribe());
        let listening = events.listen(pool.clone());
        // It starts by saying that it cannot know what came before it.
        assert_eq!(soon(first.signals.recv()).await, Ok(Signal::Missed));
        assert_eq!(soon(second.signals.recv()).await, Ok(Signal::Missed));

        let new = testing::new_plan(&pool, 12).await;
        let plan = plans::submit(&pool, &new, None).await.unwrap();
        assert_eq!(soon(first.signals.recv()).await, Ok(Signal::Plan(plan.id)));
        assert_eq!(soon(second.signals.recv()).await, Ok(Signal::Plan(plan.id)));

        let job = queue::claim(&pool, WorkerId::generate(), testing::LONG)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(soon(first.signals.recv()).await, Ok(Signal::Plan(plan.id)));
        let progress = JobProgress {
            iterations: 16_384,
            best_cost_m: 900,
        };
        queue::announce_progress(&pool, &job.lease, progress, Some(&[0]))
            .await
            .unwrap();
        let Ok(Signal::Progress(said)) = soon(first.signals.recv()).await else {
            panic!("the progress of the search was not passed on");
        };
        assert_eq!(
            (said.plan_id, said.job_id, said.attempt, said.iterations),
            (plan.id, job.lease.job_id, 1, 16_384)
        );

        // One that cannot be read is passed over, and the next one is heard all the same.
        sqlx::query("SELECT pg_notify($1, 'not an id')")
            .bind(PLANS_CHANNEL)
            .execute(&pool)
            .await
            .unwrap();
        assert!(queue::cancel_plan(&pool, plan.id).await.unwrap());
        assert_eq!(soon(first.signals.recv()).await, Ok(Signal::Plan(plan.id)));

        // An order that is placed, and withdrawn again, says which day it is of.
        let order = orders::create(&pool, &testing::new_order(new.created_by, 13))
            .await
            .unwrap();
        let of_the_day = Ok(Signal::Orders(testing::date(13)));
        assert_eq!(soon(first.signals.recv()).await, of_the_day);
        assert!(orders::cancel(&pool, order.id).await.unwrap());
        assert_eq!(soon(first.signals.recv()).await, of_the_day);

        listening.abort();
        let _ = listening.await;
    }

    /// A connection that listens can be lost like any other, and what was announced
    /// meanwhile with it. The listener cannot say what that was, only that it may be.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_listener_that_lost_its_connection_says_that_it_may_have_missed_something(
        pool: PgPool,
    ) {
        let events = Events::new();
        let mut subscription = events.subscribe();
        let listening = events.listen(pool.clone());
        assert_eq!(soon(subscription.signals.recv()).await, Ok(Signal::Missed));

        let ended: Vec<bool> = sqlx::query_scalar(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity
             WHERE datname = current_database() AND query LIKE 'LISTEN%'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(ended, [true], "the one connection that listens");

        assert_eq!(soon(subscription.signals.recv()).await, Ok(Signal::Missed));
        // And it listens again.
        let new = testing::new_plan(&pool, 12).await;
        let plan = plans::submit(&pool, &new, None).await.unwrap();
        assert_eq!(
            soon(subscription.signals.recv()).await,
            Ok(Signal::Plan(plan.id))
        );

        listening.abort();
        let _ = listening.await;
    }

    #[tokio::test]
    async fn closing_the_hub_is_seen_by_those_who_subscribed_before_and_after() {
        let events = Events::new();
        let mut early = events.subscribe();
        assert!(!*early.closed.borrow_and_update());

        events.close();
        soon(early.closed.changed()).await.unwrap();
        assert!(*early.closed.borrow());
        assert!(*events.subscribe().closed.borrow());
    }
}
