//! What the API does before it serves: it connects, applies the schema, and makes
//! the first admin.
//!
//! The API owns the schema (D32). A solver applies none and waits until this has run.

use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use routeforge_domain::db::{self, DbError, users};
use routeforge_domain::{Login, NewUser, Role};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tracing::{info, warn};

use crate::auth::Passwords;
use crate::config::AdminSeed;
use crate::error::ApiError;
use crate::validate;

/// Connections the API keeps to the database, at most.
const CONNECTIONS: u32 = 10;

/// A request that gets no connection within this long is answered with an error.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// Connects to the database.
///
/// # Errors
///
/// If the database does not answer. The API does not wait for it as a solver does:
/// nothing it could do is of use without one, and an error at once says more than a
/// port that is open and serves nothing.
pub(crate) async fn connect(url: &str) -> anyhow::Result<PgPool> {
    let database = PgConnectOptions::from_str(url).context("DATABASE_URL is not a URL")?;
    PgPoolOptions::new()
        .max_connections(CONNECTIONS)
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .connect_with(database)
        .await
        .context("could not connect to the database")
}

/// Brings the schema up to what this build knows.
///
/// sqlx holds a lock on the database while it does, so two APIs that start together
/// do not get in each other's way. A database with migrations this build does not
/// know is refused: the API must not write to a schema it has not seen.
pub(crate) async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    db::MIGRATOR
        .run(pool)
        .await
        .context("could not apply the schema")
}

/// Makes the admin the settings ask for, unless the address has an account already.
///
/// An account that is there is left as it is, password and role alike. So changing
/// `ADMIN_PASSWORD` later changes nothing, and somebody who registered the address
/// first does not become an admin by it.
pub(crate) async fn seed_admin(
    pool: &PgPool,
    passwords: &Passwords,
    admin: &AdminSeed,
) -> anyhow::Result<()> {
    let login = Login {
        email: admin.email.clone(),
        password: admin.password.clone(),
    };
    if let Err(refused) = validate::registration(&login) {
        return Err(not_an_account(refused));
    }

    if let Some(existing) = users::find_by_email(pool, &admin.email).await? {
        if existing.user.role == Role::Admin {
            info!(email = %existing.user.email, "the admin of ADMIN_EMAIL is there already");
        } else {
            warn!(
                email = %existing.user.email,
                role = %existing.user.role,
                "ADMIN_EMAIL belongs to an account that is no admin; it was left as it is"
            );
        }
        return Ok(());
    }

    let new = NewUser {
        email: login.email,
        password_hash: passwords
            .hash(login.password)
            .await
            .map_err(|error| anyhow!("could not hash ADMIN_PASSWORD: {error:?}"))?,
        role: Role::Admin,
    };
    match users::create(pool, &new).await {
        Ok(user) => info!(email = %user.email, "made the admin of ADMIN_EMAIL"),
        // Another API that started at the same moment was first.
        Err(DbError::EmailTaken) => {}
        Err(error) => return Err(error).context("could not store the admin of ADMIN_EMAIL"),
    }
    Ok(())
}

/// Says which of the two settings is not what an account needs.
fn not_an_account(refused: ApiError) -> anyhow::Error {
    let ApiError::Invalid { errors, .. } = refused else {
        return anyhow!("ADMIN_EMAIL and ADMIN_PASSWORD were refused: {refused}");
    };
    let reasons: Vec<String> = errors
        .iter()
        .map(|error| {
            let setting = if error.field == "email" {
                "ADMIN_EMAIL"
            } else {
                "ADMIN_PASSWORD"
            };
            format!("{setting} {}", error.message)
        })
        .collect();
    anyhow!("{}", reasons.join(", and "))
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use serde_json::json;

    use super::*;
    use crate::testing;

    fn seed(email: &str, password: &str) -> AdminSeed {
        AdminSeed {
            email: email.to_owned(),
            password: password.to_owned(),
        }
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn the_first_admin_is_made_once_and_can_log_in(pool: PgPool) {
        let passwords = Passwords::cheap();
        let admin = seed("Admin@Example.com", "correct horse");

        seed_admin(&pool, &passwords, &admin).await.unwrap();
        let made = users::list(&pool).await.unwrap();
        assert_eq!(made.len(), 1);
        assert_eq!(
            (made[0].email.as_str(), made[0].role),
            ("admin@example.com", Role::Admin)
        );

        // Every start of the API runs this again.
        seed_admin(&pool, &passwords, &admin).await.unwrap();
        assert_eq!(users::list(&pool).await.unwrap(), made);

        let api = testing::api(&pool);
        let login = json!({ "email": "admin@example.com", "password": "correct horse" });
        let session = api.post(None, "/api/auth/login", login).await;
        assert_eq!(session.status, StatusCode::OK, "{}", session.text);
        assert_eq!(session.body["user"]["role"], "admin");
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn an_account_that_is_there_is_left_as_it_is(pool: PgPool) {
        let passwords = Passwords::cheap();
        seed_admin(
            &pool,
            &passwords,
            &seed("admin@example.com", "the first one"),
        )
        .await
        .unwrap();
        let api = testing::api(&pool);
        let log_in = async |password: &str| {
            let login = json!({ "email": "admin@example.com", "password": password });
            api.post(None, "/api/auth/login", login).await.status
        };

        // A password that was changed in the settings does not change the account's.
        seed_admin(
            &pool,
            &passwords,
            &seed("admin@example.com", "the second one"),
        )
        .await
        .unwrap();
        assert_eq!(log_in("the first one").await, StatusCode::OK);
        assert_eq!(log_in("the second one").await, StatusCode::UNAUTHORIZED);

        // And whoever took the address before the settings named it stays what they are.
        let client = json!({ "email": "early@example.com", "password": "a password" });
        let registered = api.post(None, "/api/auth/register", client).await;
        assert_eq!(registered.status, StatusCode::CREATED);
        seed_admin(
            &pool,
            &passwords,
            &seed("early@example.com", "another password"),
        )
        .await
        .unwrap();
        let early = users::find_by_email(&pool, "early@example.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(early.user.role, Role::Client);
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn settings_that_are_no_account_stop_the_start(pool: PgPool) {
        let passwords = Passwords::cheap();
        for (admin, named) in [
            (seed("admin", "correct horse"), vec!["ADMIN_EMAIL"]),
            (seed("admin@example.com", "short"), vec!["ADMIN_PASSWORD"]),
            (
                seed("admin", "short"),
                vec!["ADMIN_EMAIL", "ADMIN_PASSWORD"],
            ),
        ] {
            let error = seed_admin(&pool, &passwords, &admin).await.unwrap_err();
            for setting in named {
                assert!(error.to_string().contains(setting), "{error}");
            }
            // What was refused is not repeated.
            assert!(!error.to_string().contains("short"), "{error}");
        }
        assert!(users::list(&pool).await.unwrap().is_empty());
    }

    #[sqlx::test(migrations = false)]
    async fn the_api_brings_the_schema(pool: PgPool) {
        assert!(!db::is_migrated(&pool).await.unwrap());
        let api = testing::api(&pool);
        let unready = api.get(None, "/readyz").await;
        assert_eq!(unready.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(api.get(None, "/healthz").await.status, StatusCode::OK);

        migrate(&pool).await.unwrap();
        // A second API starting later finds nothing left to do.
        migrate(&pool).await.unwrap();

        assert!(db::is_migrated(&pool).await.unwrap());
        let ready = api.get(None, "/readyz").await;
        assert_eq!(ready.status, StatusCode::OK);
        assert_eq!(ready.body, json!({ "status": "ready" }));
    }

    #[tokio::test]
    async fn a_database_that_is_not_there_ends_the_start_with_the_reason() {
        let error = connect("the database").await.unwrap_err();
        assert!(error.to_string().contains("DATABASE_URL"), "{error:#}");

        // Port 9 is the discard service, which nothing offers any more.
        let nowhere = "postgres://nobody:secret@127.0.0.1:9/nothing";
        let error = connect(nowhere).await.unwrap_err();
        assert!(error.to_string().contains("could not connect"), "{error:#}");
        // The address holds the password, and is not repeated.
        assert!(!format!("{error:#}").contains("secret"), "{error:#}");
    }
}
