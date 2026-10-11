//! Identifiers.
//!
//! Every table has its own id type, so an order id cannot be passed where a plan id is
//! expected. All of them are UUIDv7 made in the application (D11): the schema needs no
//! server-side generator, and ids sort by the time they were made.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[cfg_attr(feature = "openapi", derive(::utoipa::ToSchema))]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// A new id. It sorts after every id this process made before it.
            pub fn generate() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                text.parse().map(Self)
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Self {
                id.0
            }
        }
    };
}

id_type!(
    /// A client, an operator or an admin.
    UserId
);
id_type!(
    /// One version of the depot's name and position.
    DepotId
);
id_type!(CourierId);
id_type!(OrderId);
id_type!(
    /// One planning run for a service date.
    PlanId
);
id_type!(
    /// One courier's route in a plan.
    RouteId
);
id_type!(
    /// A unit of work on the queue.
    JobId
);
id_type!(
    /// One running solver process. It picks a new id every time it starts.
    WorkerId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_version_7_and_sort_by_creation() {
        let ids: Vec<OrderId> = (0..1_000).map(|_| OrderId::generate()).collect();
        assert!(ids.iter().all(|id| id.0.get_version_num() == 7));
        assert!(ids.is_sorted_by(|earlier, later| earlier < later));
    }

    #[test]
    fn an_id_reads_and_writes_as_a_plain_uuid() {
        let id = PlanId::generate();
        assert_eq!(id.to_string().parse::<PlanId>().unwrap(), id);
        assert_eq!(
            serde_json::to_string(&id).unwrap(),
            format!("\"{}\"", id.0.as_hyphenated())
        );
        assert_eq!(
            serde_json::from_str::<PlanId>(&format!("\"{id}\"")).unwrap(),
            id
        );
        assert_eq!(PlanId::from(Uuid::from(id)), id);
    }

    #[test]
    fn text_that_is_no_uuid_is_refused() {
        assert!("not-a-uuid".parse::<JobId>().is_err());
    }
}
