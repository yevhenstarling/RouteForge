//! Couriers: who drives the routes. For operators and admins.
//!
//! A courier is never deleted, because stored routes refer to it. One that is to get
//! no more routes is made inactive.

use actix_web::{HttpResponse, get, patch, post, web};
use routeforge_domain::db::couriers;
use routeforge_domain::{Courier, CourierChange, NewCourier};
use utoipa_actix_web::service_config::ServiceConfig;
use uuid::Uuid;

use crate::app::{AppState, NotAcceptable, NotAllowed, NotLoggedIn, NotThere};
use crate::auth::Staff;
use crate::error::ApiError;
use crate::validate;

const NO_SUCH_COURIER: ApiError = ApiError::NotFound("there is no such courier");

pub(crate) fn configure(config: &mut ServiceConfig<'_>) {
    config
        .service(create_courier)
        .service(list_couriers)
        .service(get_courier)
        .service(change_courier);
}

/// Add a courier
///
/// The courier is active from the start: the next plan may give it a route.
#[utoipa::path(
    tag = "couriers",
    security(("bearer" = [])),
    responses(
        (status = 201, description = "The courier", body = Courier),
        NotLoggedIn,
        NotAllowed,
        NotAcceptable,
    )
)]
#[post("/api/couriers")]
async fn create_courier(
    _: Staff,
    state: web::Data<AppState>,
    body: web::Json<NewCourier>,
) -> Result<HttpResponse, ApiError> {
    let new = body.into_inner();
    validate::courier(&new)?;
    let courier = couriers::create(&state.pool, &new).await?;
    Ok(HttpResponse::Created().json(courier))
}

/// List the couriers
///
/// All of them, oldest first, the inactive ones included.
#[utoipa::path(
    tag = "couriers",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "Every courier", body = [Courier]),
        NotLoggedIn,
        NotAllowed,
    )
)]
#[get("/api/couriers")]
async fn list_couriers(
    _: Staff,
    state: web::Data<AppState>,
) -> Result<web::Json<Vec<Courier>>, ApiError> {
    Ok(web::Json(couriers::list(&state.pool).await?))
}

/// Read one courier
#[utoipa::path(
    tag = "couriers",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The courier", body = Courier),
        NotLoggedIn,
        NotAllowed,
        NotThere,
    )
)]
#[get("/api/couriers/{id}")]
async fn get_courier(
    _: Staff,
    state: web::Data<AppState>,
    path: web::Path<Uuid>,
) -> Result<web::Json<Courier>, ApiError> {
    couriers::get(&state.pool, path.into_inner().into())
        .await?
        .map(web::Json)
        .ok_or(NO_SUCH_COURIER)
}

/// Take a courier out of planning, or put one back
///
/// An inactive courier gets no route from the plans that follow. The routes it has
/// stay as they are.
#[utoipa::path(
    tag = "couriers",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The courier, as it is now", body = Courier),
        NotLoggedIn,
        NotAllowed,
        NotThere,
    )
)]
#[patch("/api/couriers/{id}")]
async fn change_courier(
    _: Staff,
    state: web::Data<AppState>,
    path: web::Path<Uuid>,
    body: web::Json<CourierChange>,
) -> Result<web::Json<Courier>, ApiError> {
    couriers::set_active(&state.pool, path.into_inner().into(), body.active)
        .await?
        .map(web::Json)
        .ok_or(NO_SUCH_COURIER)
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use routeforge_domain::{CourierId, Role};
    use serde_json::json;
    use sqlx::PgPool;

    use crate::testing;

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_courier_is_added_read_and_switched_off_and_on(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;

        let added = api
            .post(
                Some(&operator),
                "/api/couriers",
                json!({ "name": "Van 1", "capacity": 40 }),
            )
            .await;
        assert_eq!(added.status, StatusCode::CREATED);
        assert_eq!(added.body["name"], "Van 1");
        assert_eq!(added.body["capacity"], 40);
        assert_eq!(added.body["active"], true);
        let path = format!("/api/couriers/{}", added.body["id"].as_str().unwrap());

        let read = api.get(Some(&operator), &path).await;
        assert_eq!(read.body, added.body);

        let resting = api
            .patch(Some(&operator), &path, json!({ "active": false }))
            .await;
        assert_eq!(resting.status, StatusCode::OK);
        assert_eq!(resting.body["active"], false);
        // Still listed: stored routes may refer to it.
        let listed = api.get(Some(&operator), "/api/couriers").await;
        assert_eq!(listed.body, json!([resting.body]));

        let back = api
            .patch(Some(&operator), &path, json!({ "active": true }))
            .await;
        assert_eq!(back.body["active"], true);
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_courier_that_cannot_be_is_refused_and_one_that_is_not_there_is_missed(pool: PgPool) {
        let api = testing::api(&pool);
        let operator = api.actor(Role::Operator).await;

        let refused = api
            .post(
                Some(&operator),
                "/api/couriers",
                json!({ "name": "", "capacity": 0 }),
            )
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        let fields: Vec<&str> = refused.body["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|error| error["field"].as_str().unwrap())
            .collect();
        assert_eq!(fields, ["name", "capacity"]);
        assert_eq!(
            api.get(Some(&operator), "/api/couriers").await.body,
            json!([])
        );

        let nobody = format!("/api/couriers/{}", CourierId::generate());
        assert_eq!(
            api.get(Some(&operator), &nobody).await.status,
            StatusCode::NOT_FOUND
        );
        let change = api
            .patch(Some(&operator), &nobody, json!({ "active": false }))
            .await;
        assert_eq!(change.status, StatusCode::NOT_FOUND);

        // An id that is no id is a request that cannot be read.
        let reply = api.get(Some(&operator), "/api/couriers/van-1").await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    }
}
