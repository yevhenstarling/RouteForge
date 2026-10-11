//! The API's settings, read from the environment.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, bail, ensure};

/// No span of time is set longer than a day, as in the solver.
const LONGEST_SECS: u64 = 24 * 60 * 60;

/// A secret shorter than the hash it keys adds nothing to it. HS256 hashes to 32 bytes.
const SHORTEST_SECRET: usize = 32;

/// How the API behaves. None of it is secret, so it can be logged: what must not be
/// is in [`Secrets`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Config {
    /// The address and port to listen on.
    pub(crate) bind: SocketAddr,
    /// How long a login is good for.
    pub(crate) token_ttl: Duration,
    pub(crate) limits: Limits,
    /// A worker that was seen within this span counts as alive. A worker marks itself
    /// as seen once per solver heartbeat (D27), so this is a few of those.
    pub(crate) worker_alive: Duration,
}

/// What a plan's budget for each of its routes may be (D31).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits {
    /// The budget of a plan that asks for none, in milliseconds.
    pub(crate) default_budget_ms: u32,
    /// The longest a plan may ask for.
    pub(crate) max_budget_ms: u32,
    /// The most candidate moves a plan may ask for.
    pub(crate) max_budget_iterations: u64,
}

impl Config {
    /// Reads the settings from the environment of the process.
    pub(crate) fn from_env() -> anyhow::Result<Self> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// Reads the settings from `var`, which gives the value of a variable by name.
    ///
    /// A variable that is missing or empty has its default.
    pub(crate) fn from_vars(var: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let number = |name: &str, default: u64| -> anyhow::Result<u64> {
            match set(&var, name) {
                None => Ok(default),
                Some(value) => value
                    .trim()
                    .parse()
                    .with_context(|| format!("{name} must be a whole number, got `{value}`")),
            }
        };
        let seconds = |name: &str, default: u64| -> anyhow::Result<Duration> {
            let seconds = number(name, default)?;
            ensure!(
                (1..=LONGEST_SECS).contains(&seconds),
                "{name} must be between 1 and {LONGEST_SECS} seconds, got {seconds}"
            );
            Ok(Duration::from_secs(seconds))
        };
        let millis = |name: &str, default: u64| -> anyhow::Result<u32> {
            let millis = number(name, default)?;
            match u32::try_from(millis) {
                Ok(millis) if millis > 0 => Ok(millis),
                _ => bail!(
                    "{name} must be between 1 and {} milliseconds, got {millis}",
                    u32::MAX
                ),
            }
        };

        let bind = match set(&var, "API_BIND") {
            None => SocketAddr::from(([127, 0, 0, 1], 8080)),
            Some(value) => value.trim().parse().with_context(|| {
                format!(
                    "API_BIND must be an address and a port, like 127.0.0.1:8080, got `{value}`"
                )
            })?,
        };
        let limits = Limits {
            default_budget_ms: millis("API_BUDGET_DEFAULT_MS", 10_000)?,
            max_budget_ms: millis("API_BUDGET_MAX_MS", 300_000)?,
            max_budget_iterations: number("API_BUDGET_MAX_ITERATIONS", 10_000_000_000)?,
        };
        ensure!(
            limits.default_budget_ms <= limits.max_budget_ms,
            "API_BUDGET_DEFAULT_MS ({}) must not be more than API_BUDGET_MAX_MS ({})",
            limits.default_budget_ms,
            limits.max_budget_ms
        );
        // The column that holds the moves of a plan is a signed 64-bit number.
        ensure!(
            (1..=i64::MAX.unsigned_abs()).contains(&limits.max_budget_iterations),
            "API_BUDGET_MAX_ITERATIONS must be between 1 and {}, got {}",
            i64::MAX,
            limits.max_budget_iterations
        );

        Ok(Self {
            bind,
            token_ttl: seconds("JWT_TTL_SECS", 3_600)?,
            limits,
            worker_alive: seconds("API_WORKER_ALIVE_SECS", 15)?,
        })
    }
}

/// What the API needs and must not show: where the database is, which holds the
/// password, the key that signs the tokens, and the first admin's password.
///
/// It has no `Debug`, so that none of it can reach a log.
pub(crate) struct Secrets {
    pub(crate) database_url: String,
    pub(crate) jwt_secret: String,
    /// The admin to make when the API starts, if one is asked for.
    pub(crate) admin: Option<AdminSeed>,
}

/// The account `ADMIN_EMAIL` and `ADMIN_PASSWORD` ask for.
pub(crate) struct AdminSeed {
    pub(crate) email: String,
    pub(crate) password: String,
}

impl Secrets {
    pub(crate) fn from_env() -> anyhow::Result<Self> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    pub(crate) fn from_vars(var: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let database_url = set(&var, "DATABASE_URL").context("DATABASE_URL is not set")?;
        let jwt_secret = set(&var, "JWT_SECRET").context("JWT_SECRET is not set")?;
        ensure!(
            jwt_secret.len() >= SHORTEST_SECRET,
            "JWT_SECRET must be at least {SHORTEST_SECRET} bytes long, got {}",
            jwt_secret.len()
        );
        let admin = match (set(&var, "ADMIN_EMAIL"), set(&var, "ADMIN_PASSWORD")) {
            (Some(email), Some(password)) => Some(AdminSeed {
                email: email.trim().to_owned(),
                password,
            }),
            (None, None) => None,
            (Some(_), None) => bail!("ADMIN_EMAIL is set and ADMIN_PASSWORD is not"),
            (None, Some(_)) => bail!("ADMIN_PASSWORD is set and ADMIN_EMAIL is not"),
        };
        Ok(Self {
            database_url,
            jwt_secret,
            admin,
        })
    }
}

/// The value of a variable, unless it is missing or empty. Docker Compose passes a
/// variable that is not set as an empty one.
fn set(var: &impl Fn(&str) -> Option<String>, name: &str) -> Option<String> {
    var(name).filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(known, _)| *known == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    fn config(set: &[(&str, &str)]) -> anyhow::Result<Config> {
        Config::from_vars(vars(set))
    }

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn nothing_set_gives_the_defaults() {
        assert_eq!(
            config(&[]).unwrap(),
            Config {
                bind: "127.0.0.1:8080".parse().unwrap(),
                token_ttl: Duration::from_secs(3_600),
                limits: Limits {
                    default_budget_ms: 10_000,
                    max_budget_ms: 300_000,
                    max_budget_iterations: 10_000_000_000,
                },
                worker_alive: Duration::from_secs(15),
            }
        );
    }

    #[test]
    fn every_setting_can_be_set() {
        let config = config(&[
            ("API_BIND", " 0.0.0.0:9000 "),
            ("JWT_TTL_SECS", "60"),
            ("API_BUDGET_DEFAULT_MS", "2000"),
            ("API_BUDGET_MAX_MS", "2000"),
            ("API_BUDGET_MAX_ITERATIONS", "5"),
            ("API_WORKER_ALIVE_SECS", "3"),
        ])
        .unwrap();
        assert_eq!(
            config,
            Config {
                bind: "0.0.0.0:9000".parse().unwrap(),
                token_ttl: Duration::from_secs(60),
                limits: Limits {
                    default_budget_ms: 2_000,
                    max_budget_ms: 2_000,
                    max_budget_iterations: 5,
                },
                worker_alive: Duration::from_secs(3),
            }
        );
    }

    #[test]
    fn an_empty_variable_has_its_default() {
        let config = config(&[("API_BIND", ""), ("JWT_TTL_SECS", "  ")]).unwrap();
        assert_eq!(config, self::config(&[]).unwrap());
    }

    #[test]
    fn values_that_make_no_sense_are_refused_by_name() {
        for (name, value) in [
            ("API_BIND", "localhost"),
            ("API_BIND", "8080"),
            ("JWT_TTL_SECS", "0"),
            ("JWT_TTL_SECS", "86401"),
            ("JWT_TTL_SECS", "an hour"),
            ("API_BUDGET_DEFAULT_MS", "0"),
            ("API_BUDGET_DEFAULT_MS", "300001"),
            ("API_BUDGET_MAX_MS", "9999"),
            ("API_BUDGET_MAX_MS", "4294967296"),
            ("API_BUDGET_MAX_ITERATIONS", "0"),
            ("API_BUDGET_MAX_ITERATIONS", "9223372036854775808"),
            ("API_BUDGET_MAX_ITERATIONS", "-1"),
            ("API_WORKER_ALIVE_SECS", "0"),
        ] {
            let error = config(&[(name, value)]).unwrap_err();
            assert!(error.to_string().contains(name), "{name}={value}: {error}");
        }
    }

    #[test]
    fn the_database_and_the_signing_key_are_required() {
        let secrets = Secrets::from_vars(vars(&[
            ("DATABASE_URL", "postgres://x"),
            ("JWT_SECRET", SECRET),
        ]))
        .unwrap();
        assert_eq!(secrets.database_url, "postgres://x");
        assert_eq!(secrets.jwt_secret, SECRET);
        assert!(secrets.admin.is_none());

        for (missing, set) in [
            ("DATABASE_URL", vec![("JWT_SECRET", SECRET)]),
            ("JWT_SECRET", vec![("DATABASE_URL", "postgres://x")]),
            (
                "JWT_SECRET",
                vec![("DATABASE_URL", "postgres://x"), ("JWT_SECRET", "")],
            ),
        ] {
            let Err(error) = Secrets::from_vars(vars(&set)) else {
                panic!("started without {missing}");
            };
            assert!(error.to_string().contains(missing), "{error}");
        }
    }

    #[test]
    fn a_signing_key_that_is_too_short_is_refused() {
        let short = &SECRET[..31];
        let Err(error) = Secrets::from_vars(vars(&[
            ("DATABASE_URL", "postgres://x"),
            ("JWT_SECRET", short),
        ])) else {
            panic!("took a key of 31 bytes");
        };
        assert!(error.to_string().contains("at least 32"), "{error}");
        // What was refused is not repeated, short as it is.
        assert!(!error.to_string().contains(short), "{error}");
    }

    #[test]
    fn the_first_admin_needs_both_an_address_and_a_password() {
        let base = [("DATABASE_URL", "postgres://x"), ("JWT_SECRET", SECRET)];
        let with = |extra: &[(&'static str, &'static str)]| {
            let mut all = base.to_vec();
            all.extend_from_slice(extra);
            Secrets::from_vars(vars(&all))
        };

        let secrets = with(&[
            ("ADMIN_EMAIL", " admin@example.com "),
            ("ADMIN_PASSWORD", "correct horse"),
        ])
        .unwrap();
        let admin = secrets.admin.unwrap();
        assert_eq!(
            (admin.email.as_str(), admin.password.as_str()),
            ("admin@example.com", "correct horse")
        );

        for (given, missing) in [
            ("ADMIN_EMAIL", "ADMIN_PASSWORD"),
            ("ADMIN_PASSWORD", "ADMIN_EMAIL"),
        ] {
            let Err(error) = with(&[(given, "admin@example.com")]) else {
                panic!("took {given} alone");
            };
            assert!(error.to_string().contains(missing), "{error}");
        }
    }
}
