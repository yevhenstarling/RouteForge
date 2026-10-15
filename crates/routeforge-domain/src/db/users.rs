//! Users.
//!
//! An account is reached by an address with a password, or by Telegram (D59). The
//! first kind is opened with [`create`]. The second is made by
//! [`find_or_create_by_telegram`], the first time the bot hears of somebody.

use chrono::{DateTime, Utc};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use super::{DbError, on_violation, parsed};
use crate::ids::UserId;
use crate::model::{Credentials, NewUser, Role, User};

struct UserRow {
    id: Uuid,
    email: Option<String>,
    telegram_id: Option<i64>,
    role: String,
    created_at: DateTime<Utc>,
}

impl TryFrom<UserRow> for User {
    type Error = DbError;

    fn try_from(row: UserRow) -> Result<Self, DbError> {
        Ok(Self {
            id: row.id.into(),
            email: row.email,
            telegram_id: row.telegram_id,
            role: parsed(&row.role)?,
            created_at: row.created_at,
        })
    }
}

/// Stores a new user. The address is stored in lower case.
///
/// # Errors
///
/// [`DbError::EmailTaken`] if the address, in any case, already has an account.
pub async fn create(executor: impl PgExecutor<'_>, new: &NewUser) -> Result<User, DbError> {
    sqlx::query_as!(
        UserRow,
        "INSERT INTO users (id, email, password_hash, role)
         VALUES ($1, $2, $3, $4)
         RETURNING id, email, telegram_id, role, created_at",
        UserId::generate().0,
        new.email.to_lowercase(),
        new.password_hash,
        new.role.as_str(),
    )
    .fetch_one(executor)
    .await
    .map_err(|error| on_violation(error, "users_email_key", DbError::EmailTaken))?
    .try_into()
}

pub async fn get(executor: impl PgExecutor<'_>, id: UserId) -> Result<Option<User>, DbError> {
    sqlx::query_as!(
        UserRow,
        "SELECT id, email, telegram_id, role, created_at FROM users WHERE id = $1",
        id.0,
    )
    .fetch_optional(executor)
    .await?
    .map(User::try_from)
    .transpose()
}

/// The user with this address, in any case, together with the password hash.
pub async fn find_by_email(
    executor: impl PgExecutor<'_>,
    email: &str,
) -> Result<Option<Credentials>, DbError> {
    // An account with an address has a password: the schema sees to it. So the hash
    // is read as one that is there.
    let Some(row) = sqlx::query!(
        r#"SELECT id, email, telegram_id, role, created_at, password_hash AS "password_hash!"
           FROM users
           WHERE email = $1"#,
        email.to_lowercase(),
    )
    .fetch_optional(executor)
    .await?
    else {
        return Ok(None);
    };
    let user = UserRow {
        id: row.id,
        email: row.email,
        telegram_id: row.telegram_id,
        role: row.role,
        created_at: row.created_at,
    };
    Ok(Some(Credentials {
        user: user.try_into()?,
        password_hash: row.password_hash,
    }))
}

/// The account of this Telegram user, if they have one.
pub async fn find_by_telegram(
    executor: impl PgExecutor<'_>,
    telegram_id: i64,
) -> Result<Option<User>, DbError> {
    sqlx::query_as!(
        UserRow,
        "SELECT id, email, telegram_id, role, created_at FROM users WHERE telegram_id = $1",
        telegram_id,
    )
    .fetch_optional(executor)
    .await?
    .map(User::try_from)
    .transpose()
}

/// The account of this Telegram user. If they have none it is made here: a client's,
/// with no address and no password (D59).
///
/// However many ask at the same moment for somebody new, one account is made, and
/// every one of them is given it. The schema refuses an id that is not positive.
pub async fn find_or_create_by_telegram(pool: &PgPool, telegram_id: i64) -> Result<User, DbError> {
    // Everybody but a newcomer has one, and reading it writes nothing.
    if let Some(known) = find_by_telegram(pool, telegram_id).await? {
        return Ok(known);
    }
    make_for_telegram(pool, telegram_id).await
}

/// Makes the account of a Telegram user, or gives the one that somebody else made
/// since the caller looked.
///
/// Whoever loses the race to insert waits for the winner and then meets the conflict.
/// The update it makes there changes nothing. It is there because a conflict that
/// does nothing would return no row: the winner's row is not in the snapshot of this
/// statement.
async fn make_for_telegram(
    executor: impl PgExecutor<'_>,
    telegram_id: i64,
) -> Result<User, DbError> {
    sqlx::query_as!(
        UserRow,
        "INSERT INTO users (id, telegram_id, role)
         VALUES ($1, $2, $3)
         ON CONFLICT (telegram_id) DO UPDATE SET telegram_id = EXCLUDED.telegram_id
         RETURNING id, email, telegram_id, role, created_at",
        UserId::generate().0,
        telegram_id,
        Role::Client.as_str(),
    )
    .fetch_one(executor)
    .await?
    .try_into()
}

/// Every user, oldest first.
pub async fn list(executor: impl PgExecutor<'_>) -> Result<Vec<User>, DbError> {
    sqlx::query_as!(
        UserRow,
        "SELECT id, email, telegram_id, role, created_at FROM users ORDER BY id",
    )
    .fetch_all(executor)
    .await?
    .into_iter()
    .map(User::try_from)
    .collect()
}

/// Changes what a user may do. `None` if there is no such user.
pub async fn set_role(
    executor: impl PgExecutor<'_>,
    id: UserId,
    role: Role,
) -> Result<Option<User>, DbError> {
    sqlx::query_as!(
        UserRow,
        "UPDATE users SET role = $2 WHERE id = $1
         RETURNING id, email, telegram_id, role, created_at",
        id.0,
        role.as_str(),
    )
    .fetch_optional(executor)
    .await?
    .map(User::try_from)
    .transpose()
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;
    use crate::db::testing;

    fn new_user(email: &str, role: Role) -> NewUser {
        NewUser {
            email: email.to_owned(),
            password_hash: "$argon2id$not-a-real-hash".to_owned(),
            role,
        }
    }

    #[sqlx::test]
    async fn a_user_can_be_stored_and_read_back(pool: PgPool) {
        let created = create(&pool, &new_user("Ada@Example.com", Role::Operator))
            .await
            .unwrap();
        assert_eq!(created.email.as_deref(), Some("ada@example.com"));
        assert_eq!(created.telegram_id, None);
        assert_eq!(created.role, Role::Operator);

        assert_eq!(get(&pool, created.id).await.unwrap(), Some(created.clone()));
        assert_eq!(get(&pool, UserId::generate()).await.unwrap(), None);

        let credentials = find_by_email(&pool, "ADA@example.COM")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(credentials.user, created);
        assert_eq!(credentials.password_hash, "$argon2id$not-a-real-hash");
        assert!(
            find_by_email(&pool, "nobody@example.com")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[sqlx::test]
    async fn an_address_has_one_account_whatever_its_case(pool: PgPool) {
        create(&pool, &new_user("ada@example.com", Role::Client))
            .await
            .unwrap();
        let error = create(&pool, &new_user("ADA@example.com", Role::Admin))
            .await
            .unwrap_err();
        assert!(matches!(error, DbError::EmailTaken), "{error:?}");
        assert_eq!(list(&pool).await.unwrap().len(), 1);
    }

    #[sqlx::test]
    async fn users_are_listed_oldest_first_and_can_change_role(pool: PgPool) {
        let first = testing::user(&pool, Role::Client).await;
        let second = testing::user(&pool, Role::Client).await;

        let promoted = set_role(&pool, second.id, Role::Operator)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(promoted.role, Role::Operator);
        assert_eq!(list(&pool).await.unwrap(), [first, promoted]);

        assert_eq!(
            set_role(&pool, UserId::generate(), Role::Admin)
                .await
                .unwrap(),
            None
        );
    }

    #[sqlx::test]
    async fn a_telegram_user_gets_an_account_the_first_time_and_the_same_one_after(pool: PgPool) {
        assert_eq!(find_by_telegram(&pool, 4_242).await.unwrap(), None);

        let made = find_or_create_by_telegram(&pool, 4_242).await.unwrap();
        // A client's, that no address and no password lead to.
        assert_eq!(
            (made.email.as_deref(), made.telegram_id, made.role),
            (None, Some(4_242), Role::Client)
        );
        assert_eq!(get(&pool, made.id).await.unwrap(), Some(made.clone()));
        assert_eq!(
            find_by_telegram(&pool, 4_242).await.unwrap(),
            Some(made.clone())
        );

        let again = find_or_create_by_telegram(&pool, 4_242).await.unwrap();
        assert_eq!(again, made);
        // Somebody else is somebody else. The largest id Telegram can give fits.
        let largest = (1_i64 << 52) - 1;
        let other = find_or_create_by_telegram(&pool, largest).await.unwrap();
        assert_ne!(other.id, made.id);
        assert_eq!(other.telegram_id, Some(largest));
        assert_eq!(list(&pool).await.unwrap(), [made.clone(), other]);

        // What an admin has made of the account is what the bot is told from then on.
        set_role(&pool, made.id, Role::Operator).await.unwrap();
        let promoted = find_or_create_by_telegram(&pool, 4_242).await.unwrap();
        assert_eq!((promoted.id, promoted.role), (made.id, Role::Operator));
    }

    /// The first two things somebody sends the bot may be handled at the same moment.
    #[sqlx::test]
    async fn several_first_messages_at_once_make_one_account(pool: PgPool) {
        let asking: Vec<_> = (0..8)
            .map(|_| {
                let pool = pool.clone();
                tokio::spawn(async move { find_or_create_by_telegram(&pool, 4_242).await })
            })
            .collect();
        let mut given = Vec::new();
        for asked in asking {
            given.push(testing::soon(asked).await.unwrap().unwrap());
        }

        assert!(given.iter().all(|user| *user == given[0]), "{given:#?}");
        assert_eq!(list(&pool).await.unwrap(), [given[0].clone()]);
    }

    /// The race above, with its loser picked by hand: a caller that looked before the
    /// winner had committed, found nobody, and goes on to make the account.
    #[sqlx::test]
    async fn an_account_that_was_made_meanwhile_is_the_one_that_is_given(pool: PgPool) {
        let winner = find_or_create_by_telegram(&pool, 4_242).await.unwrap();
        set_role(&pool, winner.id, Role::Operator).await.unwrap();

        let given = make_for_telegram(&pool, 4_242).await.unwrap();

        // The winner's account, as it is by now, and no second one.
        assert_eq!((given.id, given.role), (winner.id, Role::Operator));
        assert_eq!(list(&pool).await.unwrap(), [given]);
    }

    #[sqlx::test]
    async fn the_schema_holds_an_account_to_a_way_of_reaching_it(pool: PgPool) {
        let refused = async |sql: &'static str| -> String {
            let error = sqlx::query(sql)
                .bind(UserId::generate().0)
                .execute(&pool)
                .await
                .expect_err(sql);
            let sqlx::Error::Database(database) = error else {
                panic!("{sql}: {error:?}");
            };
            database
                .constraint()
                .expect("a constraint was violated")
                .to_owned()
        };

        // Neither an address nor a Telegram id: nobody could ever use it.
        assert_eq!(
            refused("INSERT INTO users (id, role) VALUES ($1, 'client')").await,
            "users_reachable_check"
        );
        // An address without a password, and a password without an address.
        assert_eq!(
            refused("INSERT INTO users (id, email, role) VALUES ($1, 'a@example.com', 'client')")
                .await,
            "users_password_check"
        );
        assert_eq!(
            refused(
                "INSERT INTO users (id, telegram_id, password_hash, role)
                 VALUES ($1, 7, 'hash', 'client')"
            )
            .await,
            "users_password_check"
        );
        for id in [0, -7] {
            let error = find_or_create_by_telegram(&pool, id).await.unwrap_err();
            assert!(
                matches!(&error, DbError::Sqlx(sqlx::Error::Database(database))
                    if database.constraint() == Some("users_telegram_id_check")),
                "{id}: {error:?}"
            );
        }
        find_or_create_by_telegram(&pool, 7).await.unwrap();
        assert_eq!(
            refused("INSERT INTO users (id, telegram_id, role) VALUES ($1, 7, 'client')").await,
            "users_telegram_id_key"
        );
        assert_eq!(list(&pool).await.unwrap().len(), 1);

        // An account may be reached both ways. Nothing makes one yet.
        sqlx::query(
            "INSERT INTO users (id, email, password_hash, telegram_id, role)
             VALUES ($1, 'both@example.com', 'hash', 8, 'client')",
        )
        .bind(UserId::generate().0)
        .execute(&pool)
        .await
        .unwrap();
        let both = find_or_create_by_telegram(&pool, 8).await.unwrap();
        assert_eq!(both.email.as_deref(), Some("both@example.com"));
    }

    /// The schema repeats the values of every enumeration in a CHECK. This one and its
    /// siblings in the other modules store each value, so the two cannot drift apart.
    #[sqlx::test]
    async fn the_schema_accepts_every_role(pool: PgPool) {
        for (number, &role) in Role::ALL.iter().enumerate() {
            let user = create(&pool, &new_user(&format!("user{number}@example.com"), role))
                .await
                .unwrap();
            assert_eq!(user.role, role);
        }
    }

    #[sqlx::test]
    async fn a_role_this_build_does_not_know_is_an_error_not_a_panic(pool: PgPool) {
        let user = testing::user(&pool, Role::Client).await;
        sqlx::query("ALTER TABLE users DROP CONSTRAINT users_role_check")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE users SET role = 'courier' WHERE id = $1")
            .bind(user.id.0)
            .execute(&pool)
            .await
            .unwrap();

        let error = get(&pool, user.id).await.unwrap_err();
        assert!(
            matches!(&error, DbError::Unreadable(what) if what.contains("courier")),
            "{error:?}"
        );
    }
}
