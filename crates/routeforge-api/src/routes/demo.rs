//! Orders for a demonstration: a day's worth of them, on the streets round the depot.
//! For admins.
//!
//! They are made here, in the API, so that showing the system needs no tool beside it
//! and no sixth crate. The same seed gives the same orders round the same depot, so a
//! demonstration can be repeated.

use std::f64::consts::TAU;

use actix_web::{HttpResponse, post, web};
use rand::seq::IndexedRandom;
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use routeforge_domain::db::{depots, orders};
use routeforge_domain::{
    DemoCreated, DemoRequest, EARTH_RADIUS_M, LatLon, NewOrder, OrderKind, UserId,
};
use utoipa_actix_web::service_config::ServiceConfig;

use crate::app::{AppState, NotAcceptable, NotAllowed, NotLoggedIn, NotNow};
use crate::auth::Admin;
use crate::error::ApiError;
use crate::streets;
use crate::validate::{self, DEMO_RADIUS_DEFAULT_KM};

pub(crate) fn configure(config: &mut ServiceConfig<'_>) {
    config.service(create_demo_orders);
}

/// Generate orders for a demonstration
///
/// Stores `count` pending orders for the day, within `radius_km` of the depot. Each is
/// at a point of a street of Kyiv, picked at random. Round a depot with no street of
/// Kyiv within the radius they are spread evenly over the disc instead. About one in
/// five is a pickup, the rest are deliveries, each with a demand of 1 to 3. They are the
/// caller's orders. All of them are stored, or none.
#[utoipa::path(
    tag = "demo",
    security(("bearer" = [])),
    responses(
        (status = 201, description = "How many orders were stored", body = DemoCreated),
        NotLoggedIn,
        NotAllowed,
        NotNow,
        NotAcceptable,
    )
)]
#[post("/api/demo/orders")]
async fn create_demo_orders(
    Admin(admin): Admin,
    state: web::Data<AppState>,
    body: web::Json<DemoRequest>,
) -> Result<HttpResponse, ApiError> {
    let request = body.into_inner();
    validate::demo(&request)?;
    let depot = depots::current(&state.pool).await?.ok_or_else(|| {
        ApiError::Conflict("no depot has been set to scatter orders round".to_owned())
    })?;
    let scattered = scatter(depot.location, &request, admin.id).ok_or_else(|| {
        ApiError::Conflict("the depot is too near a pole to scatter orders round it".to_owned())
    })?;

    // One statement, so the orders are stored together or not at all.
    orders::create_many(&state.pool, &scattered).await?;
    Ok(HttpResponse::Created().json(DemoCreated {
        created: request.count,
    }))
}

/// The orders a request asks for: `count` of them within `radius_km` of the depot, on
/// its streets where there are any (D87).
///
/// Every random choice comes from a generator seeded with the request's seed, and they
/// are made in a fixed order. `None` if a position would lie beyond a pole.
fn scatter(depot: LatLon, request: &DemoRequest, client_id: UserId) -> Option<Vec<NewOrder>> {
    let radius_m = request.radius_km.unwrap_or(DEMO_RADIUS_DEFAULT_KM) * 1_000.0;
    let streets = streets::near(depot, radius_m);
    let mut rng = ChaCha8Rng::seed_from_u64(u64::from(request.seed));

    let mut order = || {
        let location = match streets.choose(&mut rng) {
            Some(&point) => point,
            None => on_disc(depot, radius_m, &mut rng)?,
        };
        let kind = if rng.random_range(0..5) == 0 {
            OrderKind::Pickup
        } else {
            OrderKind::Delivery
        };
        Some(NewOrder {
            client_id,
            kind,
            service_date: request.service_date,
            location,
            address: None,
            demand: rng.random_range(1..=3),
        })
    };
    (0..request.count).map(|_| order()).collect()
}

/// A point anywhere on the disc of `radius_m` round `centre`. `None` if it would lie
/// beyond a pole.
fn on_disc(centre: LatLon, radius_m: f64, rng: &mut ChaCha8Rng) -> Option<LatLon> {
    // Even over the disc and not crowded at its middle: a ring's share of the area grows
    // with its radius, which the root makes up for.
    let distance = radius_m * rng.random::<f64>().sqrt();
    let bearing = rng.random_range(0.0..TAU);
    // How much shorter a degree of longitude is here than at the equator.
    let east_scale = centre.lat().to_radians().cos();
    let north = (distance * bearing.cos() / EARTH_RADIUS_M).to_degrees();
    let east = (distance * bearing.sin() / (EARTH_RADIUS_M * east_scale)).to_degrees();
    // Across the date line the longitude starts again from the other end.
    let lon = (centre.lon() + east + 180.0).rem_euclid(360.0) - 180.0;
    LatLon::new(centre.lat() + north, lon).ok()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use actix_web::http::StatusCode;
    use routeforge_domain::{Role, haversine_m};
    use serde_json::{Value, json};
    use sqlx::PgPool;

    use super::*;
    use crate::testing::{self, date};

    fn kyiv() -> LatLon {
        LatLon::new(50.45, 30.52).unwrap()
    }

    fn request(count: u32, seed: u32, radius_km: Option<f64>) -> DemoRequest {
        DemoRequest {
            service_date: date(12),
            count,
            seed,
            radius_km,
        }
    }

    #[test]
    fn the_same_seed_gives_the_same_orders() {
        let client = UserId::generate();
        let first = scatter(kyiv(), &request(300, 7, None), client).unwrap();
        let again = scatter(kyiv(), &request(300, 7, None), client).unwrap();
        assert_eq!(first, again);
        assert_eq!(first.len(), 300);

        let other = scatter(kyiv(), &request(300, 8, None), client).unwrap();
        assert_ne!(first, other);
        // A longer run starts as the shorter one did.
        let longer = scatter(kyiv(), &request(301, 7, None), client).unwrap();
        assert_eq!(longer[..300], first[..]);
    }

    /// How many of `orders` lie on each side of `depot`: north-east, north-west,
    /// south-east and south-west.
    fn quadrants(depot: LatLon, orders: &[NewOrder]) -> [usize; 4] {
        [(true, true), (true, false), (false, true), (false, false)].map(|(north, east)| {
            orders
                .iter()
                .filter(|order| {
                    (order.location.lat() > depot.lat()) == north
                        && (order.location.lon() > depot.lon()) == east
                })
                .count()
        })
    }

    /// What is the same of every demo order, wherever it lies: a day of 2000 of them.
    fn assert_kinds_and_demands(orders: &[NewOrder]) {
        assert!(orders.iter().all(|order| (1..=3).contains(&order.demand)));
        let pickups = orders
            .iter()
            .filter(|order| order.kind == OrderKind::Pickup)
            .count();
        assert!((300..500).contains(&pickups), "{pickups} of 2000");
        assert!(orders.iter().all(|order| order.service_date == date(12)));
    }

    #[test]
    fn in_kyiv_the_orders_lie_on_its_streets() {
        let depot = kyiv();
        let orders = scatter(depot, &request(2_000, 1, Some(10.0)), UserId::generate()).unwrap();

        let bits = |point: LatLon| (point.lat().to_bits(), point.lon().to_bits());
        let streets: HashSet<_> = streets::near(depot, 10_000.0)
            .into_iter()
            .map(bits)
            .collect();
        assert!(
            orders
                .iter()
                .all(|order| streets.contains(&bits(order.location)))
        );
        assert!(
            orders
                .iter()
                .all(|order| haversine_m(depot, order.location) <= 10_000.0)
        );
        // On every side of the depot, and so on both banks of the Dnipro. Not evenly:
        // there are more streets on some sides than on others.
        for quadrant in quadrants(depot, &orders) {
            assert!(quadrant >= 200, "{quadrant} of 2000");
        }
        assert_kinds_and_demands(&orders);
    }

    #[test]
    fn away_from_the_streets_the_orders_lie_all_over_a_disc() {
        let depot = LatLon::new(49.84, 24.03).unwrap();
        let orders = scatter(depot, &request(2_000, 1, Some(10.0)), UserId::generate()).unwrap();
        let distances: Vec<f64> = orders
            .iter()
            .map(|order| haversine_m(depot, order.location))
            .collect();

        // Within the radius, give or take what a flat map is off by at this size.
        assert!(distances.iter().all(|&metres| metres <= 10_000.0 * 1.01));
        // Even over the area: a quarter of them in the inner half of the radius.
        let inner = distances.iter().filter(|&&metres| metres < 5_000.0).count();
        assert!((400..600).contains(&inner), "{inner} of 2000");
        // And on every side of the depot.
        for quadrant in quadrants(depot, &orders) {
            assert!((400..600).contains(&quadrant), "{quadrant} of 2000");
        }
        assert_kinds_and_demands(&orders);
    }

    #[test]
    fn a_depot_at_the_date_line_or_near_a_pole_is_handled() {
        let fiji = LatLon::new(-17.0, 179.99).unwrap();
        let orders = scatter(fiji, &request(200, 3, Some(10.0)), UserId::generate()).unwrap();
        assert!(orders.iter().any(|order| order.location.lon() < 0.0));
        assert!(
            orders
                .iter()
                .all(|order| haversine_m(fiji, order.location) <= 10_100.0)
        );

        let pole = LatLon::new(89.99, 0.0).unwrap();
        assert_eq!(
            scatter(pole, &request(200, 3, Some(10.0)), UserId::generate()),
            None
        );
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn the_generated_orders_are_stored_as_the_admins(pool: PgPool) {
        let api = testing::api(&pool);
        let admin = api.actor(Role::Admin).await;
        let asked = json!({ "service_date": date(12), "count": 40, "seed": 2026 });

        let early = api
            .post(Some(&admin), "/api/demo/orders", asked.clone())
            .await;
        assert_eq!(early.status, StatusCode::CONFLICT, "there is no depot yet");
        let depot = api.depot().await;

        let made = api
            .post(Some(&admin), "/api/demo/orders", asked.clone())
            .await;
        assert_eq!(made.status, StatusCode::CREATED);
        assert_eq!(made.body, json!({ "created": 40 }));

        let listed = api
            .get(Some(&admin), "/api/orders?service_date=2026-10-12")
            .await;
        let stored = listed.body.as_array().unwrap();
        assert_eq!(stored.len(), 40);
        let expected = scatter(depot.location, &request(40, 2026, None), admin.user.id).unwrap();
        for (order, expected) in stored.iter().zip(&expected) {
            assert_eq!(order["client_id"], json!(admin.user.id));
            assert_eq!(order["status"], "pending");
            // A position that has been through JSON may come back changed in its last
            // digit, which is a nanometre.
            let (lat, lon) = (&order["location"]["lat"], &order["location"]["lon"]);
            assert!((lat.as_f64().unwrap() - expected.location.lat()).abs() < 1e-12);
            assert!((lon.as_f64().unwrap() - expected.location.lon()).abs() < 1e-12);
            assert_eq!(order["kind"], json!(expected.kind));
            assert_eq!(order["demand"], expected.demand);
        }
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_demonstration_too_large_or_too_wide_is_refused(pool: PgPool) {
        let api = testing::api(&pool);
        let admin = api.actor(Role::Admin).await;
        api.depot().await;

        for (change, field) in [
            (json!({ "count": 0 }), "count"),
            (json!({ "count": 2001 }), "count"),
            (json!({ "radius_km": 0 }), "radius_km"),
            (json!({ "radius_km": 31 }), "radius_km"),
        ] {
            let mut asked = json!({ "service_date": date(12), "count": 10, "seed": 1 });
            for (key, value) in change.as_object().unwrap() {
                asked[key] = value.clone();
            }
            let refused = api.post(Some(&admin), "/api/demo/orders", asked).await;
            assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{field}");
            assert_eq!(refused.body["errors"][0]["field"], field);
        }
        let listed = api.get(Some(&admin), "/api/orders").await;
        assert_eq!(listed.body, Value::Array(Vec::new()));
    }
}
