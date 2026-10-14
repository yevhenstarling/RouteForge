//! The worker: takes jobs from the queue, runs them, and tells the queue how each one
//! ended (D33 to D37).
//!
//! A worker does one job at a time. It claims until nothing is due, then waits for a
//! notification, for its poll interval or to be told to stop, whichever comes first.
//! A process runs [`Config::concurrency`] of them, one unless it is told otherwise,
//! so capacity grows with the number of solver instances.

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use futures_util::FutureExt as _;
use routeforge_domain::db::queue::{self, ClaimedJob};
use routeforge_domain::db::{DbError, workers};
use routeforge_domain::{DistanceProvider, JobPayload, WorkerId};
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::MissedTickBehavior;
use tracing::{Instrument, error, info, info_span, warn};

use crate::config::Config;
use crate::wake::{self, Jitter, Wake};
use crate::{plan_job, route_job};

/// Becomes `true` once, when the process is told to stop.
pub(crate) type Shutdown = watch::Receiver<bool>;

/// A worker that was not seen for this long is taken out of the registry when a solver
/// starts. The load view shows those seen within seconds (D27).
const FORGOTTEN_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Waits until the process is told to stop. If nobody is left to say so, that counts
/// as being told.
pub(crate) async fn stopped(shutdown: &mut Shutdown) {
    let _ = shutdown.wait_for(|&stop| stop).await;
}

/// What the workers of one process share.
pub(crate) struct Context<P> {
    pub(crate) pool: PgPool,
    pub(crate) config: Config,
    /// Where the distances between the stops of a route come from.
    pub(crate) distances: P,
}

/// How a job ended when it did not fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Its results are stored and the job is done.
    Done,
    /// The job was no longer this worker's: its plan has ended, or another worker has
    /// claimed it. Nothing was stored (D6).
    Gone,
}

/// Runs the workers of this process until `shutdown` says stop.
///
/// They share the pool and one connection that listens for notifications.
///
/// # Errors
///
/// When a worker has ended without being told to (D82). The others are told to stop
/// then, and give their jobs back. A process that goes on with a worker less says
/// nothing about it, and one that ends says why.
pub(crate) async fn run<P>(
    pool: PgPool,
    config: Config,
    distances: P,
    mut shutdown: Shutdown,
) -> anyhow::Result<()>
where
    P: DistanceProvider + Send + Sync + 'static,
{
    let (wake, listener) = wake::listen(pool.clone());
    let hostname: Arc<str> = hostname().into();
    let context = Arc::new(Context {
        pool,
        config,
        distances,
    });

    // The workers of solvers that are gone would stay in the registry for good: every
    // solver that starts registers workers under new ids (D84).
    match workers::forget(&context.pool, FORGOTTEN_AFTER).await {
        Ok(0) => {}
        Ok(forgotten) => info!(
            forgotten,
            "took workers not seen for a day out of the registry"
        ),
        Err(error) => warn!(%error, "could not tidy the registry of workers"),
    }

    // The workers stop when the process is told to, and when one of them is lost.
    let (halt, halted) = watch::channel(false);
    let mut workers = JoinSet::new();
    for _ in 0..context.config.concurrency {
        let worker = Worker::new(
            Arc::clone(&context),
            Arc::clone(&hostname),
            wake.clone(),
            halted.clone(),
        );
        workers.spawn(worker.run());
    }
    let ended = supervise(workers, &halt, &mut shutdown).await;
    listener.abort();
    ended
}

/// Waits until every worker has ended, and tells them to stop when the process is.
///
/// A worker ends when it is told to stop, and at no other time: a job that panics is
/// a failed attempt and no more (D82). So a worker that ends otherwise has been lost
/// to a fault outside its jobs. `halt` then tells the others to stop as well, and the
/// answer says what became of it.
async fn supervise(
    mut workers: JoinSet<()>,
    halt: &watch::Sender<bool>,
    shutdown: &mut Shutdown,
) -> anyhow::Result<()> {
    let mut lost = None;
    loop {
        tokio::select! {
            ended = workers.join_next() => {
                let Some(ended) = ended else { break };
                let fault = match ended {
                    Err(error) if error.is_panic() => {
                        Some(format!("it panicked: {}", said(&*error.into_panic())))
                    }
                    Err(error) => Some(error.to_string()),
                    Ok(()) if !*halt.borrow() => Some("it stopped by itself".to_owned()),
                    Ok(()) => None,
                };
                if let Some(reason) = fault {
                    error!(%reason, "a worker was lost; the others are told to stop");
                    halt.send_replace(true);
                    lost.get_or_insert(reason);
                }
            }
            () = stopped(shutdown), if !*halt.borrow() => {
                halt.send_replace(true);
            }
        }
    }
    match lost {
        Some(reason) => Err(anyhow!("a worker was lost: {reason}")),
        None => Ok(()),
    }
}

/// The name this machine goes by in the registry. Under Docker it is the id of the
/// container, which tells the instances of a scaled service apart.
fn hostname() -> String {
    ["HOSTNAME", "COMPUTERNAME"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

pub(crate) struct Worker<P> {
    context: Arc<Context<P>>,
    id: WorkerId,
    hostname: Arc<str>,
    wake: Wake,
    shutdown: Shutdown,
    jitter: Jitter,
}

impl<P> Worker<P>
where
    P: DistanceProvider + Send + Sync + 'static,
{
    /// A worker with an id of its own. The queue tells workers apart by it (D22).
    pub(crate) fn new(
        context: Arc<Context<P>>,
        hostname: Arc<str>,
        wake: Wake,
        shutdown: Shutdown,
    ) -> Self {
        let id = WorkerId::generate();
        Self {
            context,
            id,
            hostname,
            wake,
            shutdown,
            jitter: Jitter::for_worker(id),
        }
    }

    /// For a test, which has to tell its workers apart.
    #[cfg(test)]
    pub(crate) fn id(&self) -> WorkerId {
        self.id
    }

    /// Works until told to stop.
    pub(crate) async fn run(mut self) {
        let span = info_span!("worker", worker_id = %self.id);
        async move {
            info!(hostname = %self.hostname, "the worker is looking for jobs");
            let seen = keep_registered(
                Arc::clone(&self.context),
                self.id,
                Arc::clone(&self.hostname),
            );
            // Beside the work and not in a task of its own, so that it ends with the
            // worker however the worker ends.
            tokio::select! {
                () = self.work() => {}
                () = seen => {}
            }
            info!("the worker has stopped");
        }
        .instrument(span)
        .await;
    }

    async fn work(&mut self) {
        loop {
            if *self.shutdown.borrow() {
                return;
            }
            // Before the claim: a job announced from here on ends the wait below.
            self.wake.mark_seen();
            // The claim is not raced against the order to stop. Abandoned half way,
            // it could leave a job claimed that nobody knows of.
            let lease = self.context.config.lease;
            match queue::claim(&self.context.pool, self.id, lease).await {
                Ok(Some(job)) => {
                    self.settle(job).await;
                    // There may be more: one notification can stand for many jobs.
                    continue;
                }
                Ok(None) => {}
                Err(DbError::Unreadable(what)) => {
                    // The queue has counted the attempt as failed. On to the next job.
                    warn!(%what, "passed over a job this build cannot read");
                    continue;
                }
                // The wait below is the pause before the next try.
                Err(error) => warn!(%error, "could not look for a job"),
            }

            let pause = self.jitter.around(self.context.config.poll);
            tokio::select! {
                () = self.wake.woken() => {}
                () = tokio::time::sleep(pause) => {}
                () = stopped(&mut self.shutdown) => return,
            }
        }
    }

    /// Runs one job and tells the queue how it ended.
    async fn settle(&self, job: ClaimedJob) {
        let span = info_span!(
            "job",
            job_id = %job.lease.job_id,
            plan_id = %job.lease.plan_id,
            kind = %job.payload.kind(),
            attempt = job.lease.attempt,
            request_id = job.request_id.as_deref(),
        );
        let context = &*self.context;
        let mut shutdown = self.shutdown.clone();
        async {
            // Told to stop, the worker drops the job where it stands: an open
            // completion rolls back, and a route search is told to end (D34, D35).
            let ended = tokio::select! {
                biased;
                () = stopped(&mut shutdown) => None,
                ended = guarded(execute(context, &job)) => Some(ended),
            };
            match ended {
                Some(Ok(Outcome::Done)) => info!("the job is done"),
                Some(Ok(Outcome::Gone)) => {
                    info!("the job is no longer this worker's; nothing was stored");
                }
                Some(Err(error)) => {
                    let reason = format!("{error:#}");
                    warn!(error = %reason, "the attempt failed");
                    match queue::fail(&context.pool, &job.lease, &reason).await {
                        Ok(true) => {}
                        Ok(false) => info!("the job was no longer this worker's to fail"),
                        Err(error) => {
                            warn!(%error, "could not report the failure; the lease will run out");
                        }
                    }
                }
                None => match queue::release(&context.pool, &job.lease).await {
                    Ok(true) => info!("gave the job back: the worker is stopping"),
                    Ok(false) => info!("the job was over before it could be given back"),
                    Err(error) => {
                        warn!(%error, "could not give the job back; the lease will run out");
                    }
                },
            }
        }
        .instrument(span)
        .await;
    }
}

async fn execute<P>(context: &Context<P>, job: &ClaimedJob) -> anyhow::Result<Outcome>
where
    P: DistanceProvider + Sync,
{
    match &job.payload {
        JobPayload::Plan { plan_id } => plan_job::run(&context.pool, job, *plan_id).await,
        JobPayload::Route {
            plan_id,
            courier_id,
            order_ids,
            seed,
        } => {
            let route = route_job::Route {
                plan_id: *plan_id,
                courier_id: *courier_id,
                order_ids,
                seed: *seed,
            };
            route_job::run(context, &job.lease, &route).await
        }
    }
}

/// What a job gives, with a panic anywhere in it as an error of the attempt (D82).
///
/// A panic on the blocking thread of a search or a clustering comes back as an error
/// already (D37). One in the job's own code would unwind through the worker and end
/// it, and the job would wait out its lease for the next worker to panic over. Caught
/// here, it is a failed attempt like any other, with what the panic said as its reason.
///
/// The job is not polled again after it has panicked. It is dropped with whatever it
/// held, the heartbeat and the search's flag among them, as a job that is told to stop
/// is (D34).
async fn guarded<T>(job: impl Future<Output = anyhow::Result<T>>) -> anyhow::Result<T> {
    match AssertUnwindSafe(job).catch_unwind().await {
        Ok(ended) => ended,
        Err(panic) => Err(anyhow!("the job panicked: {}", said(&*panic))),
    }
}

/// What a panic said, where it said it in words.
fn said(panic: &(dyn Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("it said nothing in words")
}

/// Puts a worker into the registry and marks it as seen every heartbeat interval,
/// whether it is working or idle. Never returns.
async fn keep_registered<P>(context: Arc<Context<P>>, id: WorkerId, hostname: Arc<str>) {
    let mut every = tokio::time::interval(context.config.heartbeat);
    every.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        // The first tick is immediate.
        every.tick().await;
        if let Err(error) = workers::register(&context.pool, id, &hostname).await {
            // The registry is for people to look at. The queue does not need it (D27).
            warn!(%error, "could not mark the worker as seen");
        }
    }
}

#[cfg(test)]
mod tests;
