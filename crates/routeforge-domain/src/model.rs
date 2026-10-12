//! Users, the depot, couriers, orders, plans and routes.
//!
//! These types are what the HTTP API sends and receives and what the repositories in
//! `db` read and write. A `New…` type holds what creating the thing needs.

use std::fmt;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::geo::LatLon;
use crate::ids::{CourierId, DepotId, OrderId, PlanId, RouteId, UserId};
use crate::text::text_enum;

text_enum! {
    /// What a user may do.
    Role {
        /// Places orders and sees their own.
        Client => "client",
        /// Sees every order and manages couriers and plans.
        Operator => "operator",
        /// Everything an operator may do, and manages users and the depot.
        Admin => "admin",
    }
}

text_enum! {
    /// Which way a parcel travels (D1).
    OrderKind {
        /// From the depot to the client.
        Delivery => "delivery",
        /// From the client back to the depot.
        Pickup => "pickup",
    }
}

text_enum! {
    /// Where an order stands.
    OrderStatus {
        /// Accepted and not on a route.
        Pending => "pending",
        /// A plan has given it to a courier.
        Planned => "planned",
        /// Withdrawn before any plan took it.
        Cancelled => "cancelled",
    }
}

text_enum! {
    /// Where a plan stands.
    PlanStatus {
        /// Waiting for a solver to cluster its orders.
        Queued => "queued",
        /// Clustered. Its routes are being optimized.
        Routing => "routing",
        /// Every route is stored.
        Done => "done",
        /// A job ran out of attempts. The plan's orders are pending again.
        Failed => "failed",
        /// Stopped by an operator. The plan's orders are pending again.
        Cancelled => "cancelled",
        /// Was done, and an operator took it back (D30). Its routes are deleted and
        /// its orders are pending again.
        Discarded => "discarded",
    }
}

impl PlanStatus {
    /// Whether the plan still has work on the queue. A service date has at most one
    /// such plan.
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Routing)
    }
}

/// Someone who can log in, or who talks to the bot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct User {
    pub id: UserId,
    /// In lower case. An account that was made for a Telegram user has none (D59).
    pub email: Option<String>,
    /// The id Telegram gives the user, if the account is one the bot reaches.
    #[cfg_attr(feature = "openapi", schema(minimum = 1))]
    pub telegram_id: Option<i64>,
    pub role: Role,
    pub created_at: DateTime<Utc>,
}

/// What opening an account with an address and a password needs.
#[derive(Clone, PartialEq, Eq)]
pub struct NewUser {
    /// Stored in lower case, whatever the case here.
    pub email: String,
    /// The hash of the password, never the password.
    pub password_hash: String,
    pub role: Role,
}

/// A user together with the password hash, for checking a login.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub user: User,
    pub password_hash: String,
}

// Written by hand so that a hash cannot reach a log through `{:?}`.
impl fmt::Debug for NewUser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewUser")
            .field("email", &self.email)
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

/// Where every route starts and ends (D1).
///
/// A depot row is never changed. Moving the depot adds a row, and the newest row is
/// the current depot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Depot {
    pub id: DepotId,
    pub name: String,
    pub location: LatLon,
    pub created_at: DateTime<Utc>,
}

/// What setting the depot needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NewDepot {
    pub name: String,
    pub location: LatLon,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Courier {
    pub id: CourierId,
    pub name: String,
    /// How much the courier can have on board at one time. A route may ask for more
    /// than that in all: what is delivered is off before what is picked up comes on
    /// (D65).
    pub capacity: u32,
    /// Only active couriers get routes.
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

/// What creating a courier needs. A new courier is active.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NewCourier {
    pub name: String,
    /// At least 1.
    #[cfg_attr(feature = "openapi", schema(minimum = 1))]
    pub capacity: u32,
}

/// One parcel to deliver or to pick up: one stop on a route (D1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Order {
    pub id: OrderId,
    pub client_id: UserId,
    pub kind: OrderKind,
    pub status: OrderStatus,
    /// The day the courier is to call.
    pub service_date: NaiveDate,
    pub location: LatLon,
    /// What to show a person. Routing uses `location` alone.
    pub address: Option<String>,
    /// How much of a courier's capacity the order takes.
    pub demand: u32,
    /// The plan that gave the order to a courier. Set exactly while it is planned.
    pub plan_id: Option<PlanId>,
    pub created_at: DateTime<Utc>,
}

/// What creating an order needs. A new order is pending.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewOrder {
    pub client_id: UserId,
    pub kind: OrderKind,
    pub service_date: NaiveDate,
    pub location: LatLon,
    pub address: Option<String>,
    /// At least 1.
    pub demand: u32,
}

/// Which orders to list. Every field that is set narrows the list.
///
/// Orders are listed oldest first, which is the order of their ids (D11). So a list is
/// read in pages by asking for at most `limit` orders `after` the last one of the page
/// before.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct OrderFilter {
    /// Only the orders of this client.
    #[cfg_attr(feature = "openapi", param(value_type = Option<Uuid>))]
    pub client_id: Option<UserId>,
    /// Only the orders for this day.
    pub service_date: Option<NaiveDate>,
    /// Only the orders that stand like this.
    #[cfg_attr(feature = "openapi", param(inline))]
    pub status: Option<OrderStatus>,
    /// Only the orders created after the one with this id.
    #[cfg_attr(feature = "openapi", param(value_type = Option<Uuid>))]
    pub after: Option<OrderId>,
    /// At most this many orders.
    pub limit: Option<u32>,
}

/// How long the search for each route of a plan may go on (D31).
///
/// The limits are those of the search itself (D15): a span of time, a number of
/// candidate moves, or both, in which case whichever is reached first ends the search.
/// A value of this type has at least one of them, because annealing never ends on its
/// own (D18): [`RouteBudget::new`] and deserialization both check.
///
/// With moves alone, the same seed gives the same route (D5). A span of time cuts the
/// search wherever the machine has got to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(try_from = "UncheckedBudget")]
pub struct RouteBudget {
    /// Wall-clock time for one route, in milliseconds.
    time_ms: Option<u32>,
    /// Candidate moves for one route. With this limit alone, the same seed gives the
    /// same route.
    iterations: Option<u64>,
}

/// A route budget that sets no limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a route budget needs a limit: a time, a number of moves, or both")]
pub struct UnlimitedBudget;

/// What a budget looks like in JSON, before it is checked.
#[derive(Deserialize)]
struct UncheckedBudget {
    time_ms: Option<u32>,
    iterations: Option<u64>,
}

impl TryFrom<UncheckedBudget> for RouteBudget {
    type Error = UnlimitedBudget;

    fn try_from(unchecked: UncheckedBudget) -> Result<Self, Self::Error> {
        Self::new(unchecked.time_ms, unchecked.iterations)
    }
}

impl RouteBudget {
    /// A budget of `time_ms` milliseconds, of `iterations` candidate moves, or of both.
    pub fn new(time_ms: Option<u32>, iterations: Option<u64>) -> Result<Self, UnlimitedBudget> {
        if time_ms.is_none() && iterations.is_none() {
            return Err(UnlimitedBudget);
        }
        Ok(Self {
            time_ms,
            iterations,
        })
    }

    /// The time one route may take, in milliseconds.
    pub fn time_ms(self) -> Option<u32> {
        self.time_ms
    }

    /// The time one route may take.
    pub fn time(self) -> Option<Duration> {
        self.time_ms.map(|ms| Duration::from_millis(u64::from(ms)))
    }

    /// The candidate moves the search for one route may evaluate.
    pub fn iterations(self) -> Option<u64> {
        self.iterations
    }
}

/// One planning run: the orders of a service date turned into courier routes (D2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Plan {
    pub id: PlanId,
    pub service_date: NaiveDate,
    /// The depot the routes start and end at, as it was when the plan was made.
    pub depot_id: DepotId,
    pub status: PlanStatus,
    /// What every random choice in the plan's routes derives from (D5).
    pub seed: u32,
    /// How long each of the plan's routes may be searched for.
    pub budget: RouteBudget,
    pub created_by: UserId,
    /// Pending orders of the date when the plan was clustered.
    pub orders_total: u32,
    /// How many of them fit no courier. They stay pending.
    pub orders_unassigned: u32,
    /// Routes the plan was split into.
    pub routes_total: u32,
    /// Routes that are stored. The plan is done when this reaches `routes_total`.
    pub routes_done: u32,
    /// Why the plan failed.
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    /// When the plan was done, failed or cancelled. A discarded plan keeps the time
    /// it was done.
    pub finished_at: Option<DateTime<Utc>>,
}

/// Which plans to list: those of one day, newest first.
///
/// A day has one plan in progress at most, and beside it the ones that came before. So
/// they are listed whole, like the couriers (D44).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct PlanFilter {
    /// The day the plans are for.
    pub service_date: NaiveDate,
}

/// What creating a plan needs. A new plan is queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPlan {
    pub service_date: NaiveDate,
    pub depot_id: DepotId,
    pub seed: u32,
    pub budget: RouteBudget,
    pub created_by: UserId,
}

/// What clustering found, for the step from queued to routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanCounts {
    pub orders_total: u32,
    pub orders_unassigned: u32,
    pub routes_total: u32,
}

/// One courier's round trip from the depot through its stops.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Route {
    pub id: RouteId,
    pub plan_id: PlanId,
    pub courier_id: CourierId,
    /// The length of the round trip in metres (D4).
    pub distance_m: u64,
    /// In the order the courier calls.
    pub stops: Vec<RouteStop>,
}

/// One call on a route, with what a map needs to draw it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RouteStop {
    pub order_id: OrderId,
    pub kind: OrderKind,
    pub location: LatLon,
    pub demand: u32,
}

/// What storing a finished route needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewRoute {
    pub plan_id: PlanId,
    pub courier_id: CourierId,
    pub distance_m: u64,
    /// The orders in the order the courier calls.
    pub stops: Vec<OrderId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::testing::assert_text_round_trips;

    #[test]
    fn every_enumeration_has_one_text_everywhere() {
        assert_text_round_trips(Role::ALL);
        assert_text_round_trips(OrderKind::ALL);
        assert_text_round_trips(OrderStatus::ALL);
        assert_text_round_trips(PlanStatus::ALL);
    }

    #[test]
    fn a_plan_is_active_until_it_is_done_failed_or_cancelled() {
        let active: Vec<PlanStatus> = PlanStatus::ALL
            .iter()
            .copied()
            .filter(|status| status.is_active())
            .collect();
        assert_eq!(active, [PlanStatus::Queued, PlanStatus::Routing]);
    }

    #[test]
    fn a_budget_needs_at_least_one_limit() {
        let timed = RouteBudget::new(Some(1_500), None).unwrap();
        assert_eq!(
            (timed.time_ms(), timed.time(), timed.iterations()),
            (Some(1_500), Some(Duration::from_millis(1_500)), None)
        );
        let counted = RouteBudget::new(None, Some(u64::MAX)).unwrap();
        assert_eq!(
            (counted.time(), counted.iterations()),
            (None, Some(u64::MAX))
        );
        let both = RouteBudget::new(Some(u32::MAX), Some(0)).unwrap();
        assert_eq!(
            (both.time_ms(), both.iterations()),
            (Some(u32::MAX), Some(0))
        );

        let error = RouteBudget::new(None, None).unwrap_err();
        assert!(error.to_string().contains("needs a limit"));
    }

    #[test]
    fn json_cannot_smuggle_in_a_budget_without_a_limit() {
        let timed: RouteBudget = serde_json::from_str(r#"{"time_ms": 10000}"#).unwrap();
        assert_eq!(timed, RouteBudget::new(Some(10_000), None).unwrap());
        // A limit that is not set is written out, as a null.
        assert_eq!(
            serde_json::to_value(timed).unwrap(),
            serde_json::json!({ "time_ms": 10_000, "iterations": null })
        );

        let both = RouteBudget::new(Some(250), Some(1_000_000)).unwrap();
        let json = serde_json::to_value(both).unwrap();
        assert_eq!(serde_json::from_value::<RouteBudget>(json).unwrap(), both);

        for unlimited in ["{}", r#"{"time_ms": null, "iterations": null}"#] {
            let error = serde_json::from_str::<RouteBudget>(unlimited).unwrap_err();
            assert!(error.to_string().contains("needs a limit"), "{unlimited}");
        }
        assert!(serde_json::from_str::<RouteBudget>(r#"{"time_ms": -1}"#).is_err());
    }

    #[test]
    fn a_password_hash_does_not_show_in_debug_output() {
        let user = User {
            id: UserId::generate(),
            email: Some("ada@example.com".to_owned()),
            telegram_id: None,
            role: Role::Client,
            created_at: Utc::now(),
        };
        let new = NewUser {
            email: "ada@example.com".to_owned(),
            password_hash: "$argon2id$secret".to_owned(),
            role: Role::Client,
        };
        let credentials = Credentials {
            user,
            password_hash: new.password_hash.clone(),
        };
        assert!(!format!("{new:?}").contains("secret"));
        assert!(!format!("{credentials:?}").contains("secret"));
        assert!(format!("{credentials:?}").contains("ada@example.com"));
    }

    #[test]
    fn an_order_travels_as_flat_json_with_a_nested_position() {
        let order = NewOrder {
            client_id: UserId::generate(),
            kind: OrderKind::Pickup,
            service_date: NaiveDate::from_ymd_opt(2026, 10, 12).unwrap(),
            location: LatLon::new(50.45, 30.52).unwrap(),
            address: None,
            demand: 2,
        };
        let json = serde_json::to_value(&order).unwrap();
        assert_eq!(json["kind"], "pickup");
        assert_eq!(json["service_date"], "2026-10-12");
        assert_eq!(json["location"]["lat"], 50.45);
        assert_eq!(serde_json::from_value::<NewOrder>(json).unwrap(), order);
    }
}
