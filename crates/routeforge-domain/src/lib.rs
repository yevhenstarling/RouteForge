//! RouteForge domain types and geo helpers.
//!
//! Three services share this crate:
//!
//! - Everything at the top level is plain data and arithmetic: identifiers, the things
//!   the system keeps (users, the depot, couriers, orders, plans, routes, jobs) in the
//!   shape the HTTP API speaks them, and distances on the globe. The Telegram bot uses
//!   this part only.
//! - With the `db` feature, the `db` module adds the PostgreSQL schema, the repositories
//!   and the job queue (D3). The API and the solver enable it.
//!
//! The decisions cited as D1, D2, … are in `docs/architecture.md`.

mod geo;
mod ids;
mod job;
mod model;
mod text;

pub use geo::{
    DistanceProvider, DistanceTable, EARTH_RADIUS_M, Haversine, InvalidLatLon, LatLon, Planar,
    Projection, haversine_m,
};
pub use ids::{CourierId, DepotId, JobId, OrderId, PlanId, RouteId, UserId, WorkerId};
pub use job::{Job, JobKind, JobPayload, JobProgress, JobStatus, WorkerInfo};
pub use model::{
    Courier, Credentials, Depot, NewCourier, NewDepot, NewOrder, NewPlan, NewRoute, NewUser, Order,
    OrderFilter, OrderKind, OrderStatus, Plan, PlanCounts, PlanStatus, Role, Route, RouteStop,
    User,
};
pub use text::UnknownVariant;
