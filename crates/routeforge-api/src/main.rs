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
//! | `BOT_SERVICE_KEY` | none | The key the Telegram bot shows to be given a user's token. At least 32 bytes. Without it the API takes no bot. |
//! | `BOT_JWT_TTL_SECS` | 60 | How long such a token is good for. |
//! | `API_BUDGET_DEFAULT_MS` | 10000 | The time each route of a plan gets when the plan asks for no budget. |
//! | `API_BUDGET_MAX_MS` | 300000 | The longest a plan may ask for, for each route. |
//! | `API_BUDGET_MAX_ITERATIONS` | 10000000000 | The most candidate moves a plan may ask for, for each route. |
//! | `API_WORKER_ALIVE_SECS` | 15 | A worker seen within this long counts as alive. |
//! | `API_LOGIN_ATTEMPTS` | 5 | How many logins are taken for one address within a minute. |
//! | `API_HASHES_AT_ONCE` | 4 | How many passwords are hashed at the same time. Each takes 19 MiB. |
//! | `LOG_FORMAT` | `text` | `json` for one JSON object per line. |
//! | `RUST_LOG` | `info` | What to log, in the syntax of `tracing`'s `EnvFilter`. |
//!
//! Started with no argument, it serves. `routeforge-api healthcheck` serves nothing:
//! it asks the API that listens on `API_BIND` whether it is ready, and ends with
//! success if it is. That is the health check of the image (D55).
//!
//! The decisions cited as D1, D2, … are in `docs/architecture.md`.

mod app;
mod auth;
mod config;
mod error;
mod events;
mod logins;
mod page;
mod probe;
mod request_id;
mod routes;
mod startup;
mod stream;
#[cfg(test)]
mod testing;
mod validate;

use actix_web::{HttpServer, web};
use anyhow::{Context, bail};
use routeforge_domain::service::{init_logging, told_to_stop};
use tracing::{info, warn};

use crate::app::{AppState, app};
use crate::auth::{Passwords, ServiceKey, Tokens};
use crate::config::{Config, Secrets};
use crate::events::Events;
use crate::logins::Logins;
use crate::stream::Pace;

/// How long the requests in hand are given to finish once the API is told to stop.
/// `docker stop` waits ten seconds before it kills, and this has to be inside that.
const STOP_WITHIN_SECS: u64 = 8;

fn main() -> anyhow::Result<()> {
    let argument = std::env::args_os()
        .nth(1)
        .map(|argument| argument.to_string_lossy().into_owned());
    match argument.as_deref() {
        None => serve(),
        // Where the API listens is read from the variable the API reads it from.
        Some("healthcheck") => probe::probe(Config::from_env()?.bind),
        Some(other) => bail!(
            "`{other}` is nothing the API does: it serves when it is given no argument, \
             and asks a running API whether it is ready when it is given `healthcheck`"
        ),
    }
}

#[actix_web::main]
async fn serve() -> anyhow::Result<()> {
    init_logging();
    let config = Config::from_env()?;
    let secrets = Secrets::from_env()?;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        ?config,
        "the API is starting"
    );
    // Said every time it starts so: the API cannot tell who reaches its port (D70).
    if let Some(warning) = secrets.demo_warning() {
        warn!("{warning}");
    }

    let pool = startup::connect(&secrets.database_url).await?;
    startup::migrate(&pool).await?;
    info!("the database has the schema");
    let passwords = Passwords::new(config.hashes_at_once)?;
    if let Some(admin) = &secrets.admin {
        startup::seed_admin(&pool, &passwords, admin).await?;
    }

    if secrets.bot_key.is_none() {
        info!("BOT_SERVICE_KEY is not set: this API takes no bot");
    }
    let state = web::Data::new(AppState {
        pool: pool.clone(),
        tokens: Tokens::new(secrets.jwt_secret.as_bytes(), config.token_ttl),
        passwords,
        logins: Logins::new(config.login_attempts, logins::WINDOW),
        bot_key: secrets.bot_key.as_deref().map(ServiceKey::new),
        bot_token_ttl: config.bot_token_ttl,
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
