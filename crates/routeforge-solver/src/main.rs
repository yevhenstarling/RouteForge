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
//! | `LOG_FORMAT` | `text` | `json` for one JSON object per line. |
//! | `RUST_LOG` | `info` | What to log, in the syntax of `tracing`'s `EnvFilter`. |
//!
//! The decisions cited as D1, D2, … are in `docs/architecture.md`.

mod config;
mod plan_job;
mod route_job;
mod startup;
#[cfg(test)]
mod testing;
mod wake;
mod worker;

use std::io::IsTerminal;

use routeforge_domain::Haversine;
use tokio::sync::watch;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::config::Config;

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

    let (stop, mut shutdown) = watch::channel(false);
    tokio::spawn(async move {
        told_to_stop().await;
        info!("told to stop");
        stop.send_replace(true);
    });

    let Some(pool) = startup::connect(&database_url, &config, &mut shutdown).await? else {
        return Ok(());
    };
    if startup::wait_for_schema(&pool, startup::SCHEMA_CHECK, &mut shutdown).await {
        worker::run(pool.clone(), config, Haversine, shutdown).await;
    }
    pool.close().await;
    info!("the solver has stopped");
    Ok(())
}

/// Logs to standard output: lines for people by default, JSON with `LOG_FORMAT=json`.
/// Either way an event carries the fields of the spans it happened in, which is where
/// the ids of the worker, the job, the plan and the request are (D33).
fn init_logging() {
    // An empty `RUST_LOG` would mean errors only. It is taken as not set.
    let filter = std::env::var("RUST_LOG")
        .ok()
        .filter(|directives| !directives.trim().is_empty())
        .and_then(|directives| EnvFilter::try_new(directives).ok())
        .unwrap_or_else(|| EnvFilter::new("info"));
    let json = std::env::var("LOG_FORMAT").is_ok_and(|format| format.eq_ignore_ascii_case("json"));
    let logging = tracing_subscriber::fmt()
        .with_env_filter(filter)
        // Colours for a person at a terminal. In a container's log they are in the
        // way of searching for an id.
        .with_ansi(std::io::stdout().is_terminal());
    if json {
        logging.json().init();
    } else {
        logging.init();
    }
}

/// Returns when the process is asked to stop: Ctrl-C anywhere, and SIGTERM where
/// there is one, which is what `docker stop` sends.
async fn told_to_stop() {
    let interrupted = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            // Without a handler only a kill ends the process, and that needs no help.
            warn!(%error, "cannot hear Ctrl-C");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        match signal(SignalKind::terminate()) {
            Ok(mut terminated) => {
                tokio::select! {
                    () = interrupted => {}
                    _ = terminated.recv() => {}
                }
            }
            Err(error) => {
                warn!(%error, "cannot hear SIGTERM");
                interrupted.await;
            }
        }
    }
    #[cfg(not(unix))]
    interrupted.await;
}
