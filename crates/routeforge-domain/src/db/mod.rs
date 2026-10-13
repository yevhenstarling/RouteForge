//! PostgreSQL persistence and the job queue (D3).
//!
//! One module per table, or per group of tables that change together. [`queue`] is the
//! job queue; the others are plain repositories. The conventions they share, in the
//! schema and in how rows become domain types, are D19.
//!
//! How the functions take their connection:
//!
//! - A function that runs one statement takes any executor: a pool, a connection, or a
//!   transaction as `&mut *tx`. It then takes part in whatever transaction the caller
//!   is in.
//! - A function that needs several statements to be one unit opens its own transaction
//!   and takes the pool.
//! - [`orders::lock_pending`] only makes sense inside the caller's transaction, and
//!   takes a connection to say so.
//!
//! Every query is checked against the schema when the crate is compiled. The build
//! reads what it needs from the `.sqlx` directory at the workspace root, so it needs no
//! database; `scripts/sqlx-prepare` rewrites that directory after a query or a
//! migration has changed (D28).

use std::str::FromStr;
use std::time::Duration;

use sqlx::PgExecutor;

use crate::geo::{InvalidLatLon, InvalidPolyline};
use crate::model::UnlimitedBudget;
use crate::text::UnknownVariant;

pub mod couriers;
pub mod depots;
pub mod orders;
pub mod plans;
pub mod queue;
pub mod routes;
#[cfg(test)]
mod testing;
pub mod users;
pub mod workers;

/// The schema, embedded from the `migrations` directory of this crate.
///
/// The API runs it at startup, and the solver waits until [`is_migrated`] says it has
/// been run (D32). Tests in other crates hand it to
/// `#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]`.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

/// PostgreSQL's code for a query that names a table which does not exist.
const UNDEFINED_TABLE: &str = "42P01";

/// Whether the database has every migration this build knows.
///
/// A database that nothing has been applied to answers `false`, and so does one where
/// a migration was started and did not succeed. One with migrations this build does
/// not know answers `true`: it is newer than the build, and what that means for a
/// service is for the service to decide.
pub async fn is_migrated(executor: impl PgExecutor<'_>) -> Result<bool, DbError> {
    let applied = sqlx::query_scalar!("SELECT version FROM _sqlx_migrations WHERE success")
        .fetch_all(executor)
        .await;
    let applied = match applied {
        Ok(applied) => applied,
        // sqlx makes its bookkeeping table when it applies the first migration.
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some(UNDEFINED_TABLE) => {
            return Ok(false);
        }
        Err(error) => return Err(error.into()),
    };
    Ok(MIGRATOR
        .iter()
        .filter(|migration| !migration.migration_type.is_down_migration())
        .all(|migration| applied.contains(&migration.version)))
}

/// Why a repository or the queue could not do what it was asked.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// [`users::create`]: the address already belongs to an account.
    #[error("the email address is already registered")]
    EmailTaken,
    /// [`plans::create`]: a service date has at most one plan in progress.
    #[error("the service date already has a plan in progress")]
    ActivePlanExists,
    /// A value is larger than its column can hold.
    #[error("{0} is too large to be stored")]
    OutOfRange(&'static str),
    /// A stored value is not one this build knows. The schema's checks rule that out
    /// for a database this build migrated, so it points at a newer schema.
    #[error("the database holds a value this build cannot read: {0}")]
    Unreadable(String),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

impl From<UnknownVariant> for DbError {
    fn from(unknown: UnknownVariant) -> Self {
        Self::Unreadable(unknown.to_string())
    }
}

impl From<InvalidLatLon> for DbError {
    fn from(invalid: InvalidLatLon) -> Self {
        Self::Unreadable(invalid.to_string())
    }
}

impl From<InvalidPolyline> for DbError {
    fn from(invalid: InvalidPolyline) -> Self {
        Self::Unreadable(invalid.to_string())
    }
}

impl From<UnlimitedBudget> for DbError {
    fn from(unlimited: UnlimitedBudget) -> Self {
        Self::Unreadable(unlimited.to_string())
    }
}

/// Turns the violation of one named constraint into the error a caller can act on.
fn on_violation(error: sqlx::Error, constraint: &str, meaning: DbError) -> DbError {
    match &error {
        sqlx::Error::Database(database) if database.constraint() == Some(constraint) => meaning,
        _ => DbError::Sqlx(error),
    }
}

/// Reads a column that holds a `u32`. Such a column is a BIGINT with a range check.
fn unsigned(value: i64, column: &'static str) -> Result<u32, DbError> {
    u32::try_from(value)
        .map_err(|_| DbError::Unreadable(format!("{column} is {value}, which is no u32")))
}

/// Reads a column that holds an enumeration as text.
fn parsed<T: FromStr<Err = UnknownVariant>>(text: &str) -> Result<T, DbError> {
    Ok(text.parse()?)
}

/// A span of time as the whole milliseconds a query adds to the database clock.
///
/// The worker says how long, the database says from when (D6). A span too long for
/// the database to add is refused by the database.
fn millis(span: Duration) -> i64 {
    i64::try_from(span.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    #[test]
    fn a_stored_number_outside_a_u32_is_an_error() {
        assert_eq!(unsigned(0, "x").unwrap(), 0);
        assert_eq!(unsigned(i64::from(u32::MAX), "x").unwrap(), u32::MAX);
        for outside in [-1, i64::from(u32::MAX) + 1, i64::MIN, i64::MAX] {
            let error = unsigned(outside, "couriers.capacity").unwrap_err();
            assert!(
                matches!(&error, DbError::Unreadable(what) if what.contains("couriers.capacity")),
                "{error:?}"
            );
        }
    }

    #[test]
    fn a_span_is_passed_in_whole_milliseconds() {
        assert_eq!(millis(Duration::ZERO), 0);
        assert_eq!(millis(Duration::from_micros(1_999)), 1);
        assert_eq!(millis(Duration::from_secs(60)), 60_000);
        assert_eq!(millis(Duration::MAX), i64::MAX);
    }

    /// The test database was migrated from the same directory by `#[sqlx::test]`. Had
    /// the two differed, running the exported migrator would apply a migration or
    /// refuse a changed one.
    #[sqlx::test]
    async fn the_exported_migrator_is_the_schema_the_tests_run_on(pool: PgPool) {
        MIGRATOR.run(&pool).await.unwrap();
        let applied: i64 =
            sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(applied, MIGRATOR.iter().count() as i64);
        assert!(is_migrated(&pool).await.unwrap());
    }

    #[sqlx::test(migrations = false)]
    async fn a_database_is_migrated_once_every_migration_has_succeeded(pool: PgPool) {
        // Nothing has been applied, so there is not even a bookkeeping table.
        assert!(!is_migrated(&pool).await.unwrap());

        MIGRATOR.run(&pool).await.unwrap();
        assert!(is_migrated(&pool).await.unwrap());

        // A migration that was started and did not finish.
        sqlx::query(
            "UPDATE _sqlx_migrations SET success = false
             WHERE version = (SELECT max(version) FROM _sqlx_migrations)",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(!is_migrated(&pool).await.unwrap());

        // A database that is one migration behind this build.
        sqlx::query(
            "DELETE FROM _sqlx_migrations
             WHERE version = (SELECT max(version) FROM _sqlx_migrations)",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(!is_migrated(&pool).await.unwrap());
    }

    /// The database of a newer build has everything this one needs.
    #[sqlx::test]
    async fn a_database_ahead_of_this_build_counts_as_migrated(pool: PgPool) {
        sqlx::query(
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
             VALUES (9999, 'from a newer build', true, ''::bytea, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(is_migrated(&pool).await.unwrap());
    }
}
