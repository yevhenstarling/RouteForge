//! What the API checks in a request before it acts on it.
//!
//! The domain types refuse, as they are read, what can never make sense: a position
//! off the globe, a budget without a limit. What is left for here are the rules a
//! number or a text has to keep, and the limits that are the API's own settings.
//!
//! Every field that fails is reported, not just the first, so that a client can put
//! them all right at once.

use routeforge_domain::{
    DemoRequest, FieldError, Login, NewCourier, NewDepot, OrderRequest, RouteBudget, TelegramLogin,
};

use crate::config::Limits;
use crate::error::ApiError;

const PASSWORD_SHORTEST: usize = 8;
/// A password is hashed whole, and the work grows with its length.
const PASSWORD_LONGEST: usize = 128;
/// What the mail standards allow an address.
const EMAIL_LONGEST: usize = 254;
const NAME_LONGEST: usize = 100;
const ADDRESS_LONGEST: usize = 300;

/// A page of a list, if the request does not say.
pub(crate) const PAGE_DEFAULT: u32 = 100;
pub(crate) const PAGE_LARGEST: u32 = 1_000;

/// The demo orders are stored one by one, in one transaction.
pub(crate) const DEMO_MOST: u32 = 2_000;
pub(crate) const DEMO_RADIUS_DEFAULT_KM: f64 = 5.0;
/// Beyond this the flat map the solver clusters on is no longer a good one (D29).
pub(crate) const DEMO_RADIUS_LARGEST_KM: f64 = 30.0;

/// The fields of one request that were refused.
#[derive(Debug, Default)]
struct Refused(Vec<FieldError>);

impl Refused {
    /// Refuses `field` unless `holds`.
    fn unless(&mut self, holds: bool, field: &str, message: impl Into<String>) {
        if !holds {
            self.0.push(FieldError {
                field: field.to_owned(),
                message: message.into(),
            });
        }
    }

    fn into_result(self) -> Result<(), ApiError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(ApiError::invalid(self.0))
        }
    }
}

/// An address and a password to open an account with.
pub(crate) fn registration(login: &Login) -> Result<(), ApiError> {
    let mut refused = Refused::default();
    refused.unless(
        is_email(&login.email),
        "email",
        "must be an email address, without spaces",
    );
    let length = login.password.chars().count();
    refused.unless(
        length >= PASSWORD_SHORTEST && login.password.len() <= PASSWORD_LONGEST,
        "password",
        format!("must be {PASSWORD_SHORTEST} to {PASSWORD_LONGEST} characters long"),
    );
    refused.into_result()
}

/// Who the bot says it is talking to. Telegram counts its users from 1.
pub(crate) fn telegram(login: &TelegramLogin) -> Result<(), ApiError> {
    let mut refused = Refused::default();
    refused.unless(login.telegram_id >= 1, "telegram_id", "must be at least 1");
    refused.into_result()
}

/// Something with an `@` in it that an address could be. Whether mail arrives there is
/// not for the API to know.
fn is_email(text: &str) -> bool {
    let Some((name, domain)) = text.split_once('@') else {
        return false;
    };
    text.len() <= EMAIL_LONGEST
        && !name.is_empty()
        && !domain.is_empty()
        && !domain.contains('@')
        && !text
            .chars()
            .any(|letter| letter.is_whitespace() || letter.is_control())
}

/// A name a person is to recognize something by.
fn is_name(text: &str) -> bool {
    !text.trim().is_empty() && text.chars().count() <= NAME_LONGEST
}

fn name_rule() -> String {
    format!("must not be empty, and at most {NAME_LONGEST} characters long")
}

pub(crate) fn order(request: &OrderRequest) -> Result<(), ApiError> {
    let mut refused = Refused::default();
    refused.unless(request.demand >= 1, "demand", "must be at least 1");
    refused.unless(
        request
            .address
            .as_ref()
            .is_none_or(|address| address.chars().count() <= ADDRESS_LONGEST),
        "address",
        format!("must be at most {ADDRESS_LONGEST} characters long"),
    );
    refused.into_result()
}

pub(crate) fn courier(new: &NewCourier) -> Result<(), ApiError> {
    let mut refused = Refused::default();
    refused.unless(is_name(&new.name), "name", name_rule());
    refused.unless(new.capacity >= 1, "capacity", "must be at least 1");
    refused.into_result()
}

pub(crate) fn depot(new: &NewDepot) -> Result<(), ApiError> {
    let mut refused = Refused::default();
    refused.unless(is_name(&new.name), "name", name_rule());
    refused.into_result()
}

/// A budget a plan asks for, against what the API allows (D31).
pub(crate) fn budget(budget: RouteBudget, limits: &Limits) -> Result<(), ApiError> {
    let mut refused = Refused::default();
    refused.unless(
        budget
            .time_ms()
            .is_none_or(|time_ms| time_ms <= limits.max_budget_ms),
        "budget.time_ms",
        format!("must be at most {}", limits.max_budget_ms),
    );
    refused.unless(
        budget
            .iterations()
            .is_none_or(|moves| moves <= limits.max_budget_iterations),
        "budget.iterations",
        format!("must be at most {}", limits.max_budget_iterations),
    );
    refused.into_result()
}

pub(crate) fn demo(request: &DemoRequest) -> Result<(), ApiError> {
    let mut refused = Refused::default();
    refused.unless(
        (1..=DEMO_MOST).contains(&request.count),
        "count",
        format!("must be between 1 and {DEMO_MOST}"),
    );
    refused.unless(
        // A NaN is in no range.
        request
            .radius_km
            .is_none_or(|radius| radius > 0.0 && radius <= DEMO_RADIUS_LARGEST_KM),
        "radius_km",
        format!("must be more than 0 and at most {DEMO_RADIUS_LARGEST_KM}"),
    );
    refused.into_result()
}

/// How many items a page of a list is to have.
pub(crate) fn page(limit: Option<u32>) -> Result<u32, ApiError> {
    let limit = limit.unwrap_or(PAGE_DEFAULT);
    if (1..=PAGE_LARGEST).contains(&limit) {
        Ok(limit)
    } else {
        Err(ApiError::invalid_field(
            "limit",
            format!("must be between 1 and {PAGE_LARGEST}"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use routeforge_domain::{LatLon, OrderKind};

    use super::*;

    /// The names of the fields a request was refused for, in the order they were found.
    fn refused_fields(result: Result<(), ApiError>) -> Vec<String> {
        match result {
            Ok(()) => Vec::new(),
            Err(ApiError::Invalid { errors, .. }) => {
                errors.into_iter().map(|error| error.field).collect()
            }
            Err(other) => panic!("refused for another reason: {other:?}"),
        }
    }

    fn login(email: &str, password: &str) -> Login {
        Login {
            email: email.to_owned(),
            password: password.to_owned(),
        }
    }

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 12).unwrap()
    }

    #[test]
    fn an_account_needs_an_address_and_a_password_of_some_length() {
        for email in [
            "ada@example.com",
            "a@b",
            "ada.lovelace+work@mail.example.co.uk",
        ] {
            assert!(registration(&login(email, "12345678")).is_ok(), "{email}");
        }
        for email in [
            "",
            "ada",
            "@example.com",
            "ada@",
            "ada@@example.com",
            "ada @example.com",
            "ada@exa\nmple.com",
            &format!("{}@example.com", "a".repeat(250)),
        ] {
            assert_eq!(
                refused_fields(registration(&login(email, "12345678"))),
                ["email"],
                "{email:?}"
            );
        }

        assert!(registration(&login("ada@example.com", &"x".repeat(128))).is_ok());
        // Eight characters, whatever bytes they take.
        assert!(registration(&login("ada@example.com", "пароль12")).is_ok());
        for password in ["", "1234567", &"x".repeat(129)] {
            assert_eq!(
                refused_fields(registration(&login("ada@example.com", password))),
                ["password"]
            );
        }

        // Both at once are both reported.
        assert_eq!(
            refused_fields(registration(&login("ada", "short"))),
            ["email", "password"]
        );
    }

    #[test]
    fn a_telegram_user_has_a_positive_id() {
        let user = |telegram_id| TelegramLogin { telegram_id };
        assert!(telegram(&user(1)).is_ok());
        assert!(telegram(&user(i64::MAX)).is_ok());
        for id in [0, -1, i64::MIN] {
            assert_eq!(refused_fields(telegram(&user(id))), ["telegram_id"], "{id}");
        }
    }

    #[test]
    fn an_order_needs_a_demand_and_an_address_that_fits() {
        let request = |demand: u32, address: Option<String>| OrderRequest {
            kind: OrderKind::Delivery,
            service_date: date(),
            location: LatLon::new(50.45, 30.52).unwrap(),
            address,
            demand,
        };
        assert!(order(&request(1, None)).is_ok());
        assert!(order(&request(u32::MAX, Some("я".repeat(300)))).is_ok());
        assert_eq!(refused_fields(order(&request(0, None))), ["demand"]);
        assert_eq!(
            refused_fields(order(&request(0, Some("x".repeat(301))))),
            ["demand", "address"]
        );
    }

    #[test]
    fn a_courier_and_a_depot_need_a_name() {
        let van = |name: &str, capacity: u32| NewCourier {
            name: name.to_owned(),
            capacity,
        };
        assert!(courier(&van("Van 1", 1)).is_ok());
        assert_eq!(refused_fields(courier(&van("Van 1", 0))), ["capacity"]);
        assert_eq!(
            refused_fields(courier(&van("   ", 0))),
            ["name", "capacity"]
        );
        assert_eq!(refused_fields(courier(&van(&"x".repeat(101), 5))), ["name"]);

        let at = |name: &str| NewDepot {
            name: name.to_owned(),
            location: LatLon::new(50.45, 30.52).unwrap(),
        };
        assert!(depot(&at("Left bank")).is_ok());
        assert_eq!(refused_fields(depot(&at(""))), ["name"]);
    }

    #[test]
    fn a_budget_is_held_to_the_limits_of_the_api() {
        let limits = Limits {
            default_budget_ms: 1_000,
            max_budget_ms: 5_000,
            max_budget_iterations: 1_000_000,
        };
        let of = |time_ms, iterations| RouteBudget::new(time_ms, iterations).unwrap();

        for within in [
            of(Some(0), None),
            of(Some(5_000), None),
            of(None, Some(1_000_000)),
            of(Some(5_000), Some(1_000_000)),
        ] {
            assert!(budget(within, &limits).is_ok(), "{within:?}");
        }
        assert_eq!(
            refused_fields(budget(of(Some(5_001), None), &limits)),
            ["budget.time_ms"]
        );
        assert_eq!(
            refused_fields(budget(of(None, Some(1_000_001)), &limits)),
            ["budget.iterations"]
        );
        // Being within one limit does not excuse the other.
        assert_eq!(
            refused_fields(budget(of(Some(1), Some(u64::MAX)), &limits)),
            ["budget.iterations"]
        );
        assert_eq!(
            refused_fields(budget(of(Some(u32::MAX), Some(u64::MAX)), &limits)),
            ["budget.time_ms", "budget.iterations"]
        );
    }

    #[test]
    fn a_demonstration_has_a_size_and_a_reach() {
        let request = |count: u32, radius_km: Option<f64>| DemoRequest {
            service_date: date(),
            count,
            seed: 7,
            radius_km,
        };
        assert!(demo(&request(1, None)).is_ok());
        assert!(demo(&request(2_000, Some(30.0))).is_ok());
        assert!(demo(&request(300, Some(0.001))).is_ok());

        for count in [0, 2_001, u32::MAX] {
            assert_eq!(refused_fields(demo(&request(count, None))), ["count"]);
        }
        for radius in [0.0, -1.0, 30.001, f64::INFINITY, f64::NAN] {
            assert_eq!(
                refused_fields(demo(&request(10, Some(radius)))),
                ["radius_km"],
                "{radius}"
            );
        }
    }

    #[test]
    fn a_page_has_a_default_size_and_a_largest() {
        assert_eq!(page(None).unwrap(), 100);
        assert_eq!(page(Some(1)).unwrap(), 1);
        assert_eq!(page(Some(1_000)).unwrap(), 1_000);
        for limit in [0, 1_001, u32::MAX] {
            let error = page(Some(limit)).unwrap_err();
            assert!(
                matches!(&error, ApiError::Invalid { errors, .. } if errors[0].field == "limit"),
                "{error:?}"
            );
        }
    }
}
