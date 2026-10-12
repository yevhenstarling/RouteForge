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
    /// How long a token is good for that the bot was given for a Telegram user (D60).
    pub(crate) bot_token_ttl: Duration,
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
            bot_token_ttl: seconds("BOT_JWT_TTL_SECS", 60)?,
            limits,
            worker_alive: seconds("API_WORKER_ALIVE_SECS", 15)?,
        })
    }
}

/// What the API needs and must not show: where the database is, which holds the
/// password, the key that signs the tokens, the first admin's password, and the key
/// the bot shows.
///
/// It has no `Debug`, so that none of it can reach a log.
pub(crate) struct Secrets {
    pub(crate) database_url: String,
    pub(crate) jwt_secret: String,
    /// The admin to make when the API starts, if one is asked for.
    pub(crate) admin: Option<AdminSeed>,
    /// The key the bot shows to be given a Telegram user's token. Without one the API
    /// takes no bot (D60).
    pub(crate) bot_key: Option<String>,
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
        let bot_key = set(&var, "BOT_SERVICE_KEY");
        if let Some(key) = &bot_key {
            // Whoever has it can be given a token for any account the bot reaches. It
            // is held to the length the signing key is held to.
            ensure!(
                key.len() >= SHORTEST_SECRET,
                "BOT_SERVICE_KEY must be at least {SHORTEST_SECRET} bytes long, got {}",
                key.len()
            );
        }
        Ok(Self {
            database_url,
            jwt_secret,
            admin,
            bot_key,
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
    use routeforge_domain::Login;

    use super::*;
    use crate::validate;

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
                bot_token_ttl: Duration::from_secs(60),
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
            ("BOT_JWT_TTL_SECS", "5"),
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
                bot_token_ttl: Duration::from_secs(5),
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
            ("BOT_JWT_TTL_SECS", "0"),
            ("BOT_JWT_TTL_SECS", "a minute"),
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
        assert!(secrets.bot_key.is_none());

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

    /// The API takes a bot only when it has a key for one, and a key worth the name.
    #[test]
    fn the_bots_key_is_optional_and_has_a_least_length() {
        let base = [("DATABASE_URL", "postgres://x"), ("JWT_SECRET", SECRET)];
        let with = |key: &'static str| {
            let mut all = base.to_vec();
            all.push(("BOT_SERVICE_KEY", key));
            Secrets::from_vars(vars(&all))
        };

        let key = "a key the bot shows, and long enough";
        assert_eq!(with(key).unwrap().bot_key.as_deref(), Some(key));
        // Docker Compose passes a variable that is not set as an empty one.
        assert!(with("").unwrap().bot_key.is_none());
        assert!(with("   ").unwrap().bot_key.is_none());

        let short = &key[..31];
        let Err(error) = with(short) else {
            panic!("took a key of 31 bytes");
        };
        assert!(error.to_string().contains("BOT_SERVICE_KEY"), "{error}");
        assert!(error.to_string().contains("at least 32"), "{error}");
        assert!(!error.to_string().contains(short), "{error}");
    }

    /// The lines of one service in `docker-compose.yml`, as they are on a clone that
    /// has no `.env`: every `${VARIABLE:-fallback}` replaced by its fallback.
    fn service_of_the_stack(name: &str) -> Vec<String> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docker-compose.yml");
        let compose = std::fs::read_to_string(path).unwrap();
        let opening = format!("  {name}:");
        compose
            .lines()
            .skip_while(|line| *line != opening)
            .skip(1)
            // The service ends where a line is indented as little as its own name.
            .take_while(|line| line.starts_with("    ") || line.trim().is_empty())
            .map(|line| fallbacks(line.trim()))
            .collect()
    }

    /// `text` with every `${VARIABLE:-fallback}` replaced by its fallback. A variable
    /// without one would be empty on a fresh clone, which is what must not be.
    fn fallbacks(text: &str) -> String {
        let mut resolved = String::new();
        let mut rest = text;
        while let Some((before, variable)) = rest.split_once("${") {
            let (variable, after) = variable.split_once('}').expect("a `${` is closed");
            let (_, fallback) = variable
                .split_once(":-")
                .unwrap_or_else(|| panic!("${{{variable}}} has nothing to fall back on"));
            resolved.push_str(before);
            resolved.push_str(fallback);
            rest = after;
        }
        resolved + rest
    }

    /// What a service is given for a variable, on a clone that has no `.env`.
    fn given(service: &[String], name: &str) -> Option<String> {
        service.iter().find_map(|line| {
            let (key, value) = line.split_once(": ")?;
            (key == name).then(|| value.to_owned())
        })
    }

    /// A fresh clone has no `.env`, and `docker compose up` is to be all it needs
    /// (D54). So what the compose file falls back to has to be settings the API
    /// starts with.
    #[test]
    fn the_stack_starts_on_what_the_compose_file_falls_back_to() {
        let service = service_of_the_stack("api");
        let given = |name: &str| given(&service, name);

        let secrets = Secrets::from_vars(given).unwrap();
        // A bot that is given a token has an API that takes it, with nothing else set.
        assert!(secrets.bot_key.is_some(), "{service:#?}");
        // On the compose network, where the database is `db`.
        assert!(
            secrets.database_url.ends_with("@db:5432/routeforge"),
            "{}",
            secrets.database_url
        );
        let admin = secrets.admin.expect("an admin to log in as");
        let login = Login {
            email: admin.email,
            password: admin.password,
        };
        validate::registration(&login).expect("an account the API makes when it starts");

        let config = Config::from_vars(given).unwrap();
        // Every address of the container, on the port that is published, which is
        // open to this machine alone.
        assert_eq!(config.bind, "0.0.0.0:8080".parse().unwrap());
        let published = "- \"127.0.0.1:8080:8080\"".to_owned();
        assert!(service.contains(&published), "{service:#?}");
    }
}
