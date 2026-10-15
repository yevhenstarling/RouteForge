//! Identifiers.
//!
//! Every table has its own id type, so an order id cannot be passed where a plan id is
//! expected. All of them are UUIDv7 made in the application (D11): the schema needs no
//! server-side generator, and ids sort by the time they were made.
//!
//! One identifier is not the system's own: an [`IdempotencyKey`], which whoever places
//! an order makes up, so that the order can be asked for again without being placed
//! again (D76).

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

/// What makes placing an order safe to repeat (D76).
///
/// Whoever places an order makes one up and sends it along. Asked again with the same
/// key, the API answers with the order the key placed before, and stores nothing. So a
/// request whose answer was lost can be sent again, and the order is placed once.
///
/// A key is 1 to 64 letters, digits, dots, underscores and hyphens: what a header, a
/// log and a column all take as it is. A UUID is one, and [`generate`](Self::generate)
/// makes such a one. It is a client's own: the same key from another client is another
/// key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(String);

/// A text that is no [`IdempotencyKey`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a key is 1 to 64 letters, digits, dots, underscores or hyphens")]
pub struct InvalidKey;

impl IdempotencyKey {
    /// The header a key travels in.
    pub const HEADER: &'static str = "Idempotency-Key";

    /// The longest a key may be. A UUID takes 36 characters.
    pub const LONGEST: usize = 64;

    pub fn new(text: &str) -> Result<Self, InvalidKey> {
        let well_formed = (1..=Self::LONGEST).contains(&text.len())
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        if well_formed {
            Ok(Self(text.to_owned()))
        } else {
            Err(InvalidKey)
        }
    }

    /// A new key, which no other request has: a UUIDv7.
    pub fn generate() -> Self {
        Self(Uuid::now_v7().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for IdempotencyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for IdempotencyKey {
    type Err = InvalidKey;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::new(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_short_and_of_characters_that_mean_nothing_to_a_header() {
        for good in [
            "a",
            "0",
            "press-1",
            "tg_4242.17",
            "0199e3a4-7c1e-7b52-9d0a-1f2e3d4c5b6a",
            &"k".repeat(IdempotencyKey::LONGEST),
        ] {
            let key = IdempotencyKey::new(good).unwrap();
            assert_eq!(key.as_str(), good);
            assert_eq!(key.to_string(), good);
            assert_eq!(good.parse::<IdempotencyKey>().unwrap(), key);
        }
        for bad in [
            "",
            " ",
            "two words",
            "line\nbreak",
            "quote\"",
            "semi;colon",
            "ключ",
            "a/b",
            &"k".repeat(IdempotencyKey::LONGEST + 1),
        ] {
            assert_eq!(IdempotencyKey::new(bad), Err(InvalidKey), "{bad:?}");
        }
    }

    #[test]
    fn a_key_that_is_made_is_a_key_and_no_other_is_like_it() {
        let keys: Vec<IdempotencyKey> = (0..1_000).map(|_| IdempotencyKey::generate()).collect();
        for key in &keys {
            assert_eq!(IdempotencyKey::new(key.as_str()).as_ref(), Ok(key));
            assert_eq!(key.as_str().parse::<Uuid>().unwrap().get_version_num(), 7);
        }
        let different: std::collections::HashSet<&IdempotencyKey> = keys.iter().collect();
        assert_eq!(different.len(), keys.len());
    }

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
