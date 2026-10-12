//! What the HTTP API takes and gives that is not one of the things the system keeps.
//!
//! The types in `model` are those things and what creating one needs. These are the
//! bodies around them: a login, the request for an order or for a plan, the state of
//! the queue, and the shape of an error. The API and the Telegram bot share them, each
//! reading what the other writes.

use std::fmt;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::geo::LatLon;
use crate::ids::UserId;
use crate::job::{QueueDepth, WorkerInfo};
use crate::model::{NewOrder, OrderKind, Role, RouteBudget, User};

/// An address and a password, to register with or to log in with.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Login {
    pub email: String,
    pub password: String,
}

// Written by hand so that a password cannot reach a log through `{:?}`.
impl fmt::Debug for Login {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Login")
            .field("email", &self.email)
            .finish_non_exhaustive()
    }
}

/// Who the bot is talking to, as Telegram knows them. The bot sends it with its key,
/// and is given a `Session` for that user's account (D60).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TelegramLogin {
    /// The id Telegram gives the user. At least 1.
    #[cfg_attr(feature = "openapi", schema(minimum = 1))]
    pub telegram_id: i64,
}

/// What a login gives: the token that says who is asking, and for how long it does.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Session {
    /// Sent with every later request, as `Authorization: Bearer <token>`.
    pub token: String,
    /// Seconds until the token is no longer accepted.
    pub expires_in: u64,
    pub user: User,
}

// Whoever holds the token is the user, so it is kept out of logs like a password.
impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("expires_in", &self.expires_in)
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

/// What placing an order needs. The client is whoever asks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct OrderRequest {
    pub kind: OrderKind,
    /// The day the courier is to call.
    pub service_date: NaiveDate,
    pub location: LatLon,
    /// What to show a person. Routing uses `location` alone.
    pub address: Option<String>,
    /// How much of a courier's capacity the order takes. At least 1.
    #[cfg_attr(feature = "openapi", schema(minimum = 1))]
    pub demand: u32,
}

impl OrderRequest {
    /// The order this asks for, placed by `client_id`.
    pub fn placed_by(self, client_id: UserId) -> NewOrder {
        NewOrder {
            client_id,
            kind: self.kind,
            service_date: self.service_date,
            location: self.location,
            address: self.address,
            demand: self.demand,
        }
    }
}

/// What asking for a plan needs. The depot is the current one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PlanRequest {
    /// The day whose pending orders are to be planned.
    pub service_date: NaiveDate,
    /// What every random choice in the plan's routes derives from (D5). One is picked
    /// if none is given, and the plan says which.
    pub seed: Option<u32>,
    /// How long each route may be searched for (D31). There is a default, and a limit.
    pub budget: Option<RouteBudget>,
}

/// What a user is to be allowed from now on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RoleChange {
    pub role: Role,
}

/// Whether a courier is to get routes from now on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CourierChange {
    pub active: bool,
}

/// What generating orders for a demonstration needs.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DemoRequest {
    /// The day the generated orders are for.
    pub service_date: NaiveDate,
    /// How many orders to generate.
    #[cfg_attr(feature = "openapi", schema(minimum = 1))]
    pub count: u32,
    /// The same seed gives the same orders round the same depot.
    pub seed: u32,
    /// How far from the depot an order may lie, in kilometres. 5 if not given.
    pub radius_km: Option<f64>,
}

/// How many orders a demonstration got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DemoCreated {
    pub created: u32,
}

/// The queue and the workers that drain it, as they are now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SystemStats {
    pub queue: QueueDepth,
    /// Workers that were seen lately.
    pub workers_alive: u32,
    /// Those of them that hold a job.
    pub workers_busy: u32,
    /// The workers that were seen lately, in the order they started.
    pub workers: Vec<WorkerInfo>,
}

/// What an `orders` event of the stats stream says: an order of this day was placed or
/// withdrawn, so whoever shows the day reads its orders again (D63).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct OrdersChanged {
    pub service_date: NaiveDate,
}

/// Why a request was refused, in the shape of RFC 9457 (`application/problem+json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Problem {
    /// Always `about:blank`: the status code says what kind of problem it is.
    #[serde(rename = "type")]
    pub kind: String,
    /// The status code in words.
    pub title: String,
    pub status: u16,
    /// What went wrong this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The fields of the request that were refused, if that is what went wrong.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<FieldError>,
}

/// One field of a request, and what is wrong with it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use super::*;

    #[test]
    fn a_password_and_a_token_do_not_show_in_debug_output() {
        let login = Login {
            email: "ada@example.com".to_owned(),
            password: "correct horse".to_owned(),
        };
        assert!(!format!("{login:?}").contains("horse"));
        assert!(format!("{login:?}").contains("ada@example.com"));

        let session = Session {
            token: "header.claims.signature".to_owned(),
            expires_in: 3_600,
            user: User {
                id: UserId::generate(),
                email: Some(login.email),
                telegram_id: None,
                role: Role::Client,
                created_at: Utc::now(),
            },
        };
        assert!(!format!("{session:?}").contains("signature"));
        assert!(format!("{session:?}").contains("ada@example.com"));
    }

    #[test]
    fn a_request_for_an_order_becomes_the_order_of_whoever_asks() {
        let request: OrderRequest = serde_json::from_value(json!({
            "kind": "pickup",
            "service_date": "2026-10-12",
            "location": { "lat": 50.45, "lon": 30.52 },
            "demand": 2,
        }))
        .unwrap();
        assert_eq!(request.address, None);

        let client = UserId::generate();
        let order = request.clone().placed_by(client);
        assert_eq!(order.client_id, client);
        assert_eq!(
            (order.kind, order.service_date, order.location, order.demand),
            (OrderKind::Pickup, request.service_date, request.location, 2)
        );
    }

    #[test]
    fn a_request_for_a_plan_needs_only_its_date() {
        let request: PlanRequest =
            serde_json::from_value(json!({ "service_date": "2026-10-12" })).unwrap();
        assert_eq!((request.seed, request.budget), (None, None));

        let request: PlanRequest = serde_json::from_value(json!({
            "service_date": "2026-10-12",
            "seed": 7,
            "budget": { "iterations": 1_000_000 },
        }))
        .unwrap();
        assert_eq!(request.seed, Some(7));
        assert_eq!(
            request.budget,
            Some(RouteBudget::new(None, Some(1_000_000)).unwrap())
        );

        // A budget that is given has to have a limit, here as everywhere.
        let unlimited = json!({ "service_date": "2026-10-12", "budget": {} });
        assert!(serde_json::from_value::<PlanRequest>(unlimited).is_err());
    }

    #[test]
    fn a_problem_says_only_what_it_has_to_say() {
        let plain = Problem {
            kind: "about:blank".to_owned(),
            title: "Not Found".to_owned(),
            status: 404,
            detail: None,
            errors: Vec::new(),
        };
        let json = serde_json::to_value(&plain).unwrap();
        assert_eq!(
            json,
            json!({ "type": "about:blank", "title": "Not Found", "status": 404 })
        );
        assert_eq!(serde_json::from_value::<Problem>(json).unwrap(), plain);

        let refused = Problem {
            kind: "about:blank".to_owned(),
            title: "Unprocessable Entity".to_owned(),
            status: 422,
            detail: Some("the request has a field that is not acceptable".to_owned()),
            errors: vec![FieldError {
                field: "demand".to_owned(),
                message: "must be at least 1".to_owned(),
            }],
        };
        let json = serde_json::to_value(&refused).unwrap();
        assert_eq!(json["errors"][0]["field"], "demand");
        assert_eq!(serde_json::from_value::<Problem>(json).unwrap(), refused);
    }
}
