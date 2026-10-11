//! Whether the API is up, and whether it can do its work.

use actix_web::{get, web};
use routeforge_domain::db;
use serde::Serialize;
use tracing::warn;
use utoipa::ToSchema;
use utoipa_actix_web::service_config::ServiceConfig;

use crate::app::AppState;
use crate::error::ApiError;

pub(crate) fn configure(config: &mut ServiceConfig<'_>) {
    config.service(healthz).service(readyz);
}

/// The answer of a health check that went well.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Health {
    status: &'static str,
}

/// Whether the process is up
///
/// Answers without asking anything of the database.
#[utoipa::path(
    tag = "health",
    responses((status = 200, description = "The API is up", body = Health))
)]
#[get("/healthz")]
async fn healthz() -> web::Json<Health> {
    web::Json(Health { status: "ok" })
}

/// Whether the API can do its work
///
/// It can when the database answers and has the schema this build needs.
#[utoipa::path(
    tag = "health",
    responses(
        (status = 200, description = "The API is ready", body = Health),
        (status = 503, description = "The database does not answer, or lacks the schema",
            body = routeforge_domain::Problem, content_type = "application/problem+json"),
    )
)]
#[get("/readyz")]
async fn readyz(state: web::Data<AppState>) -> Result<web::Json<Health>, ApiError> {
    match db::is_migrated(&state.pool).await {
        Ok(true) => Ok(web::Json(Health { status: "ready" })),
        Ok(false) => Err(ApiError::Unavailable(
            "the database does not have the schema this build needs",
        )),
        Err(error) => {
            warn!(%error, "could not look at the schema");
            Err(ApiError::Unavailable("the database does not answer"))
        }
    }
}
