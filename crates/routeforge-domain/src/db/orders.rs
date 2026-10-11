//! Orders.
//!
//! An order is pending until a plan takes it or its client withdraws it (D20). The two
//! steps a plan takes, [`lock_pending`] and then [`mark_planned`], belong in one
//! transaction, so that an order cannot be cancelled between being counted into a
//! cluster and being marked.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgConnection, PgExecutor};
use uuid::Uuid;

use super::{DbError, parsed, unsigned};
use crate::geo::LatLon;
use crate::ids::{OrderId, PlanId};
use crate::model::{NewOrder, Order, OrderFilter, OrderStatus};

struct OrderRow {
    id: Uuid,
    client_id: Uuid,
    kind: String,
    status: String,
    service_date: NaiveDate,
    lat: f64,
    lon: f64,
    address: Option<String>,
    demand: i64,
    plan_id: Option<Uuid>,
    created_at: DateTime<Utc>,
}

impl TryFrom<OrderRow> for Order {
    type Error = DbError;

    fn try_from(row: OrderRow) -> Result<Self, DbError> {
        Ok(Self {
            id: row.id.into(),
            client_id: row.client_id.into(),
            kind: parsed(&row.kind)?,
            status: parsed(&row.status)?,
            service_date: row.service_date,
            location: LatLon::new(row.lat, row.lon)?,
            address: row.address,
            demand: unsigned(row.demand, "orders.demand")?,
            plan_id: row.plan_id.map(PlanId),
            created_at: row.created_at,
        })
    }
}

fn orders(rows: Vec<OrderRow>) -> Result<Vec<Order>, DbError> {
    rows.into_iter().map(Order::try_from).collect()
}

/// Stores a new order as pending.
///
/// The schema refuses a demand of 0.
pub async fn create(executor: impl PgExecutor<'_>, new: &NewOrder) -> Result<Order, DbError> {
    sqlx::query_as!(
        OrderRow,
        "INSERT INTO orders (id, client_id, kind, service_date, lat, lon, address, demand)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         RETURNING id, client_id, kind, status, service_date, lat, lon, address, demand,
                   plan_id, created_at",
        OrderId::generate().0,
        new.client_id.0,
        new.kind.as_str(),
        new.service_date,
        new.location.lat(),
        new.location.lon(),
        new.address,
        i64::from(new.demand),
    )
    .fetch_one(executor)
    .await?
    .try_into()
}

pub async fn get(executor: impl PgExecutor<'_>, id: OrderId) -> Result<Option<Order>, DbError> {
    sqlx::query_as!(
        OrderRow,
        "SELECT id, client_id, kind, status, service_date, lat, lon, address, demand,
                plan_id, created_at
         FROM orders
         WHERE id = $1",
        id.0,
    )
    .fetch_optional(executor)
    .await?
    .map(Order::try_from)
    .transpose()
}

/// The orders that pass the filter, oldest first.
pub async fn list(
    executor: impl PgExecutor<'_>,
    filter: &OrderFilter,
) -> Result<Vec<Order>, DbError> {
    let rows = sqlx::query_as!(
        OrderRow,
        "SELECT id, client_id, kind, status, service_date, lat, lon, address, demand,
                plan_id, created_at
         FROM orders
         WHERE ($1::uuid IS NULL OR client_id = $1)
           AND ($2::date IS NULL OR service_date = $2)
           AND ($3::text IS NULL OR status = $3)
         ORDER BY id",
        filter.client_id.map(|id| id.0),
        filter.service_date,
        filter.status.map(OrderStatus::as_str),
    )
    .fetch_all(executor)
    .await?;
    orders(rows)
}

/// The orders with these ids, oldest first. An id that names no order is left out.
///
/// This is how a route job reads its stops.
pub async fn list_by_ids(
    executor: impl PgExecutor<'_>,
    ids: &[OrderId],
) -> Result<Vec<Order>, DbError> {
    let ids: Vec<Uuid> = ids.iter().map(|id| id.0).collect();
    let rows = sqlx::query_as!(
        OrderRow,
        "SELECT id, client_id, kind, status, service_date, lat, lon, address, demand,
                plan_id, created_at
         FROM orders
         WHERE id = ANY($1)
         ORDER BY id",
        &ids,
    )
    .fetch_all(executor)
    .await?;
    orders(rows)
}

/// Withdraws an order. Only a pending order can be withdrawn.
///
/// Returns `false`, and changes nothing, if the order is planned, already cancelled or
/// does not exist.
pub async fn cancel(executor: impl PgExecutor<'_>, id: OrderId) -> Result<bool, DbError> {
    let cancelled = sqlx::query!(
        "UPDATE orders SET status = 'cancelled' WHERE id = $1 AND status = 'pending'",
        id.0,
    )
    .execute(executor)
    .await?;
    Ok(cancelled.rows_affected() == 1)
}

/// The pending orders of a service date, oldest first, locked until the transaction ends.
///
/// This is how a plan takes its orders: while the lock is held none of them can be
/// cancelled or taken by another plan. It takes a connection and not any executor,
/// because outside a transaction the lock would be gone before the function returned.
pub async fn lock_pending(
    conn: &mut PgConnection,
    service_date: NaiveDate,
) -> Result<Vec<Order>, DbError> {
    let rows = sqlx::query_as!(
        OrderRow,
        "SELECT id, client_id, kind, status, service_date, lat, lon, address, demand,
                plan_id, created_at
         FROM orders
         WHERE service_date = $1 AND status = 'pending'
         ORDER BY id
         FOR UPDATE",
        service_date,
    )
    .fetch_all(conn)
    .await?;
    orders(rows)
}

/// Gives pending orders to a plan. Returns how many were marked.
///
/// Orders that are not pending are left alone, so a caller that did not lock them
/// first can tell from the count that some had gone.
pub async fn mark_planned(
    executor: impl PgExecutor<'_>,
    plan_id: PlanId,
    order_ids: &[OrderId],
) -> Result<u64, DbError> {
    let ids: Vec<Uuid> = order_ids.iter().map(|id| id.0).collect();
    let marked = sqlx::query!(
        "UPDATE orders SET status = 'planned', plan_id = $1
         WHERE id = ANY($2) AND status = 'pending'",
        plan_id.0,
        &ids,
    )
    .execute(executor)
    .await?;
    Ok(marked.rows_affected())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sqlx::PgPool;

    use super::*;
    use crate::db::testing::{self, date};
    use crate::ids::UserId;
    use crate::model::{OrderKind, Role};

    #[sqlx::test]
    async fn an_order_can_be_stored_and_read_back(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let new = NewOrder {
            client_id: client.id,
            kind: OrderKind::Pickup,
            service_date: date(12),
            location: LatLon::new(50.4547, 30.5238).unwrap(),
            address: Some("Khreshchatyk St, 22".to_owned()),
            demand: 3,
        };
        let created = create(&pool, &new).await.unwrap();

        assert_eq!(created.client_id, client.id);
        assert_eq!(created.kind, OrderKind::Pickup);
        assert_eq!(created.status, OrderStatus::Pending);
        assert_eq!(created.service_date, date(12));
        assert_eq!(created.location, new.location);
        assert_eq!(created.address, new.address);
        assert_eq!(created.demand, 3);
        assert_eq!(created.plan_id, None);
        assert_eq!(get(&pool, created.id).await.unwrap(), Some(created));
        assert_eq!(get(&pool, OrderId::generate()).await.unwrap(), None);
    }

    #[sqlx::test]
    async fn an_order_needs_a_client_and_a_demand(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let mut new = testing::new_order(client.id, date(12));

        new.demand = 0;
        let error = create(&pool, &new).await.unwrap_err();
        assert!(
            matches!(&error, DbError::Sqlx(sqlx::Error::Database(database))
                if database.constraint() == Some("orders_demand_check")),
            "{error:?}"
        );

        new.demand = u32::MAX;
        assert_eq!(create(&pool, &new).await.unwrap().demand, u32::MAX);

        new.client_id = UserId::generate();
        let error = create(&pool, &new).await.unwrap_err();
        assert!(
            matches!(&error, DbError::Sqlx(sqlx::Error::Database(database))
                if database.is_foreign_key_violation()),
            "{error:?}"
        );
    }

    #[sqlx::test]
    async fn the_schema_accepts_every_kind_and_status_of_order(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let plan = testing::plan(&pool, date(12)).await;

        for &kind in OrderKind::ALL {
            let mut new = testing::new_order(client.id, date(12));
            new.kind = kind;
            assert_eq!(create(&pool, &new).await.unwrap().kind, kind);
        }

        for &status in OrderStatus::ALL {
            let order = testing::order(&pool, client.id, date(12)).await;
            match status {
                OrderStatus::Pending => {}
                OrderStatus::Planned => {
                    assert_eq!(mark_planned(&pool, plan.id, &[order.id]).await.unwrap(), 1);
                }
                OrderStatus::Cancelled => assert!(cancel(&pool, order.id).await.unwrap()),
            }
            assert_eq!(get(&pool, order.id).await.unwrap().unwrap().status, status);
        }
    }

    #[sqlx::test]
    async fn every_filter_narrows_the_list(pool: PgPool) {
        let ada = testing::user(&pool, Role::Client).await;
        let bob = testing::user(&pool, Role::Client).await;
        let ada_monday = testing::order(&pool, ada.id, date(12)).await;
        let ada_tuesday = testing::order(&pool, ada.id, date(13)).await;
        let bob_monday = testing::order(&pool, bob.id, date(12)).await;
        let bob_withdrawn = testing::order(&pool, bob.id, date(12)).await;
        assert!(cancel(&pool, bob_withdrawn.id).await.unwrap());

        let ids = async |filter: OrderFilter| -> Vec<OrderId> {
            let listed = list(&pool, &filter).await.unwrap();
            listed.into_iter().map(|order| order.id).collect()
        };

        assert_eq!(
            ids(OrderFilter::default()).await,
            [
                ada_monday.id,
                ada_tuesday.id,
                bob_monday.id,
                bob_withdrawn.id
            ]
        );
        assert_eq!(
            ids(OrderFilter {
                client_id: Some(ada.id),
                ..OrderFilter::default()
            })
            .await,
            [ada_monday.id, ada_tuesday.id]
        );
        assert_eq!(
            ids(OrderFilter {
                service_date: Some(date(12)),
                status: Some(OrderStatus::Pending),
                ..OrderFilter::default()
            })
            .await,
            [ada_monday.id, bob_monday.id]
        );
        assert_eq!(
            ids(OrderFilter {
                client_id: Some(bob.id),
                service_date: Some(date(12)),
                status: Some(OrderStatus::Cancelled),
            })
            .await,
            [bob_withdrawn.id]
        );
        assert!(
            ids(OrderFilter {
                service_date: Some(date(14)),
                ..OrderFilter::default()
            })
            .await
            .is_empty()
        );
    }

    #[sqlx::test]
    async fn orders_can_be_read_by_their_ids(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let first = testing::order(&pool, client.id, date(12)).await;
        let _between = testing::order(&pool, client.id, date(12)).await;
        let other_day = testing::order(&pool, client.id, date(13)).await;

        // In the order they were created, whatever the order they were asked for in,
        // and without the id that names no order.
        let asked = [other_day.id, OrderId::generate(), first.id];
        assert_eq!(
            list_by_ids(&pool, &asked).await.unwrap(),
            [first, other_day]
        );
        assert!(list_by_ids(&pool, &[]).await.unwrap().is_empty());
    }

    #[sqlx::test]
    async fn only_a_pending_order_can_be_withdrawn(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let plan = testing::plan(&pool, date(12)).await;
        let pending = testing::order(&pool, client.id, date(12)).await;
        let planned = testing::order(&pool, client.id, date(12)).await;
        mark_planned(&pool, plan.id, &[planned.id]).await.unwrap();

        assert!(cancel(&pool, pending.id).await.unwrap());
        assert!(
            !cancel(&pool, pending.id).await.unwrap(),
            "it is cancelled already"
        );
        assert!(!cancel(&pool, planned.id).await.unwrap(), "a plan holds it");
        assert!(!cancel(&pool, OrderId::generate()).await.unwrap());

        let planned = get(&pool, planned.id).await.unwrap().unwrap();
        assert_eq!(
            (planned.status, planned.plan_id),
            (OrderStatus::Planned, Some(plan.id))
        );
    }

    #[sqlx::test]
    async fn a_plan_takes_the_pending_orders_of_its_date_and_no_others(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let plan = testing::plan(&pool, date(12)).await;
        let first = testing::order(&pool, client.id, date(12)).await;
        let second = testing::order(&pool, client.id, date(12)).await;
        let withdrawn = testing::order(&pool, client.id, date(12)).await;
        let other_day = testing::order(&pool, client.id, date(13)).await;
        cancel(&pool, withdrawn.id).await.unwrap();

        let mut tx = pool.begin().await.unwrap();
        let locked = lock_pending(&mut tx, date(12)).await.unwrap();
        assert_eq!(locked, [first.clone(), second.clone()]);

        // The withdrawn order and the stranger are counted out, not marked.
        let asked = [first.id, second.id, withdrawn.id, OrderId::generate()];
        assert_eq!(mark_planned(&mut *tx, plan.id, &asked).await.unwrap(), 2);
        tx.commit().await.unwrap();

        for order in [first, second] {
            let stored = get(&pool, order.id).await.unwrap().unwrap();
            assert_eq!(
                (stored.status, stored.plan_id),
                (OrderStatus::Planned, Some(plan.id))
            );
        }
        let untouched = get(&pool, other_day.id).await.unwrap().unwrap();
        assert_eq!(untouched.status, OrderStatus::Pending);

        // What a plan has taken is not offered to the next one.
        let mut tx = pool.begin().await.unwrap();
        assert!(lock_pending(&mut tx, date(12)).await.unwrap().is_empty());
    }

    /// The race the lock is there for: a client withdraws an order while a plan is
    /// counting it into a cluster.
    #[sqlx::test]
    async fn an_order_cannot_be_withdrawn_while_a_plan_is_taking_it(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let plan = testing::plan(&pool, date(12)).await;
        let order = testing::order(&pool, client.id, date(12)).await;

        let mut planning = pool.begin().await.unwrap();
        assert_eq!(
            lock_pending(&mut planning, date(12)).await.unwrap().len(),
            1
        );

        let withdrawal = tokio::spawn({
            let pool = pool.clone();
            async move { cancel(&pool, order.id).await.unwrap() }
        });
        // The withdrawal now waits for the lock. Give it time to get that far.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!withdrawal.is_finished());

        mark_planned(&mut *planning, plan.id, &[order.id])
            .await
            .unwrap();
        planning.commit().await.unwrap();

        assert!(!withdrawal.await.unwrap(), "the plan was first");
        let stored = get(&pool, order.id).await.unwrap().unwrap();
        assert_eq!(stored.status, OrderStatus::Planned);
    }
}
