//! How an idle worker learns that a job may be waiting (D33).
//!
//! Two things end its wait. One is a notification on the jobs channel, which every
//! `enqueue` and `release` sends. The other is time: a job that becomes due because
//! its retry time has come or its lease has run out announces nothing, and a
//! notification can be missed while the listener reconnects (D26). So a worker also
//! looks at the queue every poll interval.

use std::time::Duration;

use rand::rngs::SmallRng;
use rand::{RngExt, SeedableRng};
use routeforge_domain::WorkerId;
use routeforge_domain::db::queue::JOBS_CHANNEL;
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::warn;

/// How long the listener waits before it tries again after an error: this long at
/// first, then twice as long each time, up to [`RETRY_UP_TO`].
const RETRY_FROM: Duration = Duration::from_secs(1);
const RETRY_UP_TO: Duration = Duration::from_secs(30);

/// The wake-ups of a process, as its workers see them.
///
/// What is counted is occasions to look at the queue, not jobs: a plan that fans out
/// into twenty route jobs is one notification (D26), and a reconnect counts although
/// nothing may have happened. A worker that is awake claims until nothing is due, so
/// one wake-up is enough for any number of jobs.
#[derive(Clone, Debug)]
pub(crate) struct Wake(watch::Receiver<u64>);

impl Wake {
    /// Forgets the wake-ups so far.
    ///
    /// A worker calls this before it claims. A job that is announced after the call
    /// then ends the wait that follows the claim, even if it was announced before the
    /// wait began.
    pub(crate) fn mark_seen(&mut self) {
        self.0.mark_unchanged();
    }

    /// Waits for a wake-up that came after the last [`mark_seen`](Self::mark_seen).
    ///
    /// Without a listener this never returns, and the worker is left with polling.
    pub(crate) async fn woken(&mut self) {
        if self.0.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Starts listening on the jobs channel, on a connection of its own from `pool`.
///
/// The task ends when the pool is closed or when no [`Wake`] is left.
pub(crate) fn listen(pool: PgPool) -> (Wake, JoinHandle<()>) {
    let (sender, receiver) = watch::channel(0);
    (Wake(receiver), tokio::spawn(listener(pool, sender)))
}

async fn listener(pool: PgPool, wake: watch::Sender<u64>) {
    let count = || wake.send_modify(|count| *count = count.wrapping_add(1));
    let mut retry_in = RETRY_FROM;
    loop {
        let error = match subscribe(&pool).await {
            Ok(mut listener) => {
                retry_in = RETRY_FROM;
                // A job may have been announced while nobody was listening.
                count();
                loop {
                    match listener.try_recv().await {
                        // `None` is a connection that was lost and is back. What was
                        // announced in between is not known, so it counts as well.
                        Ok(_) => count(),
                        Err(error) => break error,
                    }
                }
            }
            Err(error) => error,
        };
        if matches!(error, sqlx::Error::PoolClosed) || wake.is_closed() {
            return;
        }
        warn!(%error, ?retry_in, "not listening for jobs for now; polling goes on");
        tokio::time::sleep(retry_in).await;
        retry_in = (retry_in * 2).min(RETRY_UP_TO);
    }
}

async fn subscribe(pool: &PgPool) -> Result<PgListener, sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen(JOBS_CHANNEL).await?;
    Ok(listener)
}

/// The pause between two looks at the queue that nothing prompted.
///
/// It is the poll interval give or take a quarter, so that workers started together,
/// as `docker compose up --scale` starts them, do not all ask at the same moment. The
/// generator is seeded with the worker's id: the workspace builds `rand` without the
/// operating system's entropy (D5), and the id is random already.
#[derive(Debug)]
pub(crate) struct Jitter(SmallRng);

impl Jitter {
    pub(crate) fn for_worker(id: WorkerId) -> Self {
        let (_, random_half) = id.0.as_u64_pair();
        Self(SmallRng::seed_from_u64(random_half))
    }

    /// A span between three quarters and five quarters of `interval`.
    pub(crate) fn around(&mut self, interval: Duration) -> Duration {
        interval.mul_f64(self.0.random_range(0.75..1.25))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::soon;

    #[test]
    fn a_pause_is_within_a_quarter_of_the_interval() {
        let interval = Duration::from_secs(8);
        let mut jitter = Jitter::for_worker(WorkerId::generate());
        let pauses: Vec<Duration> = (0..1_000).map(|_| jitter.around(interval)).collect();

        assert!(
            pauses
                .iter()
                .all(|pause| (Duration::from_secs(6)..Duration::from_secs(10)).contains(pause))
        );
        // Spread over the range, not all alike.
        assert!(pauses.iter().any(|pause| *pause < Duration::from_secs(7)));
        assert!(pauses.iter().any(|pause| *pause > Duration::from_secs(9)));
    }

    #[test]
    fn two_workers_do_not_pause_in_step() {
        let interval = Duration::from_secs(8);
        let pauses = |id| {
            let mut jitter = Jitter::for_worker(id);
            (0..8).map(|_| jitter.around(interval)).collect::<Vec<_>>()
        };
        let worker = WorkerId::generate();
        assert_eq!(pauses(worker), pauses(worker));
        assert_ne!(pauses(worker), pauses(WorkerId::generate()));
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_notification_ends_the_wait_and_nothing_else_does(pool: PgPool) {
        let (mut wake, listener) = listen(pool.clone());
        let notify = async || {
            sqlx::query("SELECT pg_notify($1, 'plan')")
                .bind(JOBS_CHANNEL)
                .execute(&pool)
                .await
                .unwrap();
        };
        let quiet = Duration::from_millis(300);

        // The listener counts once when it starts, in case it missed something.
        soon(wake.woken()).await;
        wake.mark_seen();
        assert!(tokio::time::timeout(quiet, wake.woken()).await.is_err());

        notify().await;
        soon(wake.woken()).await;

        // Announced after the worker last looked, and before it starts to wait.
        wake.mark_seen();
        notify().await;
        tokio::time::sleep(quiet).await;
        soon(wake.woken()).await;

        // What was there before the worker looked is not held against it afterwards.
        notify().await;
        tokio::time::sleep(quiet).await;
        wake.mark_seen();
        assert!(tokio::time::timeout(quiet, wake.woken()).await.is_err());

        listener.abort();
        let _ = listener.await;
    }

    #[tokio::test]
    async fn without_a_listener_the_wait_never_ends() {
        let (sender, receiver) = watch::channel(0);
        let mut wake = Wake(receiver);
        drop(sender);
        let waited = tokio::time::timeout(Duration::from_millis(100), wake.woken()).await;
        assert!(waited.is_err());
    }
}
