//! Routes and their stops.
//!
//! A route is written once, when its route job completes, and is read for the map. It
//! is deleted only together with the other routes of its plan: when the plan fails or
//! is cancelled (D20), or is discarded after it was done (D30).

use sqlx::PgExecutor;
use uuid::Uuid;

use super::{DbError, parsed, unsigned};
use crate::geo::LatLon;
use crate::ids::{PlanId, RouteId};
use crate::model::{NewRoute, Route, RouteStop};

/// Stores a route and its stops in the order given.
///
/// It is one statement, so the route and its stops are stored together or not at all,
/// with or without a transaction around it.
///
/// The schema refuses a second route for the same courier in the same plan, and an
/// order that is on a route already: a route job whose results were stored twice
/// would trip one or the other.
///
/// # Errors
///
/// [`DbError::OutOfRange`] if the distance does not fit a signed 64-bit number.
pub async fn insert(executor: impl PgExecutor<'_>, new: &NewRoute) -> Result<RouteId, DbError> {
    let id = RouteId::generate();
    let distance_m =
        i64::try_from(new.distance_m).map_err(|_| DbError::OutOfRange("the route's distance"))?;
    let order_ids: Vec<Uuid> = new.stops.iter().map(|order| order.0).collect();
    sqlx::query!(
        "WITH route AS (
             INSERT INTO routes (id, plan_id, courier_id, distance_m)
             VALUES ($1, $2, $3, $4)
             RETURNING id
         )
         INSERT INTO route_stops (route_id, position, order_id)
         SELECT route.id, (stop.number - 1)::integer, stop.order_id
         FROM route, unnest($5::uuid[]) WITH ORDINALITY AS stop (order_id, number)",
        id.0,
        new.plan_id.0,
        new.courier_id.0,
        distance_m,
        &order_ids,
    )
    .execute(executor)
    .await?;
    Ok(id)
}

/// The routes of a plan with their stops in calling order, oldest route first.
pub async fn list_for_plan(
    executor: impl PgExecutor<'_>,
    plan_id: PlanId,
) -> Result<Vec<Route>, DbError> {
    // One row per stop, and one row with nulls for a route without stops.
    let rows = sqlx::query!(
        r#"SELECT routes.id, routes.courier_id, routes.distance_m,
                  stops.order_id AS "order_id?", orders.kind AS "kind?",
                  orders.lat AS "lat?", orders.lon AS "lon?", orders.demand AS "demand?"
           FROM routes
           LEFT JOIN route_stops AS stops ON stops.route_id = routes.id
           LEFT JOIN orders ON orders.id = stops.order_id
           WHERE routes.plan_id = $1
           ORDER BY routes.id, stops.position"#,
        plan_id.0,
    )
    .fetch_all(executor)
    .await?;

    let mut routes: Vec<Route> = Vec::new();
    for row in rows {
        let id = RouteId(row.id);
        if routes.last().is_none_or(|route| route.id != id) {
            let distance_m = u64::try_from(row.distance_m).map_err(|_| {
                DbError::Unreadable(format!("routes.distance_m is {}", row.distance_m))
            })?;
            routes.push(Route {
                id,
                plan_id,
                courier_id: row.courier_id.into(),
                distance_m,
                stops: Vec::new(),
            });
        }
        let (Some(order_id), Some(kind), Some(lat), Some(lon), Some(demand)) =
            (row.order_id, row.kind, row.lat, row.lon, row.demand)
        else {
            continue;
        };
        if let Some(route) = routes.last_mut() {
            route.stops.push(RouteStop {
                order_id: order_id.into(),
                kind: parsed(&kind)?,
                location: LatLon::new(lat, lon)?,
                demand: unsigned(demand, "orders.demand")?,
            });
        }
    }
    Ok(routes)
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;
    use crate::db::testing::{self, date};
    use crate::db::{couriers, orders};
    use crate::ids::{CourierId, OrderId};
    use crate::model::{OrderKind, Role};

    fn is_violation_of(error: &DbError, constraint: &str) -> bool {
        matches!(error, DbError::Sqlx(sqlx::Error::Database(database))
            if database.constraint() == Some(constraint))
    }

    #[sqlx::test]
    async fn a_route_keeps_its_stops_in_calling_order(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let plan = testing::plan(&pool, date(12)).await;
        let courier = testing::courier(&pool, 10).await;
        let mut stored = Vec::new();
        for _ in 0..5 {
            stored.push(testing::order(&pool, client.id, date(12)).await);
        }
        // Not the order they were created in, which is also the order of their ids.
        let calling_order: Vec<OrderId> = [3, 0, 4, 1, 2].map(|at: usize| stored[at].id).to_vec();

        let id = insert(
            &pool,
            &NewRoute {
                plan_id: plan.id,
                courier_id: courier.id,
                distance_m: 12_345,
                stops: calling_order.clone(),
            },
        )
        .await
        .unwrap();

        let routes = list_for_plan(&pool, plan.id).await.unwrap();
        assert_eq!(routes.len(), 1);
        let route = &routes[0];
        assert_eq!(
            (route.id, route.plan_id, route.courier_id, route.distance_m),
            (id, plan.id, courier.id, 12_345)
        );
        let listed: Vec<OrderId> = route.stops.iter().map(|stop| stop.order_id).collect();
        assert_eq!(listed, calling_order);

        // A stop carries what the map draws.
        let first = &route.stops[0];
        assert_eq!(
            (first.kind, first.location, first.demand),
            (stored[3].kind, stored[3].location, stored[3].demand)
        );
        assert_eq!(first.kind, OrderKind::Delivery);
    }

    #[sqlx::test]
    async fn the_routes_of_a_plan_are_listed_apart(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let plan = testing::plan(&pool, date(12)).await;
        let other_plan = testing::plan(&pool, date(13)).await;
        let mut expected = Vec::new();
        for stops in [2usize, 0, 3] {
            let courier = testing::courier(&pool, 10).await;
            let mut order_ids = Vec::new();
            for _ in 0..stops {
                order_ids.push(testing::order(&pool, client.id, date(12)).await.id);
            }
            let new = NewRoute {
                plan_id: plan.id,
                courier_id: courier.id,
                distance_m: 1_000 * stops as u64,
                stops: order_ids.clone(),
            };
            insert(&pool, &new).await.unwrap();
            expected.push((courier.id, order_ids));
        }
        // Another plan's route does not show up.
        let courier = testing::courier(&pool, 10).await;
        let stranger = testing::order(&pool, client.id, date(13)).await;
        let elsewhere = NewRoute {
            plan_id: other_plan.id,
            courier_id: courier.id,
            distance_m: 5,
            stops: vec![stranger.id],
        };
        insert(&pool, &elsewhere).await.unwrap();

        let listed: Vec<(CourierId, Vec<OrderId>)> = list_for_plan(&pool, plan.id)
            .await
            .unwrap()
            .into_iter()
            .map(|route| {
                let stops = route.stops.iter().map(|stop| stop.order_id).collect();
                (route.courier_id, stops)
            })
            .collect();
        assert_eq!(listed, expected);
        assert!(
            list_for_plan(&pool, PlanId::generate())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[sqlx::test]
    async fn an_order_cannot_be_put_on_two_routes(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let plan = testing::plan(&pool, date(12)).await;
        let order = testing::order(&pool, client.id, date(12)).await;
        let other = testing::order(&pool, client.id, date(12)).await;
        let route = |courier: CourierId, stops: Vec<OrderId>| NewRoute {
            plan_id: plan.id,
            courier_id: courier,
            distance_m: 100,
            stops,
        };
        let first = testing::courier(&pool, 10).await.id;
        let second = testing::courier(&pool, 10).await.id;
        insert(&pool, &route(first, vec![order.id])).await.unwrap();

        // On another courier's route.
        let error = insert(&pool, &route(second, vec![other.id, order.id]))
            .await
            .unwrap_err();
        assert!(
            is_violation_of(&error, "route_stops_order_key"),
            "{error:?}"
        );
        // Twice on one route.
        let error = insert(&pool, &route(second, vec![other.id, other.id]))
            .await
            .unwrap_err();
        assert!(
            is_violation_of(&error, "route_stops_order_key"),
            "{error:?}"
        );

        // A refused route leaves nothing behind, not even its own row.
        assert_eq!(list_for_plan(&pool, plan.id).await.unwrap().len(), 1);
        let free = orders::get(&pool, other.id).await.unwrap().unwrap();
        assert_eq!(free.id, other.id);
        insert(&pool, &route(second, vec![other.id])).await.unwrap();
    }

    #[sqlx::test]
    async fn a_courier_has_one_route_in_a_plan(pool: PgPool) {
        let plan = testing::plan(&pool, date(12)).await;
        let courier = testing::courier(&pool, 10).await;
        let route = NewRoute {
            plan_id: plan.id,
            courier_id: courier.id,
            distance_m: 0,
            stops: Vec::new(),
        };
        insert(&pool, &route).await.unwrap();

        let error = insert(&pool, &route).await.unwrap_err();
        assert!(
            is_violation_of(&error, "routes_plan_courier_key"),
            "{error:?}"
        );
        // The same courier may drive for another plan.
        let other_plan = testing::plan(&pool, date(13)).await;
        let elsewhere = NewRoute {
            plan_id: other_plan.id,
            ..route
        };
        insert(&pool, &elsewhere).await.unwrap();

        // A courier with routes stays, which is why couriers are switched off instead.
        let error = sqlx::query("DELETE FROM couriers WHERE id = $1")
            .bind(courier.id.0)
            .execute(&pool)
            .await
            .unwrap_err();
        assert!(
            error
                .as_database_error()
                .unwrap()
                .is_foreign_key_violation()
        );
        assert!(
            couriers::set_active(&pool, courier.id, false)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[sqlx::test]
    async fn a_distance_the_column_cannot_hold_is_refused(pool: PgPool) {
        let plan = testing::plan(&pool, date(12)).await;
        let courier = testing::courier(&pool, 10).await;
        let mut route = NewRoute {
            plan_id: plan.id,
            courier_id: courier.id,
            distance_m: u64::MAX,
            stops: Vec::new(),
        };
        let error = insert(&pool, &route).await.unwrap_err();
        assert!(matches!(error, DbError::OutOfRange(_)), "{error:?}");

        route.distance_m = i64::MAX as u64;
        insert(&pool, &route).await.unwrap();
        let listed = list_for_plan(&pool, plan.id).await.unwrap();
        assert_eq!(listed[0].distance_m, i64::MAX as u64);
        assert!(listed[0].stops.is_empty());
    }
}
