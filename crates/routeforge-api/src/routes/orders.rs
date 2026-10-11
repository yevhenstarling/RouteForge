//! Orders: parcels to deliver or to pick up.
//!
//! An order belongs to whoever placed it. A client sees and withdraws their own, and
//! to a client an order of somebody else's is not there at all: the answer is the
//! `404` of an id that names nothing. Operators and admins see every order.

use actix_web::{HttpResponse, get, post, web};
use routeforge_domain::db::orders;
use routeforge_domain::{Order, OrderFilter, OrderId, OrderRequest, OrderStatus};
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
        .service(get_order)
        .service(cancel_order);
}

/// Place an order
///
/// The order is the caller's, and is pending until a plan gives it to a courier.
#[utoipa::path(
    tag = "orders",
    security(("bearer" = [])),
    responses(
        (status = 201, description = "The order", body = Order),
        NotLoggedIn,
        NotAcceptable,
    )
)]
#[post("/api/orders")]
async fn create_order(
    user: AuthUser,
    state: web::Data<AppState>,
    body: web::Json<OrderRequest>,
) -> Result<HttpResponse, ApiError> {
    let request = body.into_inner();
    validate::order(&request)?;
    let order = orders::create(&state.pool, &request.placed_by(user.id)).await?;
    Ok(HttpResponse::Created().json(order))
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
    use crate::testing::{self, date};

    fn body(day: u32) -> Value {
        json!({
            "kind": "delivery",
            "service_date": date(day),
            "location": { "lat": 50.45, "lon": 30.52 },
            "address": "Khreshchatyk St, 22",
            "demand": 2,
        })
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
