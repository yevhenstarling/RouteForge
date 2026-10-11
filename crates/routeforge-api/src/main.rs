//! RouteForge HTTP API: accounts and logins, orders, couriers, the depot, and the
//! requests for plans that the solvers work on.
//!
//! It serves JSON under `/api`, describes itself at `/api/openapi.json`, and shows
//! that description as Swagger UI at `/docs/`. `/healthz` and `/readyz` say whether it
//! is up and whether it can work. At `/` is the page with the map, which shows a plan
//! while the solvers are at it, through two event streams (D50).
//!
//! When it starts it brings the database's schema up to what this build knows. The
//! solvers apply none and wait for it (D32).
//!
//! Settings, all from the environment:
//!
//! | Variable | Default | |
//! |---|---|---|
//! | `DATABASE_URL` | none, required | The PostgreSQL database. |
//! | `JWT_SECRET` | none, required | The key that signs the tokens. At least 32 bytes. |
//! | `API_BIND` | `127.0.0.1:8080` | The address and port to listen on. |
//! | `JWT_TTL_SECS` | 3600 | How long a login is good for. |
//! | `ADMIN_EMAIL`, `ADMIN_PASSWORD` | none | The admin to make at start-up, if there is none with that address. Both or neither. |
//! | `API_BUDGET_DEFAULT_MS` | 10000 | The time each route of a plan gets when the plan asks for no budget. |
//! | `API_BUDGET_MAX_MS` | 300000 | The longest a plan may ask for, for each route. |
//! | `API_BUDGET_MAX_ITERATIONS` | 10000000000 | The most candidate moves a plan may ask for, for each route. |
//! | `API_WORKER_ALIVE_SECS` | 15 | A worker seen within this long counts as alive. |
//! | `LOG_FORMAT` | `text` | `json` for one JSON object per line. |
//! | `RUST_LOG` | `info` | What to log, in the syntax of `tracing`'s `EnvFilter`. |
//!
//! The decisions cited as D1, D2, … are in `docs/architecture.md`.

mod app;
mod auth;
mod config;
mod error;
mod events;
mod page;
mod request_id;
mod routes;
mod startup;
mod stream;
#[cfg(test)]
mod testing;
mod validate;

use std::io::IsTerminal;

use actix_web::{HttpServer, web};
use anyhow::Context;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::app::{AppState, app};
use crate::auth::{Passwords, Tokens};
use crate::config::{Config, Secrets};
use crate::events::Events;
use crate::stream::Pace;

/// How long the requests in hand are given to finish once the API is told to stop.
/// `docker stop` waits ten seconds before it kills, and this has to be inside that.
const STOP_WITHIN_SECS: u64 = 8;

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    init_logging();
    let config = Config::from_env()?;
    let secrets = Secrets::from_env()?;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        ?config,
        "the API is starting"
    );

    let pool = startup::connect(&secrets.database_url).await?;
    startup::migrate(&pool).await?;
    info!("the database has the schema");
    let passwords = Passwords::new()?;
    if let Some(admin) = &secrets.admin {
        startup::seed_admin(&pool, &passwords, admin).await?;
    }

    let state = web::Data::new(AppState {
        pool: pool.clone(),
        tokens: Tokens::new(secrets.jwt_secret.as_bytes(), config.token_ttl),
        passwords,
        limits: config.limits,
        worker_alive: config.worker_alive,
        events: Events::new(),
        pace: Pace::default(),
    });
    let listening = state.events.listen(pool.clone());

    // Told to stop, the server lets the requests it has finish first. An event stream
    // has no end of its own, so the streams are ended before the server is told.
    let stopping = {
        let state = state.clone();
        async move {
            told_to_stop().await;
            info!("told to stop");
            state.events.close();
        }
    };
    let server = HttpServer::new(move || app(state.clone()))
        .shutdown_signal(stopping)
        .shutdown_timeout(STOP_WITHIN_SECS)
        .bind(config.bind)
        .with_context(|| format!("could not listen on {}", config.bind))?;
    info!(address = %config.bind, "listening; the page is at / and Swagger UI at /docs/");

    server.run().await.context("the server failed")?;
    listening.abort();
    pool.close().await;
    info!("the API has stopped");
    Ok(())
}

/// Returns when the process is asked to stop: Ctrl-C anywhere, and SIGTERM where
/// there is one, which is what `docker stop` sends. The solver listens the same way.
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

/// Logs to standard output: lines for people by default, JSON with `LOG_FORMAT=json`.
/// Either way an event carries the fields of the span it happened in, which is where
/// the id of the request is. The solver sets its logging up the same way.
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
        // Colours for a person at a terminal. In a file they are in the way of
        // searching for an id.
        .with_ansi(std::io::stdout().is_terminal());
    if json {
        logging.json().init();
    } else {
        logging.init();
    }
}
