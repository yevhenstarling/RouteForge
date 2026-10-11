//! Getting to a database the workers can use: one that answers, and that has the
//! schema (D32).
//!
//! Neither can be taken for granted when the stack comes up together. The database
//! may still be starting, and the schema is applied by the API, which may not have
//! got that far. So a solver waits for both, for as long as it takes.

use std::str::FromStr;
use std::time::Duration;

use anyhow::Context as _;
use routeforge_domain::db;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tracing::{info, warn};

use crate::config::Config;
use crate::worker::{Shutdown, stopped};

/// How long to wait before connecting again after a failure: this long at first, then
/// twice as long each time, up to [`RETRY_UP_TO`].
const RETRY_FROM: Duration = Duration::from_secs(1);
const RETRY_UP_TO: Duration = Duration::from_secs(30);

/// How often a solver looks whether the schema has arrived.
pub(crate) const SCHEMA_CHECK: Duration = Duration::from_secs(1);

/// Connects to the database, however long it takes to come up.
///
/// `None` means the process was told to stop first.
///
/// # Errors
///
/// If `url` is not a URL at all. Trying again would not help.
pub(crate) async fn connect(
    url: &str,
    config: &Config,
    shutdown: &mut Shutdown,
) -> anyhow::Result<Option<PgPool>> {
    let database = PgConnectOptions::from_str(url).context("DATABASE_URL is not a URL")?;
    let options = PgPoolOptions::new()
        .max_connections(config.connections())
        // A call that gets no connection within a heartbeat has failed, as far as
        // keeping a lease goes. Better to know than to go on waiting.
        .acquire_timeout(config.heartbeat);

    let mut retry_in = RETRY_FROM;
    loop {
        let connecting = options.clone().connect_with(database.clone());
        tokio::select! {
            biased;
            () = stopped(shutdown) => return Ok(None),
            connected = connecting => match connected {
                Ok(pool) => return Ok(Some(pool)),
                Err(error) => warn!(%error, ?retry_in, "could not connect to the database"),
            },
        }
        tokio::select! {
            () = stopped(shutdown) => return Ok(None),
            () = tokio::time::sleep(retry_in) => {}
        }
        retry_in = (retry_in * 2).min(RETRY_UP_TO);
    }
}

/// Waits until the database has every migration this build knows, looking once every
/// `every`. A solver applies none itself.
///
/// `false` means the process was told to stop first.
pub(crate) async fn wait_for_schema(
    pool: &PgPool,
    every: Duration,
    shutdown: &mut Shutdown,
) -> bool {
    let mut said_so = false;
    loop {
        match db::is_migrated(pool).await {
            Ok(true) => return true,
            Ok(false) if !said_so => {
                info!("the database does not have the schema this build needs; waiting for it");
                said_so = true;
            }
            Ok(false) => {}
            Err(error) => warn!(%error, "could not look at the schema"),
        }
        tokio::select! {
            () = stopped(shutdown) => return false,
            () = tokio::time::sleep(every) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use routeforge_domain::db::MIGRATOR;
    use tokio::sync::watch;

    use super::*;
    use crate::testing::{self, soon};

    const OFTEN: Duration = Duration::from_millis(20);

    #[sqlx::test(migrations = false)]
    async fn a_solver_waits_until_the_schema_has_been_applied(pool: PgPool) {
        let (_stop, mut shutdown) = watch::channel(false);
        let waiting = tokio::spawn({
            let pool = pool.clone();
            async move { wait_for_schema(&pool, OFTEN, &mut shutdown).await }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!waiting.is_finished(), "there is no schema to work with");

        MIGRATOR.run(&pool).await.unwrap();
        assert!(soon(waiting).await.unwrap(), "the schema is there now");
    }

    #[sqlx::test(migrations = false)]
    async fn the_wait_for_the_schema_ends_when_the_solver_is_told_to_stop(pool: PgPool) {
        let (stop, mut shutdown) = watch::channel(false);
        let waiting = tokio::spawn({
            let pool = pool.clone();
            async move { wait_for_schema(&pool, OFTEN, &mut shutdown).await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;

        stop.send_replace(true);
        assert!(!soon(waiting).await.unwrap());
    }

    #[tokio::test]
    async fn connecting_goes_on_until_the_solver_is_told_to_stop() {
        // Port 9 is the discard service, which nothing offers any more. Whatever
        // happens to a connection there, it is no database.
        let nowhere = "postgres://nobody@127.0.0.1:9/nothing";
        let (stop, mut shutdown) = watch::channel(false);
        let connecting =
            tokio::spawn(async move { connect(nowhere, &testing::config(), &mut shutdown).await });
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(!connecting.is_finished(), "it gave up");

        stop.send_replace(true);
        assert!(soon(connecting).await.unwrap().unwrap().is_none());
    }

    #[tokio::test]
    async fn an_address_that_is_no_url_is_refused_at_once() {
        let (_stop, mut shutdown) = watch::channel(false);
        let error = connect("the database", &testing::config(), &mut shutdown)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("DATABASE_URL"), "{error:#}");
    }
}
