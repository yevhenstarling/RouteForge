//! Orders.
//!
//! An order is pending until a plan takes it or its client withdraws it (D20). The two
//! steps a plan takes, [`lock_pending`] and then [`mark_planned`], belong in one
//! transaction, so that an order cannot be cancelled between being counted into a
//! cluster and being marked.
//!
//! An order that is placed or withdrawn says so on [`ORDERS_CHANNEL`], for whoever
//! shows a day's orders to a person (D63). What a plan does to an order is announced
//! by the plan (D48).
//!
//! An order can be placed with a key of the client's making. Asked for again with the
//! same key, it is not placed again: see [`place`] (D76).

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgConnection, PgExecutor};
use uuid::Uuid;

use super::{DbError, parsed, unsigned};
use crate::geo::LatLon;
use crate::ids::{IdempotencyKey, OrderId, PlanId, UserId};
use crate::model::{NewOrder, Order, OrderFilter, OrderStatus};

/// The channel that says the orders of a day have changed: one was placed, or one was
/// withdrawn (D63).
///
/// The payload is the day, written as `2026-10-12`, and nothing more. So the
/// notifications of one transaction for one day are equal, and PostgreSQL folds them
/// into one (D26): a day of two thousand demo orders is one word. Like every
/// notification it is a hint. Whoever hears it reads the orders of that day again.
pub const ORDERS_CHANNEL: &str = "routeforge_orders";

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

/// What came of placing an order with a key (D76).
#[derive(Clone, Debug, PartialEq)]
pub enum Placed {
    /// The order was stored, by this request.
    Now(Order),
    /// The client had placed an order with this key already. This is that order, as it
    /// stands now, and nothing was stored.
    Before(Order),
}

impl Placed {
    /// The order, whichever request stored it.
    pub fn into_order(self) -> Order {
        match self {
            Self::Now(order) | Self::Before(order) => order,
        }
    }
}

/// Stores a new order as pending, and tells the watchers of its day.
///
/// It is one statement, and the notification is sent when the transaction it runs in
/// commits: an order is heard of when it can be read, and one that was rolled back is
/// not heard of at all. The schema refuses a demand of 0.
pub async fn create(executor: impl PgExecutor<'_>, new: &NewOrder) -> Result<Order, DbError> {
    // Without a key every request is an order of its own.
    Ok(place(executor, new, None).await?.into_order())
}

/// Stores a new order as [`create`] does, once for each key of a client (D76).
///
/// With a key the client has placed an order with before, nothing is stored and nobody
/// is told: the answer is that order, as [`Placed::Before`]. Whether it is the order
/// this request asks for is for the caller to say: see [`NewOrder::asks_for`].
///
/// It is one statement still. Two requests with one key that arrive together are one
/// order: the second waits for the first to commit, and is then answered with its row.
pub async fn place(
    executor: impl PgExecutor<'_>,
    new: &NewOrder,
    key: Option<&IdempotencyKey>,
) -> Result<Placed, DbError> {
    let id = OrderId::generate();
    // A key that is taken is no error but an answer, so the conflict updates the row
    // that has it, to what it is already. `DO NOTHING` would return no row at all to
    // the request that came second (D59).
    //
    // The row that comes back is this request's own if it has the id that was sent,
    // and only then is anybody told. The day is written out by `to_char`, so that it
    // reads the same whatever the session's DateStyle is.
    let placed = sqlx::query!(
        r#"WITH placed AS (
               INSERT INTO orders (id, client_id, kind, service_date, lat, lon, address, demand,
                                   idempotency_key)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
               ON CONFLICT (client_id, idempotency_key) WHERE idempotency_key IS NOT NULL
               DO UPDATE SET idempotency_key = orders.idempotency_key
               RETURNING id, client_id, kind, status, service_date, lat, lon, address, demand,
                         plan_id, created_at
           )
           SELECT id AS "id!", client_id AS "client_id!", kind AS "kind!", status AS "status!",
                  service_date AS "service_date!", lat AS "lat!", lon AS "lon!", address,
                  demand AS "demand!", plan_id, created_at AS "created_at!",
                  CASE WHEN id = $1
                       THEN pg_notify($10, to_char(service_date, 'YYYY-MM-DD'))
                  END AS watchers
           FROM placed"#,
        id.0,
        new.client_id.0,
        new.kind.as_str(),
        new.service_date,
        new.location.lat(),
        new.location.lon(),
        new.address,
        i64::from(new.demand),
        key.map(IdempotencyKey::as_str),
        ORDERS_CHANNEL,
    )
    .fetch_one(executor)
    .await?;
    let order = Order::try_from(OrderRow {
        id: placed.id,
        client_id: placed.client_id,
        kind: placed.kind,
        status: placed.status,
        service_date: placed.service_date,
        lat: placed.lat,
        lon: placed.lon,
        address: placed.address,
        demand: placed.demand,
        plan_id: placed.plan_id,
        created_at: placed.created_at,
    })?;
    Ok(if order.id == id {
        Placed::Now(order)
    } else {
        Placed::Before(order)
    })
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
///
/// With a `limit` it is one page of them, and the id of its last order is the `after`
/// that asks for the next page. Without one it is all of them.
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
           AND ($4::uuid IS NULL OR id > $4)
         ORDER BY id
         LIMIT $5",
        filter.client_id.map(|id| id.0),
        filter.service_date,
        filter.status.map(OrderStatus::as_str),
        filter.after.map(|id| id.0),
        // No limit is a NULL, which PostgreSQL takes as all of them.
        filter.limit.map(i64::from),
    )
    .fetch_all(executor)
    .await?;
    orders(rows)
}

/// The orders a client placed for a day, oldest first.
///
/// What the stream of somebody's own orders reads, whenever any plan changes (D77).
/// It is [`list`] with a client and a day, in a statement of its own, which the index
/// of the two serves however PostgreSQL comes to plan it.
pub async fn of_client_on(
    executor: impl PgExecutor<'_>,
    client_id: UserId,
    service_date: NaiveDate,
) -> Result<Vec<Order>, DbError> {
    let rows = sqlx::query_as!(
        OrderRow,
        "SELECT id, client_id, kind, status, service_date, lat, lon, address, demand,
                plan_id, created_at
         FROM orders
         WHERE client_id = $1 AND service_date = $2
         ORDER BY id",
        client_id.0,
        service_date,
    )
    .fetch_all(executor)
    .await?;
    orders(rows)
}

/// Stores many new orders as pending, in one statement, and says how many.
///
/// This is [`create`] for a batch, as the demonstration makes them (D46): a day of two
/// thousand orders is one round trip and not two thousand. The watchers of each day
/// are told as by `create`, and the words for one day are one when the transaction
/// commits (D63). None of the orders has a key.
pub async fn create_many(executor: impl PgExecutor<'_>, new: &[NewOrder]) -> Result<u64, DbError> {
    let ids: Vec<Uuid> = new.iter().map(|_| OrderId::generate().0).collect();
    let clients: Vec<Uuid> = new.iter().map(|order| order.client_id.0).collect();
    let kinds: Vec<&str> = new.iter().map(|order| order.kind.as_str()).collect();
    let days: Vec<NaiveDate> = new.iter().map(|order| order.service_date).collect();
    let lats: Vec<f64> = new.iter().map(|order| order.location.lat()).collect();
    let lons: Vec<f64> = new.iter().map(|order| order.location.lon()).collect();
    let addresses: Vec<Option<String>> = new.iter().map(|order| order.address.clone()).collect();
    let demands: Vec<i64> = new.iter().map(|order| i64::from(order.demand)).collect();
    let placed = sqlx::query!(
        "WITH placed AS (
             INSERT INTO orders (id, client_id, kind, service_date, lat, lon, address, demand)
             SELECT * FROM UNNEST($1::uuid[], $2::uuid[], $3::text[], $4::date[],
                                  $5::float8[], $6::float8[], $7::text[], $8::int8[])
             RETURNING service_date
         )
         SELECT pg_notify($9, to_char(service_date, 'YYYY-MM-DD')) AS watchers FROM placed",
        &ids,
        &clients,
        &kinds[..] as &[&str],
        &days,
        &lats,
        &lons,
        &addresses[..] as &[Option<String>],
        &demands,
        ORDERS_CHANNEL,
    )
    .execute(executor)
    .await?;
    Ok(placed.rows_affected())
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

/// Withdraws an order, and tells the watchers of its day. Only a pending order can be
/// withdrawn.
///
/// Returns `false`, changes nothing and tells nobody, if the order is planned, already
/// cancelled or does not exist.
pub async fn cancel(executor: impl PgExecutor<'_>, id: OrderId) -> Result<bool, DbError> {
    let cancelled = sqlx::query!(
        "WITH withdrawn AS (
             UPDATE orders SET status = 'cancelled' WHERE id = $1 AND status = 'pending'
             RETURNING service_date
         )
         SELECT pg_notify($2, to_char(service_date, 'YYYY-MM-DD')) FROM withdrawn",
        id.0,
        ORDERS_CHANNEL,
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
    use sqlx::postgres::PgListener;

    use super::*;
    use crate::db::queue::{JOBS_CHANNEL, PLANS_CHANNEL};
    use crate::db::testing::{self, date, soon};
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

    /// What the stream of somebody's own orders reads (D77): theirs, of that day, as the
    /// list with the same two filters gives them.
    #[sqlx::test]
    async fn the_orders_of_a_client_on_a_day_are_theirs_of_that_day(pool: PgPool) {
        let ada = testing::user(&pool, Role::Client).await;
        let bob = testing::user(&pool, Role::Client).await;
        let first = testing::order(&pool, ada.id, date(12)).await;
        testing::order(&pool, ada.id, date(13)).await;
        testing::order(&pool, bob.id, date(12)).await;
        let withdrawn = testing::order(&pool, ada.id, date(12)).await;
        assert!(cancel(&pool, withdrawn.id).await.unwrap());

        let own = of_client_on(&pool, ada.id, date(12)).await.unwrap();
        let ids: Vec<OrderId> = own.iter().map(|order| order.id).collect();
        assert_eq!(ids, [first.id, withdrawn.id]);
        let filter = OrderFilter {
            client_id: Some(ada.id),
            service_date: Some(date(12)),
            ..OrderFilter::default()
        };
        assert_eq!(own, list(&pool, &filter).await.unwrap());
        let nobodys = of_client_on(&pool, UserId::generate(), date(12)).await;
        assert!(nobodys.unwrap().is_empty());
    }

    /// The orders of a demonstration go in as one statement (D46).
    #[sqlx::test]
    async fn many_orders_are_stored_together_and_each_day_is_told_once(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let mut listener = PgListener::connect_with(&pool).await.unwrap();
        listener.listen(ORDERS_CHANNEL).await.unwrap();

        let mut new: Vec<NewOrder> = (0..5)
            .map(|_| testing::new_order(client.id, date(12)))
            .collect();
        new.push(NewOrder {
            kind: OrderKind::Pickup,
            address: Some("Khreshchatyk St, 22".to_owned()),
            demand: 3,
            ..testing::new_order(client.id, date(13))
        });
        assert_eq!(create_many(&pool, &new).await.unwrap(), 6);

        let theirs = OrderFilter {
            client_id: Some(client.id),
            ..OrderFilter::default()
        };
        let stored = list(&pool, &theirs).await.unwrap();
        assert_eq!(stored.len(), 6);
        assert!(
            stored
                .iter()
                .all(|order| order.status == OrderStatus::Pending)
        );
        for asked in &new {
            assert!(
                stored.iter().any(|order| asked.asks_for(order)),
                "{asked:?}"
            );
        }
        // One word for each day, as `create` would have said it in one transaction.
        let mut heard = Vec::new();
        for _ in 0..2 {
            let told = soon(listener.recv()).await.unwrap();
            heard.push(told.payload().to_owned());
        }
        heard.sort_unstable();
        assert_eq!(heard, ["2026-10-12", "2026-10-13"]);
        let more = tokio::time::timeout(Duration::from_millis(200), listener.recv()).await;
        assert!(more.is_err(), "{more:?}");

        // One statement: an order that cannot be stored keeps the others out as well.
        let mut broken = new.clone();
        broken[2].demand = 0;
        assert!(create_many(&pool, &broken).await.is_err());
        assert_eq!(list(&pool, &theirs).await.unwrap().len(), 6);
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
                ..OrderFilter::default()
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
    async fn a_list_is_read_in_pages_by_the_id_of_the_last_order(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let mut placed = Vec::new();
        for _ in 0..5 {
            placed.push(testing::order(&pool, client.id, date(12)).await.id);
        }
        // Newer than all of them, and kept out by the filter.
        testing::order(&pool, client.id, date(13)).await;

        let page = async |after: Option<OrderId>, limit: Option<u32>| -> Vec<OrderId> {
            let filter = OrderFilter {
                service_date: Some(date(12)),
                after,
                limit,
                ..OrderFilter::default()
            };
            let listed = list(&pool, &filter).await.unwrap();
            listed.into_iter().map(|order| order.id).collect()
        };

        assert_eq!(page(None, Some(2)).await, placed[..2]);
        assert_eq!(page(Some(placed[1]), Some(2)).await, placed[2..4]);
        assert_eq!(page(Some(placed[3]), Some(2)).await, placed[4..]);
        assert!(page(Some(placed[4]), Some(2)).await.is_empty());

        // Without a limit it is everything that follows, and a limit can be anything.
        assert_eq!(page(Some(placed[0]), None).await, placed[1..]);
        assert_eq!(page(None, Some(u32::MAX)).await, placed);
        assert!(page(None, Some(0)).await.is_empty());
        // An id that names no order is still a place in the order of ids.
        assert!(page(Some(OrderId::generate()), None).await.is_empty());
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

    /// Someone who shows the orders of a day, as the API does for the map (D63). It
    /// listens to everything the database announces, so that a word on a channel it
    /// was not meant for shows up as well.
    async fn watching(pool: &PgPool) -> PgListener {
        let mut watcher = PgListener::connect_with(pool).await.unwrap();
        watcher
            .listen_all([ORDERS_CHANNEL, PLANS_CHANNEL, JOBS_CHANNEL])
            .await
            .unwrap();
        watcher
    }

    /// The days a watcher was told of since it last looked. It waits until nothing
    /// more has come for a while, so that one word too many shows up as well.
    async fn told_of(watcher: &mut PgListener) -> Vec<String> {
        let silence = Duration::from_millis(300);
        let mut days = Vec::new();
        while let Ok(notification) = tokio::time::timeout(silence, watcher.recv()).await {
            let notification = notification.unwrap();
            assert_eq!(notification.channel(), ORDERS_CHANNEL);
            days.push(notification.payload().to_owned());
        }
        days
    }

    #[sqlx::test]
    async fn an_order_is_announced_when_it_is_placed_and_when_it_is_withdrawn(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let plan = testing::plan(&pool, date(12)).await;
        let mut watcher = watching(&pool).await;

        let order = testing::order(&pool, client.id, date(12)).await;
        assert_eq!(told_of(&mut watcher).await, ["2026-10-12"], "placed");
        let other_day = testing::order(&pool, client.id, date(9)).await;
        assert_eq!(told_of(&mut watcher).await, ["2026-10-09"]);

        // Nothing of an order is said before it can be read, and nothing at all of
        // one that never could.
        let mut tx = pool.begin().await.unwrap();
        create(&mut *tx, &testing::new_order(client.id, date(12)))
            .await
            .unwrap();
        assert!(told_of(&mut watcher).await.is_empty(), "before the commit");
        tx.commit().await.unwrap();
        assert_eq!(told_of(&mut watcher).await, ["2026-10-12"]);
        let mut tx = pool.begin().await.unwrap();
        create(&mut *tx, &testing::new_order(client.id, date(12)))
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        assert!(told_of(&mut watcher).await.is_empty(), "rolled back");

        assert!(cancel(&pool, order.id).await.unwrap());
        assert_eq!(told_of(&mut watcher).await, ["2026-10-12"], "withdrawn");

        // A withdrawal that changed nothing has nothing to say: of an order that is
        // withdrawn already, of one a plan holds, and of none.
        mark_planned(&pool, plan.id, &[other_day.id]).await.unwrap();
        for unchanged in [order.id, other_day.id, OrderId::generate()] {
            assert!(!cancel(&pool, unchanged).await.unwrap());
        }
        // Nor has a plan that takes an order. It announces itself, elsewhere (D48).
        assert!(told_of(&mut watcher).await.is_empty());
    }

    fn key(text: &str) -> IdempotencyKey {
        IdempotencyKey::new(text).unwrap()
    }

    /// How many orders a client has, whatever became of them.
    async fn orders_of(pool: &PgPool, client_id: UserId) -> usize {
        let filter = OrderFilter {
            client_id: Some(client_id),
            ..OrderFilter::default()
        };
        list(pool, &filter).await.unwrap().len()
    }

    #[sqlx::test]
    async fn an_order_placed_with_a_key_is_placed_once(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let new = testing::new_order(client.id, date(12));

        let Placed::Now(first) = place(&pool, &new, Some(&key("press-1"))).await.unwrap() else {
            panic!("the first request with a key places the order");
        };
        assert_eq!(first.status, OrderStatus::Pending);
        assert!(new.asks_for(&first));

        // Asked again, however often: the same order, and nothing more is stored.
        for _ in 0..3 {
            let again = place(&pool, &new, Some(&key("press-1"))).await.unwrap();
            assert_eq!(again, Placed::Before(first.clone()));
        }
        assert_eq!(orders_of(&pool, client.id).await, 1);

        // Another key is another order, and so is no key at all, each time.
        let other = place(&pool, &new, Some(&key("press-2"))).await.unwrap();
        assert!(matches!(&other, Placed::Now(order) if order.id != first.id));
        for _ in 0..2 {
            let keyless = place(&pool, &new, None).await.unwrap();
            assert!(matches!(keyless, Placed::Now(_)));
        }
        assert_eq!(orders_of(&pool, client.id).await, 4);
        // A key is told from another by its letters, to the case.
        let shouted = place(&pool, &new, Some(&key("PRESS-1"))).await.unwrap();
        assert!(matches!(shouted, Placed::Now(_)));
    }

    /// The answer to a repeat is the order the key placed, whatever the repeat asks
    /// for. Whether that is an answer to it is for the caller to say.
    #[sqlx::test]
    async fn a_repeat_is_answered_with_the_order_as_it_stands_now(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let plan = testing::plan(&pool, date(12)).await;
        let new = testing::new_order(client.id, date(12));
        let withdrawn = place(&pool, &new, Some(&key("a"))).await.unwrap();
        let planned = place(&pool, &new, Some(&key("b"))).await.unwrap();
        let (withdrawn, planned) = (withdrawn.into_order(), planned.into_order());
        assert!(cancel(&pool, withdrawn.id).await.unwrap());
        mark_planned(&pool, plan.id, &[planned.id]).await.unwrap();

        let Placed::Before(again) = place(&pool, &new, Some(&key("a"))).await.unwrap() else {
            panic!("an order that was withdrawn is still the order its key placed");
        };
        assert_eq!(
            (again.id, again.status),
            (withdrawn.id, OrderStatus::Cancelled)
        );
        let Placed::Before(again) = place(&pool, &new, Some(&key("b"))).await.unwrap() else {
            panic!("an order that a plan holds is still the order its key placed");
        };
        assert_eq!(
            (again.id, again.status, again.plan_id),
            (planned.id, OrderStatus::Planned, Some(plan.id))
        );
        // Nothing of the order was changed by being asked for again.
        assert_eq!(get(&pool, planned.id).await.unwrap(), Some(again));

        // One that asks for something else gets the first order all the same, and the
        // caller can tell that it is not what was asked for.
        let mut other = new.clone();
        other.demand = 4;
        let Placed::Before(found) = place(&pool, &other, Some(&key("a"))).await.unwrap() else {
            panic!("the key is taken");
        };
        assert_eq!((found.id, found.demand), (withdrawn.id, 1));
        assert!(!other.asks_for(&found) && new.asks_for(&found));
        assert_eq!(orders_of(&pool, client.id).await, 2);
    }

    /// Two clients who make up the same key have placed two orders, and neither is
    /// answered with the other's.
    #[sqlx::test]
    async fn a_key_is_a_clients_own(pool: PgPool) {
        let ada = testing::user(&pool, Role::Client).await;
        let bob = testing::user(&pool, Role::Client).await;
        let same = key("the-key-everybody-thinks-of");

        let adas = place(&pool, &testing::new_order(ada.id, date(12)), Some(&same)).await;
        let bobs = place(&pool, &testing::new_order(bob.id, date(12)), Some(&same)).await;
        let (Placed::Now(adas), Placed::Now(bobs)) = (adas.unwrap(), bobs.unwrap()) else {
            panic!("each of them placed an order");
        };
        assert_eq!((adas.client_id, bobs.client_id), (ada.id, bob.id));
        assert_ne!(adas.id, bobs.id);

        let again = place(&pool, &testing::new_order(bob.id, date(12)), Some(&same)).await;
        assert_eq!(again.unwrap(), Placed::Before(bobs));
    }

    /// The race the key is there for: a request is sent again while the first is
    /// still on its way.
    #[sqlx::test]
    async fn requests_with_one_key_that_arrive_together_are_one_order(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let new = testing::new_order(client.id, date(12));

        // As many at once as a test pool has connections for.
        let mut requests = Vec::new();
        for _ in 0..4 {
            let (pool, new) = (pool.clone(), new.clone());
            requests.push(tokio::spawn(async move {
                place(&pool, &new, Some(&key("pressed-twice")))
                    .await
                    .unwrap()
            }));
        }
        let mut stored = Vec::new();
        let mut found = Vec::new();
        for request in requests {
            match request.await.unwrap() {
                Placed::Now(order) => stored.push(order.id),
                Placed::Before(order) => found.push(order.id),
            }
        }
        assert_eq!(stored.len(), 1, "one of them placed the order");
        assert_eq!(found, [stored[0]; 3], "and the others were given it");
        assert_eq!(orders_of(&pool, client.id).await, 1);

        // A request that is held up by one that then comes to nothing places the
        // order itself.
        let mut first = pool.begin().await.unwrap();
        let undone = place(&mut *first, &new, Some(&key("undone")))
            .await
            .unwrap();
        assert!(matches!(undone, Placed::Now(_)));
        let second = tokio::spawn({
            let (pool, new) = (pool.clone(), new.clone());
            async move { place(&pool, &new, Some(&key("undone"))).await.unwrap() }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!second.is_finished(), "it waits to see what the first does");
        first.rollback().await.unwrap();
        let Placed::Now(placed) = second.await.unwrap() else {
            panic!("the first was rolled back, so the second is the one that placed it");
        };
        assert_ne!(placed.id, undone.into_order().id);
        assert_eq!(orders_of(&pool, client.id).await, 2);
    }

    /// What `IdempotencyKey` refuses, the schema refuses as well.
    #[sqlx::test]
    async fn the_schema_refuses_a_key_that_is_empty_or_too_long(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let store = async |key: String| {
            sqlx::query(
                "INSERT INTO orders (id, client_id, kind, service_date, lat, lon, demand,
                                     idempotency_key)
                 VALUES ($1, $2, 'delivery', DATE '2026-10-12', 50.4, 30.6, 1, $3)",
            )
            .bind(OrderId::generate().0)
            .bind(client.id.0)
            .bind(key)
            .execute(&pool)
            .await
        };

        for refused in [String::new(), "k".repeat(IdempotencyKey::LONGEST + 1)] {
            let error = store(refused).await.unwrap_err();
            assert!(
                matches!(&error, sqlx::Error::Database(database)
                    if database.constraint() == Some("orders_idempotency_key_check")),
                "{error:?}"
            );
        }
        store("k".repeat(IdempotencyKey::LONGEST)).await.unwrap();
        // And a key is stored once for a client, whoever stores it.
        let error = store("k".repeat(IdempotencyKey::LONGEST))
            .await
            .unwrap_err();
        assert!(
            matches!(&error, sqlx::Error::Database(database)
                if database.constraint() == Some("orders_client_idempotency_key")),
            "{error:?}"
        );
    }

    /// A repeat stores nothing, so there is nothing to tell anybody (D63).
    #[sqlx::test]
    async fn an_order_placed_with_a_key_is_announced_once(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let new = testing::new_order(client.id, date(12));
        let mut watcher = watching(&pool).await;

        place(&pool, &new, Some(&key("press-1"))).await.unwrap();
        assert_eq!(told_of(&mut watcher).await, ["2026-10-12"], "placed");
        for _ in 0..2 {
            let again = place(&pool, &new, Some(&key("press-1"))).await.unwrap();
            assert!(matches!(again, Placed::Before(_)));
        }
        assert!(told_of(&mut watcher).await.is_empty(), "asked for again");

        // Not for a day the repeat names either: nothing of that day has changed.
        let mut other_day = new.clone();
        other_day.service_date = date(13);
        place(&pool, &other_day, Some(&key("press-1")))
            .await
            .unwrap();
        assert!(told_of(&mut watcher).await.is_empty());

        place(&pool, &other_day, Some(&key("press-2")))
            .await
            .unwrap();
        assert_eq!(told_of(&mut watcher).await, ["2026-10-13"]);
    }

    /// The payload is the day and nothing more, so that PostgreSQL folds the
    /// notifications of one transaction into one for each day (D26).
    #[sqlx::test]
    async fn a_day_of_orders_placed_at_once_is_announced_once(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let mut watcher = watching(&pool).await;

        let mut tx = pool.begin().await.unwrap();
        for number in 0..40 {
            let day = if number % 4 == 0 { date(13) } else { date(12) };
            create(&mut *tx, &testing::new_order(client.id, day))
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();

        let mut told = told_of(&mut watcher).await;
        told.sort();
        assert_eq!(told, ["2026-10-12", "2026-10-13"]);
    }

    /// The API reads the day back out of the payload, so it is written one way,
    /// however the session that places the order writes its dates.
    #[sqlx::test]
    async fn the_day_is_announced_the_same_way_whatever_the_sessions_date_style(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let mut watcher = watching(&pool).await;

        let mut tx = pool.begin().await.unwrap();
        sqlx::query("SET LOCAL DateStyle = 'German, DMY'")
            .execute(&mut *tx)
            .await
            .unwrap();
        let written: String = sqlx::query_scalar("SELECT DATE '2026-10-12'::text")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(
            written, "12.10.2026",
            "the session writes its dates otherwise"
        );
        let order = create(&mut *tx, &testing::new_order(client.id, date(12)))
            .await
            .unwrap();
        assert!(cancel(&mut *tx, order.id).await.unwrap());
        tx.commit().await.unwrap();

        // Placed and withdrawn in one transaction: the same word twice, said once.
        assert_eq!(told_of(&mut watcher).await, ["2026-10-12"]);
    }
}
