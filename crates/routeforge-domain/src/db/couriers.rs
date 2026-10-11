//! Couriers.

use chrono::{DateTime, Utc};
use sqlx::PgExecutor;
use uuid::Uuid;

use super::{DbError, unsigned};
use crate::ids::CourierId;
use crate::model::{Courier, NewCourier};

struct CourierRow {
    id: Uuid,
    name: String,
    capacity: i64,
    active: bool,
    created_at: DateTime<Utc>,
}

impl TryFrom<CourierRow> for Courier {
    type Error = DbError;

    fn try_from(row: CourierRow) -> Result<Self, DbError> {
        Ok(Self {
            id: row.id.into(),
            name: row.name,
            capacity: unsigned(row.capacity, "couriers.capacity")?,
            active: row.active,
            created_at: row.created_at,
        })
    }
}

/// Stores a new courier, active from the start.
///
/// The schema refuses a capacity of 0.
pub async fn create(executor: impl PgExecutor<'_>, new: &NewCourier) -> Result<Courier, DbError> {
    sqlx::query_as!(
        CourierRow,
        "INSERT INTO couriers (id, name, capacity)
         VALUES ($1, $2, $3)
         RETURNING id, name, capacity, active, created_at",
        CourierId::generate().0,
        new.name,
        i64::from(new.capacity),
    )
    .fetch_one(executor)
    .await?
    .try_into()
}

pub async fn get(executor: impl PgExecutor<'_>, id: CourierId) -> Result<Option<Courier>, DbError> {
    sqlx::query_as!(
        CourierRow,
        "SELECT id, name, capacity, active, created_at FROM couriers WHERE id = $1",
        id.0,
    )
    .fetch_optional(executor)
    .await?
    .map(Courier::try_from)
    .transpose()
}

/// Every courier, oldest first.
pub async fn list(executor: impl PgExecutor<'_>) -> Result<Vec<Courier>, DbError> {
    sqlx::query_as!(
        CourierRow,
        "SELECT id, name, capacity, active, created_at FROM couriers ORDER BY id",
    )
    .fetch_all(executor)
    .await?
    .into_iter()
    .map(Courier::try_from)
    .collect()
}

/// The couriers a plan may give routes to, oldest first.
pub async fn list_active(executor: impl PgExecutor<'_>) -> Result<Vec<Courier>, DbError> {
    sqlx::query_as!(
        CourierRow,
        "SELECT id, name, capacity, active, created_at FROM couriers WHERE active ORDER BY id",
    )
    .fetch_all(executor)
    .await?
    .into_iter()
    .map(Courier::try_from)
    .collect()
}

/// Takes a courier out of planning or puts one back. `None` if there is no such courier.
///
/// A courier is never deleted, because stored routes refer to it.
pub async fn set_active(
    executor: impl PgExecutor<'_>,
    id: CourierId,
    active: bool,
) -> Result<Option<Courier>, DbError> {
    sqlx::query_as!(
        CourierRow,
        "UPDATE couriers SET active = $2 WHERE id = $1
         RETURNING id, name, capacity, active, created_at",
        id.0,
        active,
    )
    .fetch_optional(executor)
    .await?
    .map(Courier::try_from)
    .transpose()
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    fn courier(name: &str, capacity: u32) -> NewCourier {
        NewCourier {
            name: name.to_owned(),
            capacity,
        }
    }

    #[sqlx::test]
    async fn a_courier_can_be_stored_and_read_back(pool: PgPool) {
        let created = create(&pool, &courier("Van 1", 40)).await.unwrap();
        assert_eq!((created.name.as_str(), created.capacity), ("Van 1", 40));
        assert!(created.active);
        assert_eq!(get(&pool, created.id).await.unwrap(), Some(created));
        assert_eq!(get(&pool, CourierId::generate()).await.unwrap(), None);
    }

    #[sqlx::test]
    async fn every_capacity_a_u32_can_hold_can_be_stored_except_zero(pool: PgPool) {
        let largest = create(&pool, &courier("Freight train", u32::MAX))
            .await
            .unwrap();
        assert_eq!(largest.capacity, u32::MAX);

        let error = create(&pool, &courier("Bicycle without a basket", 0))
            .await
            .unwrap_err();
        assert!(
            matches!(&error, DbError::Sqlx(sqlx::Error::Database(database))
                if database.constraint() == Some("couriers_capacity_check")),
            "{error:?}"
        );
    }

    #[sqlx::test]
    async fn only_active_couriers_are_offered_for_planning(pool: PgPool) {
        let first = create(&pool, &courier("Van 1", 40)).await.unwrap();
        let second = create(&pool, &courier("Van 2", 25)).await.unwrap();

        let resting = set_active(&pool, first.id, false).await.unwrap().unwrap();
        assert!(!resting.active);
        assert_eq!(
            list_active(&pool).await.unwrap(),
            std::slice::from_ref(&second)
        );
        assert_eq!(list(&pool).await.unwrap(), [resting, second]);

        let back = set_active(&pool, first.id, true).await.unwrap().unwrap();
        assert!(back.active);
        assert_eq!(list_active(&pool).await.unwrap().len(), 2);

        assert_eq!(
            set_active(&pool, CourierId::generate(), false)
                .await
                .unwrap(),
            None
        );
    }
}
