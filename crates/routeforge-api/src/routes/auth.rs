//! Opening an account and logging in.

use actix_web::{HttpResponse, post, web};
use routeforge_domain::db::users;
use routeforge_domain::{Login, NewUser, Role, Session, User};
use utoipa_actix_web::service_config::ServiceConfig;

use crate::app::{AppState, NotAcceptable};
use crate::error::ApiError;
use crate::validate;

pub(crate) fn configure(config: &mut ServiceConfig<'_>) {
    config.service(register).service(login);
}

/// Open an account
///
/// Anyone can. The account is a client's: it places orders and sees its own. An admin
/// can make it an operator's or an admin's.
#[utoipa::path(
    tag = "auth",
    responses(
        (status = 201, description = "The account", body = User),
        (status = 409, description = "The address already has an account",
            body = routeforge_domain::Problem, content_type = "application/problem+json"),
        NotAcceptable,
    )
)]
#[post("/api/auth/register")]
async fn register(
    state: web::Data<AppState>,
    body: web::Json<Login>,
) -> Result<HttpResponse, ApiError> {
    let asked = body.into_inner();
    validate::registration(&asked)?;
    let new = NewUser {
        email: asked.email,
        password_hash: state.passwords.hash(asked.password).await?,
        role: Role::Client,
    };
    let user = users::create(&state.pool, &new).await?;
    Ok(HttpResponse::Created().json(user))
}

/// Log in
///
/// The token of the answer goes with every later request, as
/// `Authorization: Bearer <token>`. It says who the user is and what their role was at
/// this moment, and is good for `expires_in` seconds.
#[utoipa::path(
    tag = "auth",
    responses(
        (status = 200, description = "The token, and who it speaks for", body = Session),
        (status = 401, description = "The address or the password is wrong",
            body = routeforge_domain::Problem, content_type = "application/problem+json"),
    )
)]
#[post("/api/auth/login")]
async fn login(
    state: web::Data<AppState>,
    body: web::Json<Login>,
) -> Result<web::Json<Session>, ApiError> {
    let sent = body.into_inner();
    let (user, hash) = match users::find_by_email(&state.pool, &sent.email).await? {
        Some(account) => (Some(account.user), Some(account.password_hash)),
        None => (None, None),
    };
    // Checked even when there is no such account, so that the time it takes does not
    // say whether there is.
    let right = state.passwords.verify(sent.password, hash).await?;
    match user {
        Some(user) if right => Ok(web::Json(state.tokens.issue(user)?)),
        // Which of the two was wrong is not said either.
        _ => Err(ApiError::Unauthorized(
            "the address or the password is wrong",
        )),
    }
}
