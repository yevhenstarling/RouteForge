//! Orders: parcels to deliver or to pick up.
//!
//! An order belongs to whoever placed it. A client sees and withdraws their own, and
//! to a client an order of somebody else's is not there at all: the answer is the
//! `404` of an id that names nothing. Operators and admins see every order.
//!
//! Placing an order can be made safe to repeat, with a key of the caller's making
//! (D76).
//!
//! Of the route a plan puts an order on, whoever placed the order is told where the
//! order is on it, and nothing else (D78).

use actix_web::{HttpRequest, HttpResponse, get, post, web};
use routeforge_domain::db::orders::{self, Placed};
use routeforge_domain::db::routes;
use routeforge_domain::{
    IdempotencyKey, Order, OrderDay, OrderFilter, OrderId, OrderRequest, OrderStatus, OrderStop,
};
use utoipa_actix_web::service_config::ServiceConfig;
use uuid::Uuid;

use crate::app::{AppState, NotAcceptable, NotLoggedIn, NotNow, NotThere};
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::validate;

pub(crate) fn configure(config: &mut ServiceConfig<'_>) {
    config
        .service(create_order)
        .service(list_orders)
        // Before the route that takes an id: a path is given to the first route it
        // fits, and `stops` fits `{id}` as any word does.
        .service(list_order_stops)
        .service(get_order)
        .service(cancel_order);
}

/// Place an order
///
/// The order is the caller's, and is pending until a plan gives it to a courier.
///
/// A request may bring a key of the caller's making, as `Idempotency-Key`. Sent again
/// with the same key, it places nothing: the answer is `200`, with the order that key
/// placed, as it stands by then. So a request whose answer was lost can be sent again
/// as it was, and the order is placed once. A key is the caller's own, and is for one
/// order: with a key that has placed another order the request is refused.
#[utoipa::path(
    tag = "orders",
    security(("bearer" = [])),
    params(
        // A header is there or it is not: it is never null.
        ("Idempotency-Key" = Option<String>, Header, nullable = false,
            min_length = 1, max_length = 64, pattern = "^[A-Za-z0-9._-]+$",
            description = "What makes the request safe to repeat: 1 to 64 letters, digits, \
                dots, underscores or hyphens, made up by the caller. A UUID will do"),
    ),
    responses(
        (status = 201, description = "The order", body = Order),
        (status = 200, description = "The order this key placed before. Nothing was stored this time",
            body = Order),
        NotLoggedIn,
        NotAcceptable,
    )
)]
#[post("/api/orders")]
async fn create_order(
    user: AuthUser,
    state: web::Data<AppState>,
    request: HttpRequest,
    body: web::Json<OrderRequest>,
) -> Result<HttpResponse, ApiError> {
    let asked = body.into_inner();
    validate::order(&asked)?;
    let key = key_of(&request)?;
    let new = asked.placed_by(user.id);
    match orders::place(&state.pool, &new, key.as_ref()).await? {
        Placed::Now(order) => Ok(HttpResponse::Created().json(order)),
        Placed::Before(order) if new.asks_for(&order) => Ok(HttpResponse::Ok().json(order)),
        // The key is taken, and not by this order. To answer with the other one would
        // be to say that this one was placed.
        Placed::Before(_) => Err(ApiError::invalid_field(
            IdempotencyKey::HEADER,
            "has placed another order already: a key is for one order",
        )),
    }
}

/// The key a request came with, if it came with one (D76).
///
/// One that is no key is refused, and not passed over: whoever sent it takes the
/// request for one that is safe to repeat.
fn key_of(request: &HttpRequest) -> Result<Option<IdempotencyKey>, ApiError> {
    let Some(sent) = request.headers().get(IdempotencyKey::HEADER) else {
        return Ok(None);
    };
    let key = sent
        .to_str()
        .ok()
        .and_then(|text| IdempotencyKey::new(text).ok());
    match key {
        Some(key) => Ok(Some(key)),
        None => Err(ApiError::invalid_field(
            IdempotencyKey::HEADER,
            format!(
                "must be 1 to {} letters, digits, dots, underscores or hyphens",
                IdempotencyKey::LONGEST
            ),
        )),
    }
}

/// List orders
///
/// Oldest first, a page at a time: `limit` orders, 100 unless said otherwise and 1000
/// at most, `after` the last one of the page before. A client gets their own orders,
/// whatever `client_id` says.
#[utoipa::path(
    tag = "orders",
    security(("bearer" = [])),
    // Named here as well as in the arguments: utoipa describes a query it finds
    // there only if it is listed.
    params(OrderFilter),
    responses(
        (status = 200, description = "One page of the orders that pass the filter", body = [Order]),
        NotLoggedIn,
        NotAcceptable,
    )
)]
#[get("/api/orders")]
async fn list_orders(
    user: AuthUser,
    state: web::Data<AppState>,
    query: web::Query<OrderFilter>,
) -> Result<web::Json<Vec<Order>>, ApiError> {
    let mut filter = query.into_inner();
    filter.limit = Some(validate::page(filter.limit)?);
    if !user.is_staff() {
        filter.client_id = Some(user.id);
    }
    Ok(web::Json(orders::list(&state.pool, &filter).await?))
}

/// Say where the orders of a day are on their routes
///
/// For each order of the day that is on a stored route: the name of its courier, which
/// call of the route it is, counted from 0, and how many calls the route has. Oldest
/// order first. An order that is pending or withdrawn is not among them, and neither is
/// one that a plan has taken and whose route is still searched for.
///
/// A client gets those of their own orders. That is all a client is told of a route:
/// not its other calls, its line or its length.
#[utoipa::path(
    tag = "orders",
    security(("bearer" = [])),
    // Named here as well as in the arguments: utoipa describes a query it finds
    // there only if it is listed.
    params(OrderDay),
    responses(
        (status = 200, description = "Where each order of the day is that is on a route",
            body = [OrderStop]),
        NotLoggedIn,
    )
)]
#[get("/api/orders/stops")]
async fn list_order_stops(
    user: AuthUser,
    state: web::Data<AppState>,
    query: web::Query<OrderDay>,
) -> Result<web::Json<Vec<OrderStop>>, ApiError> {
    let whose = (!user.is_staff()).then_some(user.id);
    let stops = routes::stops_of_day(&state.pool, query.service_date, whose).await?;
    Ok(web::Json(stops))
}

/// Read one order
#[utoipa::path(
    tag = "orders",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The order", body = Order),
        NotLoggedIn,
        NotThere,
    )
)]
#[get("/api/orders/{id}")]
async fn get_order(
    user: AuthUser,
    state: web::Data<AppState>,
    path: web::Path<Uuid>,
) -> Result<web::Json<Order>, ApiError> {
    let order = visible_to(&user, &state, path.into_inner().into()).await?;
    Ok(web::Json(order))
}

/// Withdraw an order
///
/// Only an order that is still pending can be withdrawn. One that a plan has given to
/// a courier stays planned until that plan is cancelled or discarded.
#[utoipa::path(
    tag = "orders",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The order, now cancelled", body = Order),
        NotLoggedIn,
        NotThere,
        NotNow,
    )
)]
#[post("/api/orders/{id}/cancel")]
async fn cancel_order(
    user: AuthUser,
    state: web::Data<AppState>,
    path: web::Path<Uuid>,
) -> Result<web::Json<Order>, ApiError> {
    let id = OrderId::from(path.into_inner());
    visible_to(&user, &state, id).await?;

    let cancelled = orders::cancel(&state.pool, id).await?;
    // Read again either way: a plan may have taken the order since it was looked at.
    let order = visible_to(&user, &state, id).await?;
    if cancelled {
        return Ok(web::Json(order));
    }
    Err(ApiError::Conflict(match order.status {
        OrderStatus::Planned => {
            "a plan has given the order to a courier: it can no longer be withdrawn".to_owned()
        }
        status => format!("the order is {status} and cannot be withdrawn"),
    }))
}

/// The order with this id, if there is one and the caller may see it.
async fn visible_to(user: &AuthUser, state: &AppState, id: OrderId) -> Result<Order, ApiError> {
    orders::get(&state.pool, id)
        .await?
        .filter(|order| user.is_staff() || order.client_id == user.id)
        .ok_or(ApiError::NotFound("there is no such order"))
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use routeforge_domain::Role;
    use serde_json::{Value, json};
    use sqlx::PgPool;

    use super::*;
    use crate::error::PROBLEM_JSON;
    use crate::testing::{self, Actor, Api, Reply, date};

    fn body(day: u32) -> Value {
        json!({
            "kind": "delivery",
            "service_date": date(day),
            "location": { "lat": 50.45, "lon": 30.52 },
            "address": "Khreshchatyk St, 22",
            "demand": 2,
        })
    }

    /// Asks for an order as `who`, with a key or without one.
    async fn place(api: &Api, who: &Actor, key: Option<&str>, order: Value) -> Reply {
        let request = testing::request("POST", "/api/orders", Some(who));
        let request = match key {
            Some(key) => request.insert_header((IdempotencyKey::HEADER, key)),
            None => request,
        };
        api.send(request.set_json(order)).await
    }

    fn ids(listed: &Value) -> Vec<&str> {
        let orders = listed.as_array().expect("a list of orders");
        orders
            .iter()
            .map(|order| order["id"].as_str().unwrap())
            .collect()
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn an_order_is_stored_for_whoever_places_it(pool: PgPool) {
        let api = testing::api(&pool);
        let client = api.actor(Role::Client).await;

        // A client cannot place an order in another's name: there is nowhere to say whose.
        let mut asked = body(12);
        asked["client_id"] = json!(api.actor(Role::Client).await.user.id);
        let placed = api.post(Some(&client), "/api/orders", asked).await;

        assert_eq!(placed.status, StatusCode::CREATED);
        assert_eq!(placed.body["client_id"], json!(client.user.id));
        assert_eq!(placed.body["status"], "pending");
        assert_eq!(placed.body["kind"], "delivery");
        assert_eq!(placed.body["service_date"], "2026-10-12");
        assert_eq!(
            placed.body["location"],
            json!({ "lat": 50.45, "lon": 30.52 })
        );
        assert_eq!(placed.body["address"], "Khreshchatyk St, 22");
        assert_eq!(placed.body["demand"], 2);
        assert_eq!(placed.body["plan_id"], Value::Null);

        let id = placed.body["id"].as_str().unwrap();
        let read = api.get(Some(&client), &format!("/api/orders/{id}")).await;
        assert_eq!(read.status, StatusCode::OK);
        assert_eq!(read.body, placed.body);
    }

    /// What the key is for: a request whose answer was lost is sent again, and the
    /// order is there once (D76).
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn an_order_that_is_sent_again_with_its_key_is_placed_once(pool: PgPool) {
        let api = testing::api(&pool);
        let client = api.actor(Role::Client).await;

        let placed = place(&api, &client, Some("press-1"), body(12)).await;
        assert_eq!(placed.status, StatusCode::CREATED);
        assert_eq!(placed.body["status"], "pending");
        let id = placed.body["id"].as_str().unwrap();

        // Sent again as it was: the same order, and a status that says nothing was
        // placed this time.
        for _ in 0..2 {
            let again = place(&api, &client, Some("press-1"), body(12)).await;
            assert_eq!(again.status, StatusCode::OK);
            assert_eq!(again.body, placed.body);
        }
        assert_eq!(ids(&api.get(Some(&client), "/api/orders").await.body), [id]);

        // It is answered as it stands by then, withdrawn if it was withdrawn.
        let cancel = format!("/api/orders/{id}/cancel");
        let withdrawn = api.post(Some(&client), &cancel, Value::Null).await;
        assert_eq!(withdrawn.status, StatusCode::OK);
        let again = place(&api, &client, Some("press-1"), body(12)).await;
        assert_eq!(again.status, StatusCode::OK);
        assert_eq!(again.body, withdrawn.body);
        assert_eq!(again.body["status"], "cancelled");

        // Another key is another order. So is no key, every time, as it always was.
        let other = place(&api, &client, Some("press-2"), body(12)).await;
        assert_eq!(other.status, StatusCode::CREATED);
        assert_ne!(other.body["id"], placed.body["id"]);
        for _ in 0..2 {
            let keyless = place(&api, &client, None, body(12)).await;
            assert_eq!(keyless.status, StatusCode::CREATED);
        }
        let listed = api.get(Some(&client), "/api/orders").await;
        assert_eq!(ids(&listed.body).len(), 4);

        // A key is the caller's own: somebody else's that reads the same places theirs.
        let bob = api.actor(Role::Client).await;
        let bobs = place(&api, &bob, Some("press-1"), body(12)).await;
        assert_eq!(bobs.status, StatusCode::CREATED);
        assert_eq!(bobs.body["client_id"], json!(bob.user.id));
        assert_eq!(ids(&api.get(Some(&bob), "/api/orders").await.body).len(), 1);
    }

    /// A key that has placed one order does not answer for another. With the first
    /// order for an answer, whoever asked would take the second for placed.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_key_is_for_one_order(pool: PgPool) {
        let api = testing::api(&pool);
        let client = api.actor(Role::Client).await;
        let placed = place(&api, &client, Some("press-1"), body(12)).await;
        assert_eq!(placed.status, StatusCode::CREATED);

        let changes: [fn(&mut Value); 5] = [
            |order| order["kind"] = json!("pickup"),
            |order| order["service_date"] = json!(date(13)),
            |order| order["location"]["lon"] = json!(30.53),
            |order| order["address"] = Value::Null,
            |order| order["demand"] = json!(3),
        ];
        for change in changes {
            let mut other = body(12);
            change(&mut other);
            let refused = place(&api, &client, Some("press-1"), other.clone()).await;
            assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{other}");
            assert_eq!(refused.content_type.as_deref(), Some(PROBLEM_JSON));
            assert_eq!(refused.body["errors"][0]["field"], "Idempotency-Key");
            let why = refused.body["errors"][0]["message"].as_str().unwrap();
            assert!(why.contains("another order"), "{why}");
        }
        // Nothing of all that was stored, and the first order is as it was.
        let listed = api.get(Some(&client), "/api/orders").await;
        assert_eq!(listed.body, json!([placed.body]));
        let again = place(&api, &client, Some("press-1"), body(12)).await;
        assert_eq!((again.status, &again.body), (StatusCode::OK, &placed.body));
    }

    /// Whoever sends a key takes the request for one that is safe to repeat. So a key
    /// that is none is refused, and not passed over.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_key_that_is_none_is_refused_and_places_nothing(pool: PgPool) {
        let api = testing::api(&pool);
        let client = api.actor(Role::Client).await;
        let longest = "k".repeat(IdempotencyKey::LONGEST);
        let too_long = "k".repeat(IdempotencyKey::LONGEST + 1);

        for bad in ["", "two words", "a;b", "ключ", too_long.as_str()] {
            let refused = place(&api, &client, Some(bad), body(12)).await;
            assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{bad:?}");
            assert_eq!(refused.content_type.as_deref(), Some(PROBLEM_JSON));
            assert_eq!(
                refused.body["errors"],
                json!([{
                    "field": "Idempotency-Key",
                    "message": "must be 1 to 64 letters, digits, dots, underscores or hyphens",
                }]),
                "{bad:?}"
            );
        }
        assert_eq!(
            ids(&api.get(Some(&client), "/api/orders").await.body).len(),
            0
        );

        for good in [
            "k",
            longest.as_str(),
            "0199e3a4-7c1e-7b52-9d0a-1f2e3d4c5b6a",
        ] {
            let placed = place(&api, &client, Some(good), body(12)).await;
            assert_eq!(placed.status, StatusCode::CREATED, "{good:?}");
        }

        // The gate comes first: nobody is told what is wrong with a key before they
        // have shown a token.
        let request = testing::request("POST", "/api/orders", None)
            .insert_header((IdempotencyKey::HEADER, "two words"))
            .set_json(body(12));
        assert_eq!(api.send(request).await.status, StatusCode::UNAUTHORIZED);
    }

    /// An order that is asked for again changes nothing, so whoever watches its day
    /// is told nothing (D63).
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn an_order_that_is_sent_again_is_announced_once(pool: PgPool) {
        let api = testing::api(&pool);
        let _listening = api.listen().await;
        let operator = api.actor(Role::Operator).await;
        let client = api.actor(Role::Client).await;
        let mut stream = api.open(&operator, "/api/system/stats/events").await;
        stream.next_of("stats").await;

        let placed = place(&api, &client, Some("press-1"), body(13)).await;
        assert_eq!(placed.status, StatusCode::CREATED);
        assert_eq!(
            stream.next_of("orders").await,
            json!({ "service_date": "2026-10-13" })
        );
        let again = place(&api, &client, Some("press-1"), body(13)).await;
        assert_eq!(again.status, StatusCode::OK);
        let refused = place(&api, &client, Some("press-1"), body(14)).await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            stream
                .is_quiet_for(std::time::Duration::from_millis(300))
                .await
        );
    }

    /// Where an order is on its route is the one thing of a route that a client is
    /// told, and a client is told it of their own orders alone (D78).
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_client_is_told_where_their_own_orders_are_on_their_routes(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;
        let ada = api.actor(Role::Client).await;
        let bob = api.actor(Role::Client).await;
        api.depot().await;
        api.courier(100).await;
        let adas_first = api.order(&ada, 12).await;
        let bobs = api.order(&bob, 12).await;
        let adas_second = api.order(&ada, 12).await;
        // Of another day, and no plan takes it.
        api.order(&ada, 13).await;

        let stops = "/api/orders/stops?service_date=2026-10-12";
        let none = api.get(Some(&ada), stops).await;
        assert_eq!((none.status, &none.body), (StatusCode::OK, &json!([])));

        let asked = api
            .post(
                Some(&operator),
                "/api/plans",
                json!({ "service_date": date(12) }),
            )
            .await;
        assert_eq!(asked.status, StatusCode::CREATED);
        // The plan takes the orders. They are planned from then on, and on no route
        // until the search for it is over.
        assert!(testing::work_one(&pool).await);
        let read = api
            .get(Some(&ada), &format!("/api/orders/{adas_first}"))
            .await;
        assert_eq!(read.body["status"], "planned");
        assert_eq!(api.get(Some(&ada), stops).await.body, json!([]));
        testing::work_off(&pool).await;

        // What stands in for the solver calls in the reverse of the order the orders
        // were placed in. Each is counted along the whole route, the other's call
        // included, and each of the two is told of their own alone.
        assert_eq!(
            api.get(Some(&ada), stops).await.body,
            json!([
                { "order_id": adas_first, "courier": "Van", "position": 2, "stops": 3 },
                { "order_id": adas_second, "courier": "Van", "position": 0, "stops": 3 },
            ])
        );
        assert_eq!(
            api.get(Some(&bob), stops).await.body,
            json!([{ "order_id": bobs, "courier": "Van", "position": 1, "stops": 3 }])
        );
        for role in [Role::Operator, Role::Admin] {
            let staff = api.actor(role).await;
            let all = api.get(Some(&staff), stops).await;
            let of: Vec<&str> = all
                .body
                .as_array()
                .expect("a list of stops")
                .iter()
                .map(|stop| stop["order_id"].as_str().unwrap())
                .collect();
            assert_eq!(
                of,
                [adas_first.as_str(), bobs.as_str(), adas_second.as_str()],
                "{role}"
            );
        }
        let other_day = api
            .get(Some(&ada), "/api/orders/stops?service_date=2026-10-13")
            .await;
        assert_eq!(other_day.body, json!([]));

        // The day has to be said. And `stops` is no id of an order.
        for (unreadable, why) in [
            ("/api/orders/stops", "missing field `service_date`"),
            ("/api/orders/stops?service_date=monday", "invalid"),
        ] {
            let reply = api.get(Some(&ada), unreadable).await;
            assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{unreadable}");
            assert_eq!(reply.content_type.as_deref(), Some(PROBLEM_JSON));
            let detail = reply.body["detail"].as_str().unwrap();
            assert!(detail.contains(why), "{detail}");
        }

        // A plan that is taken back takes its routes with it.
        let discard = format!("/api/plans/{}/discard", asked.body["id"].as_str().unwrap());
        let discarded = api.post(Some(&operator), &discard, Value::Null).await;
        assert_eq!(discarded.status, StatusCode::OK, "{}", discarded.text);
        assert_eq!(api.get(Some(&ada), stops).await.body, json!([]));
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_client_sees_their_own_orders_and_staff_see_all(pool: PgPool) {
        let api = testing::api(&pool);
        let ada = api.actor(Role::Client).await;
        let bob = api.actor(Role::Client).await;
        let adas = api.order(&ada, 12).await;
        let bobs = api.order(&bob, 12).await;

        let listed = api.get(Some(&ada), "/api/orders").await;
        assert_eq!(ids(&listed.body), [adas.as_str()]);
        // Asking for somebody else's by name changes nothing.
        let asked = format!("/api/orders?client_id={}", bob.user.id);
        let listed = api.get(Some(&ada), &asked).await;
        assert_eq!(ids(&listed.body), [adas.as_str()]);

        // Another client's order is not there, to read or to withdraw.
        for reply in [
            api.get(Some(&ada), &format!("/api/orders/{bobs}")).await,
            api.post(
                Some(&ada),
                &format!("/api/orders/{bobs}/cancel"),
                Value::Null,
            )
            .await,
        ] {
            assert_eq!(reply.status, StatusCode::NOT_FOUND);
            assert_eq!(reply.content_type.as_deref(), Some(PROBLEM_JSON));
        }
        // It is the answer an id that names nothing gets, to the letter.
        let nothing = api
            .get(Some(&ada), &format!("/api/orders/{}", OrderId::generate()))
            .await;
        let hidden = api.get(Some(&ada), &format!("/api/orders/{bobs}")).await;
        assert_eq!(hidden.body, nothing.body);
        let still = api.get(Some(&bob), &format!("/api/orders/{bobs}")).await;
        assert_eq!(still.body["status"], "pending");

        for role in [Role::Operator, Role::Admin] {
            let staff = api.actor(role).await;
            let listed = api.get(Some(&staff), "/api/orders").await;
            assert_eq!(ids(&listed.body), [adas.as_str(), bobs.as_str()], "{role}");
            let asked = format!("/api/orders?client_id={}", bob.user.id);
            let listed = api.get(Some(&staff), &asked).await;
            assert_eq!(ids(&listed.body), [bobs.as_str()], "{role}");
            let read = api.get(Some(&staff), &format!("/api/orders/{bobs}")).await;
            assert_eq!(read.status, StatusCode::OK, "{role}");
        }
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_list_is_filtered_and_read_in_pages(pool: PgPool) {
        let api = testing::api(&pool);
        let client = api.actor(Role::Client).await;
        let mut monday = Vec::new();
        for _ in 0..5 {
            monday.push(api.order(&client, 12).await);
        }
        let tuesday = api.order(&client, 13).await;
        let withdrawn = &monday[4];
        let cancel = format!("/api/orders/{withdrawn}/cancel");
        assert_eq!(
            api.post(Some(&client), &cancel, Value::Null).await.status,
            StatusCode::OK
        );

        let listed = api
            .get(Some(&client), "/api/orders?service_date=2026-10-13")
            .await;
        assert_eq!(ids(&listed.body), [tuesday.as_str()]);
        let listed = api
            .get(
                Some(&client),
                "/api/orders?service_date=2026-10-12&status=cancelled",
            )
            .await;
        assert_eq!(ids(&listed.body), [withdrawn.as_str()]);

        // Two to a page, each page asked for by the last id of the one before.
        let mut read = Vec::new();
        let mut after = String::new();
        loop {
            let page = api
                .get(Some(&client), &format!("/api/orders?limit=2{after}"))
                .await;
            assert_eq!(page.status, StatusCode::OK);
            let page: Vec<String> = ids(&page.body).into_iter().map(str::to_owned).collect();
            assert!(page.len() <= 2);
            let Some(last) = page.last() else { break };
            after = format!("&after={last}");
            read.extend(page);
        }
        let mut all = monday.clone();
        all.push(tuesday);
        assert_eq!(read, all);
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_page_is_a_hundred_orders_unless_asked_otherwise(pool: PgPool) {
        let api = testing::api(&pool);
        let client = api.actor(Role::Client).await;
        for _ in 0..101 {
            orders::create(&pool, &testing::new_order(client.user.id, 12))
                .await
                .unwrap();
        }
        let listed = api.get(Some(&client), "/api/orders").await;
        assert_eq!(ids(&listed.body).len(), 100);
        let listed = api.get(Some(&client), "/api/orders?limit=1000").await;
        assert_eq!(ids(&listed.body).len(), 101);

        for refused in ["limit=0", "limit=1001"] {
            let reply = api
                .get(Some(&client), &format!("/api/orders?{refused}"))
                .await;
            assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
            assert_eq!(reply.body["errors"][0]["field"], "limit");
        }
        for unreadable in [
            "limit=many",
            "status=lost",
            "after=7",
            "service_date=monday",
        ] {
            let reply = api
                .get(Some(&client), &format!("/api/orders?{unreadable}"))
                .await;
            assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{unreadable}");
            assert_eq!(reply.content_type.as_deref(), Some(PROBLEM_JSON));
        }
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn only_a_pending_order_can_be_withdrawn(pool: PgPool) {
        let api = testing::api(&pool);
        let client = api.actor(Role::Client).await;
        let operator = api.actor(Role::Operator).await;
        let withdrawn = api.order(&client, 12).await;
        let planned = api.order(&client, 12).await;
        let by_staff = api.order(&client, 12).await;
        testing::take_into_a_plan(&pool, 12, &[&planned]).await;

        let cancel = |id: &str| format!("/api/orders/{id}/cancel");
        let reply = api
            .post(Some(&client), &cancel(&withdrawn), Value::Null)
            .await;
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(reply.body["status"], "cancelled");

        // Once is all it takes, and a second time says so.
        let again = api
            .post(Some(&client), &cancel(&withdrawn), Value::Null)
            .await;
        assert_eq!(again.status, StatusCode::CONFLICT);
        assert!(
            again.body["detail"].as_str().unwrap().contains("cancelled"),
            "{}",
            again.body
        );

        let held = api
            .post(Some(&client), &cancel(&planned), Value::Null)
            .await;
        assert_eq!(held.status, StatusCode::CONFLICT);
        assert!(
            held.body["detail"].as_str().unwrap().contains("a plan has"),
            "{}",
            held.body
        );
        let read = api
            .get(Some(&client), &format!("/api/orders/{planned}"))
            .await;
        assert_eq!(read.body["status"], "planned");

        // An operator may withdraw anybody's.
        let reply = api
            .post(Some(&operator), &cancel(&by_staff), Value::Null)
            .await;
        assert_eq!(reply.body["status"], "cancelled");
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn an_order_that_makes_no_sense_is_refused_with_the_reason(pool: PgPool) {
        let api = testing::api(&pool);
        let client = api.actor(Role::Client).await;
        let with = |change: fn(&mut Value)| {
            let mut asked = body(12);
            change(&mut asked);
            asked
        };

        let refused = api
            .post(
                Some(&client),
                "/api/orders",
                with(|order| order["demand"] = json!(0)),
            )
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(refused.content_type.as_deref(), Some(PROBLEM_JSON));
        assert_eq!(
            refused.body["errors"],
            json!([{ "field": "demand", "message": "must be at least 1" }])
        );

        // What the domain types refuse as they are read says why in `detail`.
        for (change, reason) in [
            (
                (|order| order["location"]["lat"] = json!(95.0)) as fn(&mut Value),
                "not a position",
            ),
            (|order| order["kind"] = json!("teleport"), "teleport"),
            (|order| order["demand"] = json!(-1), "invalid value"),
            (
                |order| order["demand"] = json!(4_294_967_296_u64),
                "invalid value",
            ),
            (|order| order["service_date"] = json!("tomorrow"), "invalid"),
            (
                |order| {
                    order.as_object_mut().unwrap().remove("location");
                },
                "missing field `location`",
            ),
        ] {
            let refused = api.post(Some(&client), "/api/orders", with(change)).await;
            assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{reason}");
            assert_eq!(refused.content_type.as_deref(), Some(PROBLEM_JSON));
            let detail = refused.body["detail"].as_str().unwrap();
            assert!(detail.contains(reason), "{detail}");
        }

        // Nothing of all that was stored.
        assert_eq!(
            ids(&api.get(Some(&client), "/api/orders").await.body).len(),
            0
        );
    }
}
