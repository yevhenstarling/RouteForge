//! RouteForge queue worker: takes `plan` and `route` jobs from the queue and computes
//! courier routes.
//!
//! Several instances run side by side and need no coordinator. Each takes the next
//! job that is due, and a job whose worker dies is taken over by another when its
//! lease has run out (D6). `docker compose up --scale solver=4` is all that scaling
//! takes.
//!
//! Settings, all from the environment:
//!
//! | Variable | Default | |
//! |---|---|---|
//! | `DATABASE_URL` | none, required | The PostgreSQL database with the queue. |
//! | `SOLVER_CONCURRENCY` | 1 | Workers in this process, each doing one job at a time. |
//! | `SOLVER_LEASE_SECS` | 30 | How long a claim holds without a heartbeat. |
//! | `SOLVER_HEARTBEAT_SECS` | 5 | How often a running job extends its lease. |
//! | `SOLVER_POLL_SECS` | 5 | How often an idle worker looks at the queue unprompted. |
//! | `SOLVER_PROGRESS_MS` | 250 | How often, at most, a search announces how far it has got. |
//! | `OSRM_URL` | none | An OSRM server to ask for road distances, and for the way a route takes along the roads. Without it the distances are great circles. |
//! | `OSRM_TIMEOUT_SECS` | 20 | How long that server may take over one answer. |
//! | `LOG_FORMAT` | `text` | `json` for one JSON object per line. |
//! | `RUST_LOG` | `info` | What to log, in the syntax of `tracing`'s `EnvFilter`. |
//!
//! A route the OSRM server gives no distances for is searched for on great circles,
//! and says so (D66). A route it gives no way for is stored without one (D73). So the
//! server may be down, or started later than the solver.
//!
//! The decisions cited as D1, D2, … are in `docs/architecture.md`.

mod config;
mod osrm;
mod plan_job;
mod route_job;
mod startup;
#[cfg(test)]
mod testing;
mod wake;
mod worker;

use routeforge_domain::service::{init_logging, told_to_stop};
use tokio::sync::watch;
use tracing::info;

use crate::config::Config;
use crate::osrm::Provider;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging();
    let config = Config::from_env()?;
    let database_url = config::database_url()?;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        ?config,
        "the solver is starting"
    );
    let distances = Provider::of(config.roads.as_ref())?;

    let (stop, mut shutdown) = watch::channel(false);
    tokio::spawn(async move {
        told_to_stop().await;
        info!("told to stop");
        stop.send_replace(true);
    });

    let Some(pool) = startup::connect(&database_url, &config, &mut shutdown).await? else {
        return Ok(());
    };
    let ran = if startup::wait_for_schema(&pool, startup::SCHEMA_CHECK, &mut shutdown).await {
        worker::run(pool.clone(), config, distances, shutdown).await
    } else {
        Ok(())
    };
    pool.close().await;
    // A worker that was lost ends the process with its reason, and with a status that
    // says something went wrong (D82).
    ran?;
    info!("the solver has stopped");
    Ok(())
}
