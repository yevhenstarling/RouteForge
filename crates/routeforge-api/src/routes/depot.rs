//! The depot: where every route starts and ends.
//!
//! There is one at a time, and setting it adds a version (D21). A plan keeps the one
//! it was made for.

use actix_web::{get, put, web};
use routeforge_domain::db::depots;
use routeforge_domain::{Depot, NewDepot};
use utoipa_actix_web::service_config::ServiceConfig;

use crate::app::{AppState, NotAcceptable, NotAllowed, NotLoggedIn, NotThere};
use crate::auth::{Admin, AuthUser};
use crate::error::ApiError;
use crate::validate;

pub(crate) fn configure(config: &mut ServiceConfig<'_>) {
    config.service(get_depot).service(set_depot);
}

/// Read the depot
///
/// As it is now. Anyone who is logged in may know where the parcels come from.
#[utoipa::path(
    tag = "depot",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The depot", body = Depot),
        NotLoggedIn,
        NotThere,
    )
)]
#[get("/api/depot")]
async fn get_depot(_: AuthUser, state: web::Data<AppState>) -> Result<web::Json<Depot>, ApiError> {
    depots::current(&state.pool)
        .await?
        .map(web::Json)
        .ok_or(ApiError::NotFound("no depot has been set yet"))
}

/// Set the depot
///
/// For admins. From now on plans start from here. Plans that were made before keep
/// the depot they were made for.
#[utoipa::path(
    tag = "depot",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The depot, as it is from now on", body = Depot),
        NotLoggedIn,
        NotAllowed,
        NotAcceptable,
    )
)]
#[put("/api/depot")]
async fn set_depot(
    _: Admin,
    state: web::Data<AppState>,
    body: web::Json<NewDepot>,
) -> Result<web::Json<Depot>, ApiError> {
    let new = body.into_inner();
    validate::depot(&new)?;
    Ok(web::Json(depots::set(&state.pool, &new).await?))
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use routeforge_domain::Role;
    use serde_json::json;
    use sqlx::PgPool;

    use crate::testing;

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn the_depot_is_the_one_set_last(pool: PgPool) {
        let api = testing::api(&pool);
        let admin = api.actor(Role::Admin).await;
        let client = api.actor(Role::Client).await;

        let none = api.get(Some(&client), "/api/depot").await;
        assert_eq!(none.status, StatusCode::NOT_FOUND);

        let left = json!({ "name": "Left bank", "location": { "lat": 50.45, "lon": 30.6 } });
        let first = api.put(Some(&admin), "/api/depot", left).await;
        assert_eq!(first.status, StatusCode::OK);
        assert_eq!(first.body["name"], "Left bank");

        let right = json!({ "name": "Right bank", "location": { "lat": 50.45, "lon": 30.45 } });
        let second = api.put(Some(&admin), "/api/depot", right).await;
        assert_ne!(second.body["id"], first.body["id"], "a version of its own");

        let current = api.get(Some(&client), "/api/depot").await;
        assert_eq!(current.status, StatusCode::OK);
        assert_eq!(current.body, second.body);
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_depot_needs_a_name_and_a_place_on_the_globe(pool: PgPool) {
        let api = testing::api(&pool);
        let admin = api.actor(Role::Admin).await;

        let nameless = json!({ "name": " ", "location": { "lat": 50.45, "lon": 30.6 } });
        let refused = api.put(Some(&admin), "/api/depot", nameless).await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(refused.body["errors"][0]["field"], "name");

        let nowhere = json!({ "name": "Depot", "location": { "lat": 50.45, "lon": 300.0 } });
        let refused = api.put(Some(&admin), "/api/depot", nowhere).await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        let detail = refused.body["detail"].as_str().unwrap();
        assert!(detail.contains("not a position"), "{detail}");

        assert_eq!(
            api.get(Some(&admin), "/api/depot").await.status,
            StatusCode::NOT_FOUND
        );
    }
}
