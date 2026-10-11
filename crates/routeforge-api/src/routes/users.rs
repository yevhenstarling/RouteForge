//! Users: who can log in, and as what. For admins.
//!
//! An account is opened as a client's (see `auth`). An admin makes operators and
//! admins of them here.

use actix_web::{get, put, web};
use routeforge_domain::db::users;
use routeforge_domain::{RoleChange, User, UserId};
use utoipa_actix_web::service_config::ServiceConfig;
use uuid::Uuid;

use crate::app::{AppState, NotAllowed, NotLoggedIn, NotNow, NotThere};
use crate::auth::Admin;
use crate::error::ApiError;

const NO_SUCH_USER: ApiError = ApiError::NotFound("there is no such user");

pub(crate) fn configure(config: &mut ServiceConfig<'_>) {
    config
        .service(list_users)
        .service(get_user)
        .service(set_user_role);
}

/// List the users
///
/// All of them, oldest first.
#[utoipa::path(
    tag = "users",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "Every user", body = [User]),
        NotLoggedIn,
        NotAllowed,
    )
)]
#[get("/api/users")]
async fn list_users(
    _: Admin,
    state: web::Data<AppState>,
) -> Result<web::Json<Vec<User>>, ApiError> {
    Ok(web::Json(users::list(&state.pool).await?))
}

/// Read one user
#[utoipa::path(
    tag = "users",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The user", body = User),
        NotLoggedIn,
        NotAllowed,
        NotThere,
    )
)]
#[get("/api/users/{id}")]
async fn get_user(
    _: Admin,
    state: web::Data<AppState>,
    path: web::Path<Uuid>,
) -> Result<web::Json<User>, ApiError> {
    users::get(&state.pool, path.into_inner().into())
        .await?
        .map(web::Json)
        .ok_or(NO_SUCH_USER)
}

/// Change what a user may do
///
/// The new role is in the tokens the user gets from now on. One they already hold
/// keeps the old role until it expires.
///
/// An admin cannot change their own role, so there is always an admin left.
#[utoipa::path(
    tag = "users",
    security(("bearer" = [])),
    responses(
        (status = 200, description = "The user, with the new role", body = User),
        NotLoggedIn,
        NotAllowed,
        NotThere,
        NotNow,
    )
)]
#[put("/api/users/{id}/role")]
async fn set_user_role(
    Admin(admin): Admin,
    state: web::Data<AppState>,
    path: web::Path<Uuid>,
    body: web::Json<RoleChange>,
) -> Result<web::Json<User>, ApiError> {
    let id = UserId::from(path.into_inner());
    if id == admin.id {
        return Err(ApiError::Conflict(
            "an admin cannot change their own role: another admin has to".to_owned(),
        ));
    }
    users::set_role(&state.pool, id, body.role)
        .await?
        .map(web::Json)
        .ok_or(NO_SUCH_USER)
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use routeforge_domain::{Role, UserId};
    use serde_json::json;
    use sqlx::PgPool;

    use crate::testing;

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn an_admin_lists_users_and_changes_what_they_may_do(pool: PgPool) {
        let api = testing::api(&pool);
        let admin = api.actor(Role::Admin).await;
        // Ada opens her account herself, so that she can log in to it.
        let ada = json!({ "email": "ada@example.com", "password": "correct horse" });
        let registered = api.post(None, "/api/auth/register", ada.clone()).await;
        assert_eq!(registered.body["role"], "client");
        let path = format!("/api/users/{}", registered.body["id"].as_str().unwrap());
        let token_of =
            |session: &testing::Reply| session.body["token"].as_str().unwrap().to_owned();
        let couriers_with = async |token: &str| {
            let request = testing::request("GET", "/api/couriers", None)
                .insert_header(("Authorization", format!("Bearer {token}")));
            api.send(request).await.status
        };
        let before = token_of(&api.post(None, "/api/auth/login", ada.clone()).await);
        assert_eq!(couriers_with(&before).await, StatusCode::FORBIDDEN);

        let listed = api.get(Some(&admin), "/api/users").await;
        assert_eq!(listed.status, StatusCode::OK);
        assert_eq!(listed.body, json!([admin.user, registered.body]));
        // What is stored of a password never leaves.
        assert!(!listed.text.contains("hash"), "{}", listed.text);
        assert_eq!(api.get(Some(&admin), &path).await.body, registered.body);

        let promoted = api
            .put(
                Some(&admin),
                &format!("{path}/role"),
                json!({ "role": "operator" }),
            )
            .await;
        assert_eq!(promoted.status, StatusCode::OK);
        assert_eq!(promoted.body["role"], "operator");
        assert_eq!(promoted.body["id"], registered.body["id"]);

        // The token Ada holds was made for a client, and stays one until it expires.
        assert_eq!(couriers_with(&before).await, StatusCode::FORBIDDEN);
        // The one her next login gives is an operator's.
        let after = token_of(&api.post(None, "/api/auth/login", ada).await);
        assert_eq!(couriers_with(&after).await, StatusCode::OK);
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn an_admin_cannot_give_up_their_own_role(pool: PgPool) {
        let api = testing::api(&pool);
        let admin = api.actor(Role::Admin).await;
        let own = format!("/api/users/{}/role", admin.user.id);

        let refused = api
            .put(Some(&admin), &own, json!({ "role": "client" }))
            .await;
        assert_eq!(refused.status, StatusCode::CONFLICT);
        let read = api
            .get(Some(&admin), &format!("/api/users/{}", admin.user.id))
            .await;
        assert_eq!(read.body["role"], "admin");

        // Another admin can.
        let other = api.actor(Role::Admin).await;
        let done = api
            .put(Some(&other), &own, json!({ "role": "client" }))
            .await;
        assert_eq!(done.body["role"], "client");
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_role_that_is_none_and_a_user_who_is_not_there(pool: PgPool) {
        let api = testing::api(&pool);
        let admin = api.actor(Role::Admin).await;
        let ada = api.actor(Role::Client).await;

        let refused = api
            .put(
                Some(&admin),
                &format!("/api/users/{}/role", ada.user.id),
                json!({ "role": "courier" }),
            )
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);

        let nobody = format!("/api/users/{}", UserId::generate());
        assert_eq!(
            api.get(Some(&admin), &nobody).await.status,
            StatusCode::NOT_FOUND
        );
        let change = api
            .put(
                Some(&admin),
                &format!("{nobody}/role"),
                json!({ "role": "operator" }),
            )
            .await;
        assert_eq!(change.status, StatusCode::NOT_FOUND);
    }
}
