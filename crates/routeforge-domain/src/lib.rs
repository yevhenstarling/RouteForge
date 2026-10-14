//! RouteForge domain types and geo helpers.
//!
//! Three services share this crate:
//!
//! - Everything at the top level is plain data and arithmetic: identifiers, the things
//!   the system keeps (users, the depot, couriers, orders, plans, routes, jobs) in the
//!   shape the HTTP API speaks them, the other bodies of its requests and responses,
//!   and distances on the globe. The Telegram bot uses this part only.
//! - With the `db` feature, the `db` module adds the PostgreSQL schema, the repositories
//!   and the job queue (D3). The API and the solver enable it.
//! - With the `openapi` feature, the types the HTTP API sends and receives describe
//!   themselves for its OpenAPI document. The API enables it.
//! - With the `service` feature, the `service` module has what the three services
//!   share beside their types: how they log, how they hear that they are to stop, and
//!   how long they wait before trying again (D81). All three enable it.
//!
//! The decisions cited as D1, D2, … are in `docs/architecture.md`.

#[cfg(feature = "db")]
pub mod db;
mod dto;
mod geo;
mod ids;
mod job;
mod model;
#[cfg(feature = "service")]
pub mod service;
mod text;

pub use dto::{
    CourierChange, DemoCreated, DemoRequest, FieldError, Login, OrderRequest, OrdersChanged,
    PlanChanged, PlanRequest, Problem, RoleChange, Session, SystemStats, TelegramLogin,
};
pub use geo::{
    DistanceProvider, DistanceTable, Distances, EARTH_RADIUS_M, Haversine, InvalidLatLon,
    InvalidPolyline, LatLon, Planar, Polyline, Projection, haversine_m,
};
pub use ids::{
    CourierId, DepotId, IdempotencyKey, InvalidKey, JobId, OrderId, PlanId, RouteId, UserId,
    WorkerId,
};
pub use job::{
    Job, JobKind, JobPayload, JobProgress, JobStatus, QueueDepth, RouteJob, RouteProgress,
    WorkerInfo,
};
pub use model::{
    Courier, Credentials, Depot, NewCourier, NewDepot, NewOrder, NewPlan, NewRoute, NewUser, Order,
    OrderDay, OrderFilter, OrderKind, OrderStatus, OrderStop, Plan, PlanCounts, PlanFilter,
    PlanStatus, Role, Route, RouteBudget, RouteStop, UnlimitedBudget, User,
};
pub use text::UnknownVariant;
