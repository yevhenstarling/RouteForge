//! Users.

use chrono::{DateTime, Utc};
use sqlx::PgExecutor;
use uuid::Uuid;

use super::{DbError, on_violation, parsed};
use crate::ids::UserId;
use crate::model::{Credentials, NewUser, Role, User};

struct UserRow {
    id: Uuid,
    email: String,
    role: String,
    created_at: DateTime<Utc>,
}

impl TryFrom<UserRow> for User {
    type Error = DbError;

    fn try_from(row: UserRow) -> Result<Self, DbError> {
        Ok(Self {
            id: row.id.into(),
            email: row.email,
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
         RETURNING id, email, role, created_at",
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
        "SELECT id, email, role, created_at FROM users WHERE id = $1",
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
    let Some(row) = sqlx::query!(
        "SELECT id, email, role, created_at, password_hash FROM users WHERE email = $1",
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
        role: row.role,
        created_at: row.created_at,
    };
    Ok(Some(Credentials {
        user: user.try_into()?,
        password_hash: row.password_hash,
    }))
}

/// Every user, oldest first.
pub async fn list(executor: impl PgExecutor<'_>) -> Result<Vec<User>, DbError> {
    sqlx::query_as!(
        UserRow,
        "SELECT id, email, role, created_at FROM users ORDER BY id",
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
        "UPDATE users SET role = $2 WHERE id = $1 RETURNING id, email, role, created_at",
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
        assert_eq!(created.email, "ada@example.com");
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
