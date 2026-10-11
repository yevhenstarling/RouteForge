//! The depot.
//!
//! There is one depot at a time, and its rows are versions (D21): [`set`] adds a row
//! and never changes one. A plan refers to the version it was made for, so the routes
//! of one plan all start at the same place and an old plan keeps its starting point.

use chrono::{DateTime, Utc};
use sqlx::PgExecutor;
use uuid::Uuid;

use super::DbError;
use crate::geo::LatLon;
use crate::ids::DepotId;
use crate::model::{Depot, NewDepot};

struct DepotRow {
    id: Uuid,
    name: String,
    lat: f64,
    lon: f64,
    created_at: DateTime<Utc>,
}

impl TryFrom<DepotRow> for Depot {
    type Error = DbError;

    fn try_from(row: DepotRow) -> Result<Self, DbError> {
        Ok(Self {
            id: row.id.into(),
            name: row.name,
            location: LatLon::new(row.lat, row.lon)?,
            created_at: row.created_at,
        })
    }
}

/// Makes this the depot from now on.
pub async fn set(executor: impl PgExecutor<'_>, new: &NewDepot) -> Result<Depot, DbError> {
    sqlx::query_as!(
        DepotRow,
        "INSERT INTO depots (id, name, lat, lon)
         VALUES ($1, $2, $3, $4)
         RETURNING id, name, lat, lon, created_at",
        DepotId::generate().0,
        new.name,
        new.location.lat(),
        new.location.lon(),
    )
    .fetch_one(executor)
    .await?
    .try_into()
}

/// The depot as it is now. `None` until one has been set.
pub async fn current(executor: impl PgExecutor<'_>) -> Result<Option<Depot>, DbError> {
    sqlx::query_as!(
        DepotRow,
        "SELECT id, name, lat, lon, created_at
         FROM depots
         ORDER BY created_at DESC, id DESC
         LIMIT 1",
    )
    .fetch_optional(executor)
    .await?
    .map(Depot::try_from)
    .transpose()
}

/// One version of the depot, current or not: the one a plan refers to.
pub async fn get(executor: impl PgExecutor<'_>, id: DepotId) -> Result<Option<Depot>, DbError> {
    sqlx::query_as!(
        DepotRow,
        "SELECT id, name, lat, lon, created_at FROM depots WHERE id = $1",
        id.0,
    )
    .fetch_optional(executor)
    .await?
    .map(Depot::try_from)
    .transpose()
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    fn depot(name: &str, lat: f64, lon: f64) -> NewDepot {
        NewDepot {
            name: name.to_owned(),
            location: LatLon::new(lat, lon).unwrap(),
        }
    }

    #[sqlx::test]
    async fn there_is_no_depot_until_one_is_set(pool: PgPool) {
        assert_eq!(current(&pool).await.unwrap(), None);
        assert_eq!(get(&pool, DepotId::generate()).await.unwrap(), None);
    }

    #[sqlx::test]
    async fn moving_the_depot_keeps_the_old_one_for_the_plans_made_from_it(pool: PgPool) {
        let old = set(&pool, &depot("Left bank", 50.45, 30.60)).await.unwrap();
        assert_eq!(current(&pool).await.unwrap(), Some(old.clone()));

        let new = set(&pool, &depot("Right bank", 50.45, 30.45))
            .await
            .unwrap();
        assert_eq!(new.location, LatLon::new(50.45, 30.45).unwrap());
        assert_eq!(current(&pool).await.unwrap(), Some(new));
        assert_eq!(get(&pool, old.id).await.unwrap(), Some(old));
    }

    /// Both rows get the transaction's timestamp, so the id has to decide.
    #[sqlx::test]
    async fn the_later_of_two_depots_set_in_one_transaction_is_current(pool: PgPool) {
        let mut tx = pool.begin().await.unwrap();
        set(&mut *tx, &depot("First", 50.0, 30.0)).await.unwrap();
        let second = set(&mut *tx, &depot("Second", 51.0, 31.0)).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(current(&pool).await.unwrap(), Some(second));
    }
}
