//! The application: what its handlers share, and how its parts are put together.
//!
//! [`app`] builds the whole of it, for `main` and for the tests alike, so that what is
//! tested is what runs.

use std::collections::BTreeMap;
use std::time::Duration;

use actix_web::body::MessageBody;
use actix_web::dev::{ServiceFactory, ServiceRequest, ServiceResponse};
use actix_web::middleware::from_fn;
use actix_web::{App, web};
use routeforge_domain::{Problem, RouteJob, RouteProgress};
use sqlx::PgPool;
use tracing_actix_web::TracingLogger;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::openapi::{ContentBuilder, Ref, RefOr, Response, ResponseBuilder};
use utoipa::{IntoResponses, Modify, OpenApi};
use utoipa_actix_web::AppExt;
use utoipa_swagger_ui::SwaggerUi;

use crate::auth::{Passwords, ServiceKey, Tokens};
use crate::config::Limits;
use crate::error::{self, PROBLEM_JSON};
use crate::events::Events;
use crate::request_id::{self, RequestSpan};
use crate::stream::Pace;
use crate::{page, routes};

/// Where the OpenAPI document is served.
pub(crate) const OPENAPI_PATH: &str = "/api/openapi.json";

/// The names of the security schemes in the OpenAPI document: a user's token, and the
/// key of the bot.
const BEARER: &str = "bearer";
const SERVICE_KEY: &str = "service_key";

/// What the handlers share.
pub(crate) struct AppState {
    pub(crate) pool: PgPool,
    pub(crate) tokens: Tokens,
    pub(crate) passwords: Passwords,
    /// The key the bot shows. An API that has none takes no bot (D60).
    pub(crate) bot_key: Option<ServiceKey>,
    /// How long a token is good for that the bot was given for a Telegram user.
    pub(crate) bot_token_ttl: Duration,
    pub(crate) limits: Limits,
    /// A worker seen within this span counts as alive.
    pub(crate) worker_alive: Duration,
    /// What the database announces, for the event streams (D50).
    pub(crate) events: Events,
    /// How the event streams pace themselves.
    pub(crate) pace: Pace,
}

/// The application.
///
/// From the outside in, a request passes the middleware that gives it its id, the one
/// that opens its span, and then the routes. The page is at `/`, Swagger UI is at
/// `/docs/`, and everything that matches no route is a `404` like any other error.
pub(crate) fn app(
    state: web::Data<AppState>,
) -> App<
    impl ServiceFactory<
        ServiceRequest,
        Config = (),
        Response = ServiceResponse<impl MessageBody>,
        Error = actix_web::Error,
        InitError = (),
    >,
> {
    let (app, openapi) = App::new()
        .into_utoipa_app()
        .openapi(ApiDoc::openapi())
        .app_data(state)
        .app_data(error::json_config())
        .app_data(error::path_config())
        .app_data(error::query_config())
        .configure(routes::configure)
        .split_for_parts();

    app.service(SwaggerUi::new("/docs/{_:.*}").url(OPENAPI_PATH, openapi))
        .service(web::redirect("/docs", "/docs/"))
        .configure(page::configure)
        .default_service(web::to(error::not_found))
        // The last one wrapped is the first a request meets.
        .wrap(TracingLogger::<RequestSpan>::new())
        .wrap(from_fn(request_id::assign))
}

/// What the OpenAPI document says before the routes add themselves to it.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "RouteForge",
        description = "Turns a day's parcel orders into courier routes.\n\n\
            To try it here: `POST /api/auth/login`, copy the `token` of the answer, \
            press **Authorize** and paste it.\n\n\
            The same API is behind the map at `/`."
    ),
    // `Problem` is what the refusals name, and the other two are what the events of a
    // stream carry. None of them is the body of a response, where utoipa would find it.
    components(schemas(Problem, RouteJob, RouteProgress)),
    modifiers(&BearerToken),
    tags(
        (name = "health", description = "Whether the API is up, and ready"),
        (name = "auth", description = "Accounts and logins"),
        (name = "orders", description = "Parcels to deliver or to pick up. A client sees their own"),
        (name = "depot", description = "Where every route starts and ends"),
        (name = "couriers", description = "Who drives the routes. For operators and admins"),
        (name = "users", description = "Who can log in, and as what. For admins"),
        (name = "plans", description = "Planning a day, and the routes that come of it. For operators and admins"),
        (name = "system", description = "The queue and its workers. For operators and admins"),
        (name = "demo", description = "Data for a demonstration. For admins"),
    )
)]
struct ApiDoc;

/// Declares the schemes the protected routes name: a user's token sent as a bearer,
/// and, for the one route that is the bot's, its key sent the same way.
struct BearerToken;

impl Modify for BearerToken {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let token = HttpBuilder::new()
            .scheme(HttpAuthScheme::Bearer)
            .bearer_format("JWT")
            .description(Some(
                "The `token` that `POST /api/auth/login` answers with.",
            ))
            .build();
        let key = HttpBuilder::new()
            .scheme(HttpAuthScheme::Bearer)
            .description(Some(
                "The key of the Telegram bot: what the API was started with as \
                 `BOT_SERVICE_KEY`. No token of a user's stands in for it.",
            ))
            .build();
        let components = openapi.components.get_or_insert_default();
        components.add_security_scheme(BEARER, SecurityScheme::Http(token));
        components.add_security_scheme(SERVICE_KEY, SecurityScheme::Http(key));
    }
}

/// A refusal as the OpenAPI document describes it: a problem (RFC 9457).
fn refusal(status: &str, description: &str) -> BTreeMap<String, RefOr<Response>> {
    let problem = ContentBuilder::new()
        .schema(Some(Ref::from_schema_name("Problem")))
        .build();
    let response = ResponseBuilder::new()
        .description(description)
        .content(PROBLEM_JSON, problem)
        .build();
    BTreeMap::from([(status.to_owned(), RefOr::T(response))])
}

/// Declares a refusal that a handler lists among its `responses(...)`.
///
/// They are types that have no value: all they are for is to be named there.
macro_rules! refusals {
    ($($(#[$doc:meta])* $name:ident => $status:literal, $description:literal;)+) => {
        $(
            $(#[$doc])*
            pub(crate) enum $name {}

            impl IntoResponses for $name {
                fn responses() -> BTreeMap<String, RefOr<Response>> {
                    refusal($status, $description)
                }
            }
        )+
    };
}

refusals! {
    /// `401`, on every route that needs a token.
    NotLoggedIn => "401", "No token was sent, or the one sent is not valid or has expired";
    /// `401`, on the route that needs the key of the bot.
    NoKey => "401", "The key of the bot was not sent, is not the one this API has, or this API has none";
    /// `403`, on the routes that need a role.
    NotAllowed => "403", "The caller's role does not allow this";
    /// `404`.
    NotThere => "404", "There is no such thing, or none the caller may know of";
    /// `409`.
    NotNow => "409", "The thing is not in a state for this";
    /// `422`.
    NotAcceptable => "422", "Something in the request cannot be accepted. `errors` names the fields, where it is known which";
}

#[cfg(test)]
mod tests;
