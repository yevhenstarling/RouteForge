//! Routes and their stops.
//!
//! A route is written once, when its route job completes, and is read for the map. It
//! is deleted only together with the other routes of its plan: when the plan fails or
//! is cancelled (D20), or is discarded after it was done (D30).
//!
//! Whoever placed an order may ask where it is on its route, and is told that and
//! nothing else of the route: see [`stops_of_day`] (D78).

use chrono::NaiveDate;
use sqlx::PgExecutor;
use uuid::Uuid;

use super::{DbError, parsed, unsigned};
use crate::geo::{LatLon, Polyline};
use crate::ids::{PlanId, RouteId, UserId};
use crate::model::{NewRoute, OrderStop, Route, RouteStop};

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
             INSERT INTO routes (id, plan_id, courier_id, distance_m, distances, path)
             VALUES ($1, $2, $3, $4, $5, $6)
             RETURNING id
         )
         INSERT INTO route_stops (route_id, position, order_id)
         SELECT route.id, (stop.number - 1)::integer, stop.order_id
         FROM route, unnest($7::uuid[]) WITH ORDINALITY AS stop (order_id, number)",
        id.0,
        new.plan_id.0,
        new.courier_id.0,
        distance_m,
        new.distances.as_str(),
        new.path.as_ref().map(Polyline::as_str),
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
    // One row per stop, and one row with nulls for a route without stops. The way a
    // route takes is tens of thousands of letters, so it comes with the first row of
    // its route alone, and not once for every stop.
    let rows = sqlx::query!(
        r#"SELECT routes.id, routes.courier_id, routes.distance_m, routes.distances,
                  CASE WHEN stops.position IS NULL OR stops.position = 0
                       THEN routes.path END AS path,
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
                distances: parsed(&row.distances)?,
                // The first row of the route, which is the one the path comes with.
                path: row.path.map(Polyline::new).transpose()?,
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

/// Where the orders of a day are on their routes, oldest order first (D78): those of
/// one client, or of everybody.
///
/// Only an order whose route is stored is on one. An order that is pending or
/// withdrawn is left out, and so is one that a plan has taken and whose route is still
/// searched for. A plan that ends early, or is discarded, deletes its routes (D20,
/// D30), so what is found here is always of the plan that holds the order now.
pub async fn stops_of_day(
    executor: impl PgExecutor<'_>,
    service_date: NaiveDate,
    client_id: Option<UserId>,
) -> Result<Vec<OrderStop>, DbError> {
    // How many calls the route has is counted over the whole route, and not over the
    // rows that are left when one client's orders are picked out.
    let rows = sqlx::query!(
        r#"SELECT stops.order_id, couriers.name AS courier, stops.position,
                  (SELECT count(*) FROM route_stops AS calls
                   WHERE calls.route_id = stops.route_id) AS "calls!"
           FROM orders
           JOIN route_stops AS stops ON stops.order_id = orders.id
           JOIN routes ON routes.id = stops.route_id
           JOIN couriers ON couriers.id = routes.courier_id
           WHERE orders.service_date = $1
             AND ($2::uuid IS NULL OR orders.client_id = $2)
           ORDER BY orders.id"#,
        service_date,
        client_id.map(|id| id.0),
    )
    .fetch_all(executor)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(OrderStop {
                order_id: row.order_id.into(),
                courier: row.courier,
                position: u32::try_from(row.position).map_err(|_| {
                    DbError::Unreadable(format!("route_stops.position is {}", row.position))
                })?,
                stops: unsigned(row.calls, "the calls of a route")?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;
    use crate::db::testing::{self, date};
    use crate::db::{couriers, orders, queue};
    use crate::geo::Distances;
    use crate::ids::{CourierId, OrderId};
    use crate::model::{OrderKind, Role};

    /// Where each order of a client is, as (order, courier, position, calls).
    async fn where_are(
        pool: &PgPool,
        day: u32,
        client_id: Option<UserId>,
    ) -> Vec<(OrderId, String, u32, u32)> {
        stops_of_day(pool, date(day), client_id)
            .await
            .unwrap()
            .into_iter()
            .map(|stop| (stop.order_id, stop.courier, stop.position, stop.stops))
            .collect()
    }

    /// A courier with a name of its own, to tell the routes apart by.
    async fn van(pool: &PgPool, name: &str) -> CourierId {
        let new = crate::model::NewCourier {
            name: name.to_owned(),
            capacity: 100,
        };
        couriers::create(pool, &new).await.unwrap().id
    }

    #[sqlx::test]
    async fn an_order_is_found_on_its_route_once_the_route_is_stored(pool: PgPool) {
        let ada = testing::user(&pool, Role::Client).await;
        let bob = testing::user(&pool, Role::Client).await;
        let plan = testing::plan(&pool, date(12)).await;
        let mut adas = Vec::new();
        let mut bobs = Vec::new();
        for _ in 0..3 {
            adas.push(testing::order(&pool, ada.id, date(12)).await.id);
            bobs.push(testing::order(&pool, bob.id, date(12)).await.id);
        }
        let pending = testing::order(&pool, ada.id, date(12)).await;
        let other_day = testing::order(&pool, ada.id, date(13)).await;
        let all: Vec<OrderId> = adas.iter().chain(&bobs).copied().collect();
        orders::mark_planned(&pool, plan.id, &all).await.unwrap();

        // A plan has taken them, and no route is stored yet: they are on none.
        assert!(where_are(&pool, 12, None).await.is_empty());

        // Two routes. Ada's orders are scattered over both, among Bob's.
        let (north, south) = (van(&pool, "North").await, van(&pool, "South").await);
        let route = |courier_id, stops: Vec<OrderId>| NewRoute {
            plan_id: plan.id,
            courier_id,
            distance_m: 100,
            distances: Distances::GreatCircle,
            path: None,
            stops,
        };
        insert(
            &pool,
            &route(north, vec![bobs[0], adas[2], bobs[1], adas[0]]),
        )
        .await
        .unwrap();
        // One of them is stored, and the other is still searched for.
        assert_eq!(
            where_are(&pool, 12, Some(ada.id)).await,
            [
                (adas[0], "North".to_owned(), 3, 4),
                (adas[2], "North".to_owned(), 1, 4),
            ]
        );
        insert(&pool, &route(south, vec![adas[1], bobs[2]]))
            .await
            .unwrap();

        // Oldest order first. Where an order is, is counted along its whole route: of
        // four calls and of two, whoever's the other calls are.
        assert_eq!(
            where_are(&pool, 12, Some(ada.id)).await,
            [
                (adas[0], "North".to_owned(), 3, 4),
                (adas[1], "South".to_owned(), 0, 2),
                (adas[2], "North".to_owned(), 1, 4),
            ]
        );
        assert_eq!(
            where_are(&pool, 12, Some(bob.id)).await,
            [
                (bobs[0], "North".to_owned(), 0, 4),
                (bobs[1], "North".to_owned(), 2, 4),
                (bobs[2], "South".to_owned(), 1, 2),
            ]
        );
        // Everybody's, for whoever may see them all, in the order they were placed.
        let everybodys: Vec<OrderId> = where_are(&pool, 12, None)
            .await
            .into_iter()
            .map(|(order_id, ..)| order_id)
            .collect();
        let mut placed = all.clone();
        placed.sort();
        assert_eq!(everybodys, placed);

        // An order that no plan has taken is on no route, and neither is anything of
        // another day or of somebody who has no orders.
        assert!(!all.contains(&pending.id));
        assert!(where_are(&pool, 13, Some(ada.id)).await.is_empty());
        assert!(where_are(&pool, 14, None).await.is_empty());
        let nobody = testing::user(&pool, Role::Client).await;
        assert!(where_are(&pool, 12, Some(nobody.id)).await.is_empty());
        assert_eq!(
            orders::get(&pool, other_day.id).await.unwrap().unwrap().id,
            other_day.id
        );
    }

    /// A plan that is taken back takes its routes with it (D20, D30), and its orders
    /// are on none again.
    #[sqlx::test]
    async fn an_order_is_on_no_route_once_its_plan_is_taken_back(pool: PgPool) {
        let routing = testing::routing(&pool, date(12), 2, 2).await;
        let worker = crate::ids::WorkerId::generate();
        for _ in 0..2 {
            let job = queue::claim(&pool, worker, testing::LONG)
                .await
                .unwrap()
                .unwrap();
            testing::complete_route(&pool, &job).await.unwrap();
        }
        let stored: Vec<OrderId> = where_are(&pool, 12, None)
            .await
            .into_iter()
            .map(|(order_id, ..)| order_id)
            .collect();
        let mut planned = routing.order_ids();
        planned.sort();
        assert_eq!(stored, planned);

        assert!(queue::discard_plan(&pool, routing.plan.id).await.unwrap());
        assert!(where_are(&pool, 12, None).await.is_empty());
    }

    /// The schema repeats the values of the enumeration in a check (D19). Each of them
    /// is stored and read back, so the two cannot drift apart unnoticed.
    #[sqlx::test]
    async fn a_route_says_what_its_length_was_measured_along(pool: PgPool) {
        let plan = testing::plan(&pool, date(12)).await;
        let mut stored = Vec::new();
        for &distances in Distances::ALL {
            let courier = testing::courier(&pool, 10).await;
            let route = NewRoute {
                plan_id: plan.id,
                courier_id: courier.id,
                distance_m: 100,
                distances,
                path: None,
                stops: Vec::new(),
            };
            insert(&pool, &route).await.unwrap();
            stored.push((courier.id, distances));
        }
        let listed: Vec<(CourierId, Distances)> = list_for_plan(&pool, plan.id)
            .await
            .unwrap()
            .into_iter()
            .map(|route| (route.courier_id, route.distances))
            .collect();
        assert_eq!(listed, stored);

        // A route from before the column was there was measured along great circles.
        let courier = testing::courier(&pool, 10).await;
        sqlx::query(
            "INSERT INTO routes (id, plan_id, courier_id, distance_m) VALUES ($1, $2, $3, 7)",
        )
        .bind(RouteId::generate().0)
        .bind(plan.id.0)
        .bind(courier.id.0)
        .execute(&pool)
        .await
        .unwrap();
        let listed = list_for_plan(&pool, plan.id).await.unwrap();
        assert_eq!(listed.last().unwrap().distances, Distances::GreatCircle);
        assert_eq!(listed.last().unwrap().path, None);

        // And nothing else is stored there.
        let error = sqlx::query("UPDATE routes SET distances = 'by_rail' WHERE plan_id = $1")
            .bind(plan.id.0)
            .execute(&pool)
            .await
            .unwrap_err();
        assert!(
            is_violation_of(&DbError::from(error), "routes_distances_check"),
            "another check refused it"
        );
    }

    fn is_violation_of(error: &DbError, constraint: &str) -> bool {
        matches!(error, DbError::Sqlx(sqlx::Error::Database(database))
            if database.constraint() == Some(constraint))
    }

    /// A line from the depot of the tests `turn` degrees to the north-east and back.
    fn there_and_back(turn: f64) -> Polyline {
        let at = |lat, lon| LatLon::new(lat, lon).unwrap();
        let depot = at(50.45, 30.52);
        Polyline::through(&[depot, at(50.45 + turn, 30.52 + turn), depot]).unwrap()
    }

    /// Each route is read with its own way and no other's, whether it has many stops,
    /// none, or no way at all (D73).
    #[sqlx::test]
    async fn a_route_by_road_keeps_the_way_it_takes(pool: PgPool) {
        let client = testing::user(&pool, Role::Client).await;
        let plan = testing::plan(&pool, date(12)).await;
        let mut expected = Vec::new();
        for (stops, distances, path) in [
            (3, Distances::Road, Some(there_and_back(0.01))),
            (0, Distances::Road, Some(there_and_back(0.02))),
            // By road, and the service did not say which way.
            (2, Distances::Road, None),
            (2, Distances::GreatCircle, None),
            (1, Distances::Road, Some(there_and_back(0.03))),
        ] {
            let courier = testing::courier(&pool, 10).await;
            let mut order_ids = Vec::new();
            for _ in 0..stops {
                order_ids.push(testing::order(&pool, client.id, date(12)).await.id);
            }
            let new = NewRoute {
                plan_id: plan.id,
                courier_id: courier.id,
                distance_m: 100,
                distances,
                path: path.clone(),
                stops: order_ids,
            };
            insert(&pool, &new).await.unwrap();
            expected.push((courier.id, stops, path));
        }

        let listed: Vec<(CourierId, usize, Option<Polyline>)> = list_for_plan(&pool, plan.id)
            .await
            .unwrap()
            .into_iter()
            .map(|route| (route.courier_id, route.stops.len(), route.path))
            .collect();
        assert_eq!(listed, expected);
    }

    /// On great circles the straight line from stop to stop is the way, and the schema
    /// keeps a line along streets from being stored for such a route.
    #[sqlx::test]
    async fn only_a_route_by_road_has_a_way(pool: PgPool) {
        let plan = testing::plan(&pool, date(12)).await;
        let courier = testing::courier(&pool, 10).await;
        let route = NewRoute {
            plan_id: plan.id,
            courier_id: courier.id,
            distance_m: 100,
            distances: Distances::GreatCircle,
            path: Some(there_and_back(0.01)),
            stops: Vec::new(),
        };
        let error = insert(&pool, &route).await.unwrap_err();
        assert!(is_violation_of(&error, "routes_path_check"), "{error:?}");
        assert!(list_for_plan(&pool, plan.id).await.unwrap().is_empty());

        // What is stored there is a line, or the route cannot be read.
        let by_road = NewRoute {
            distances: Distances::Road,
            ..route
        };
        insert(&pool, &by_road).await.unwrap();
        sqlx::query("UPDATE routes SET path = 'not a line' WHERE plan_id = $1")
            .bind(plan.id.0)
            .execute(&pool)
            .await
            .unwrap();
        let error = list_for_plan(&pool, plan.id).await.unwrap_err();
        assert!(
            matches!(&error, DbError::Unreadable(what) if what.contains("line")),
            "{error:?}"
        );
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
                distances: Distances::GreatCircle,
                path: None,
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
                distances: Distances::GreatCircle,
                path: None,
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
            distances: Distances::GreatCircle,
            path: None,
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
            distances: Distances::GreatCircle,
            path: None,
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
            distances: Distances::GreatCircle,
            path: None,
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
            distances: Distances::GreatCircle,
            path: None,
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
