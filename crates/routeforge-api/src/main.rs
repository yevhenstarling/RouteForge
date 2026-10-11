//! RouteForge HTTP API: accounts and logins, orders, couriers, the depot, and the
//! requests for plans that the solvers work on.
//!
//! It serves JSON under `/api`, describes itself at `/api/openapi.json`, and shows
//! that description as Swagger UI at `/docs/`. `/healthz` and `/readyz` say whether it
//! is up and whether it can work.
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
mod request_id;
mod routes;
mod startup;
#[cfg(test)]
mod testing;
mod validate;

use std::io::IsTerminal;

use actix_web::{HttpServer, web};
use anyhow::Context;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::app::{AppState, app};
use crate::auth::{Passwords, Tokens};
use crate::config::{Config, Secrets};

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
    });
    let server = HttpServer::new(move || app(state.clone()))
        .bind(config.bind)
        .with_context(|| format!("could not listen on {}", config.bind))?;
    info!(address = %config.bind, "listening; Swagger UI is at /docs/");

    // Ctrl-C and SIGTERM end this: the server lets the requests it has finish first.
    server.run().await.context("the server failed")?;
    pool.close().await;
    info!("the API has stopped");
    Ok(())
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
