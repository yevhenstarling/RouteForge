//! The routes, a module for each kind of thing the API serves.
//!
//! A handler says who may call it by the type of an argument (see `auth`), takes ids
//! as plain UUIDs in the path, and describes itself for the OpenAPI document with
//! `#[utoipa::path]`. Its name is the operation's id there, so no two share one.

use utoipa_actix_web::service_config::ServiceConfig;

mod auth;
mod couriers;
mod demo;
mod depot;
mod health;
mod orders;
mod plans;
mod system;
mod users;

/// Adds every route, and with it what the route says about itself.
pub(crate) fn configure(config: &mut ServiceConfig<'_>) {
    config
        .configure(health::configure)
        .configure(auth::configure)
        .configure(orders::configure)
        .configure(depot::configure)
        .configure(couriers::configure)
        .configure(users::configure)
        .configure(plans::configure)
        .configure(system::configure)
        .configure(demo::configure);
}
