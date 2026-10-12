//! The solver's settings, read from the environment.

use std::fmt;
use std::time::Duration;

use anyhow::{Context, bail, ensure};
use reqwest::Url;

/// No span of time is set longer than a day. That keeps the arithmetic on them far
/// from overflowing, and a longer one would be a mistake anyway.
const LONGEST_SECS: u64 = 24 * 60 * 60;

/// Workers in one process are a convenience for running without Docker. Scaling is by
/// instances, and each worker takes up to three database connections.
const MOST_WORKERS: u32 = 64;

/// How often the progress of a search may be announced, in milliseconds. More often
/// than twenty times a second nobody could tell, and less often than once a minute is
/// not progress any more.
const PROGRESS_MS: std::ops::RangeInclusive<u64> = 50..=60_000;

/// How the workers of a solver process behave. Every span is measured by whoever it is
/// handed to: the lease by the database (D6), the others by this process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Config {
    /// Workers in this process. Each takes one job at a time.
    pub(crate) concurrency: u32,
    /// How long a claim holds without a heartbeat. A worker that dies is replaced
    /// this long after its last one.
    pub(crate) lease: Duration,
    /// How often a running job extends its lease and stores its progress, which is
    /// also how soon it hears that its plan was cancelled. A worker marks itself as
    /// seen in the registry at the same pace.
    pub(crate) heartbeat: Duration,
    /// How often an idle worker looks at the queue without having been notified, give
    /// or take a quarter.
    pub(crate) poll: Duration,
    /// How often, at most, a route search under way announces how far it has got and
    /// with which route, to whoever watches its plan (D49).
    pub(crate) progress: Duration,
    /// The OSRM server to ask for road distances, if there is one. Without it the
    /// distances are great circles, which need nothing outside the process (D8, D66).
    pub(crate) roads: Option<Roads>,
}

/// An OSRM server and how long it is waited for.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Roads {
    /// Where the server is. It carries no password, so it can be logged.
    pub(crate) url: Url,
    /// How long the server is given to answer for one route. After that the route is
    /// searched for on great circles.
    pub(crate) within: Duration,
}

// Written by hand so that the address is logged as it is written, and not taken apart
// into the nine things an address is made of.
impl fmt::Debug for Roads {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Roads")
            .field("url", &self.url.as_str())
            .field("within", &self.within)
            .finish()
    }
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
            match var(name).filter(|value| !value.trim().is_empty()) {
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

        let concurrency = number("SOLVER_CONCURRENCY", 1)?;
        let Some(concurrency) = u32::try_from(concurrency)
            .ok()
            .filter(|workers| (1..=MOST_WORKERS).contains(workers))
        else {
            bail!("SOLVER_CONCURRENCY must be between 1 and {MOST_WORKERS}, got {concurrency}");
        };
        let progress = number("SOLVER_PROGRESS_MS", 250)?;
        ensure!(
            PROGRESS_MS.contains(&progress),
            "SOLVER_PROGRESS_MS must be between {} and {} milliseconds, got {progress}",
            PROGRESS_MS.start(),
            PROGRESS_MS.end()
        );
        // How long the server may take is read only where there is a server.
        let roads = match address(&var, "OSRM_URL")? {
            Some(url) => Some(Roads {
                url,
                within: seconds("OSRM_TIMEOUT_SECS", 20)?,
            }),
            None => None,
        };
        let config = Self {
            concurrency,
            lease: seconds("SOLVER_LEASE_SECS", 30)?,
            heartbeat: seconds("SOLVER_HEARTBEAT_SECS", 5)?,
            poll: seconds("SOLVER_POLL_SECS", 5)?,
            progress: Duration::from_millis(progress),
            roads,
        };
        // One heartbeat that does not get through must not cost the job.
        ensure!(
            config.lease >= 2 * config.heartbeat,
            "SOLVER_LEASE_SECS ({}) must be at least twice SOLVER_HEARTBEAT_SECS ({})",
            config.lease.as_secs(),
            config.heartbeat.as_secs()
        );
        Ok(config)
    }

    /// Database connections the process needs: one that listens, and three for each
    /// worker. While it searches for a route, its heartbeat, an announcement of its
    /// progress and marking itself as seen can all fall on the same moment.
    pub(crate) fn connections(&self) -> u32 {
        1 + 3 * self.concurrency
    }
}

/// The address a setting names, of something that is spoken to over HTTP. `None` if
/// the variable is missing or empty.
///
/// What was given is not repeated when it is refused. An address can carry a password,
/// and this ends in a log. For the same reason one that does carry a user or a
/// password is refused: the settings are logged when the solver starts.
fn address(var: &impl Fn(&str) -> Option<String>, name: &str) -> anyhow::Result<Option<Url>> {
    let Some(value) = var(name).filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let url = Url::parse(value.trim())
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https") && url.has_host())
        .with_context(|| format!("{name} must be an address like http://127.0.0.1:5000"))?;
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "{name} must not carry a user or a password"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "{name} must be an address, with nothing after its path"
    );
    Ok(Some(url))
}

/// Where the database is. Kept out of [`Config`], which is logged, because it holds
/// the password.
pub(crate) fn database_url() -> anyhow::Result<String> {
    std::env::var("DATABASE_URL").context("DATABASE_URL is not set")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from(vars: &[(&str, &str)]) -> anyhow::Result<Config> {
        Config::from_vars(|name| {
            vars.iter()
                .find(|(known, _)| *known == name)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    #[test]
    fn nothing_set_gives_the_defaults() {
        let config = from(&[]).unwrap();
        assert_eq!(
            config,
            Config {
                concurrency: 1,
                lease: Duration::from_secs(30),
                heartbeat: Duration::from_secs(5),
                poll: Duration::from_secs(5),
                progress: Duration::from_millis(250),
                // No routing service: great circles, which need nothing (D8).
                roads: None,
            }
        );
        assert_eq!(config.connections(), 4);
    }

    #[test]
    fn a_routing_service_is_an_address_and_a_time_to_wait() {
        let config = from(&[("OSRM_URL", " http://osrm:5000 ")]).unwrap();
        let roads = config.roads.unwrap();
        assert_eq!(roads.url.as_str(), "http://osrm:5000/");
        assert_eq!(roads.within, Duration::from_secs(20));
        // As it is logged when the solver starts.
        assert_eq!(
            format!("{roads:?}"),
            r#"Roads { url: "http://osrm:5000/", within: 20s }"#
        );

        let config = from(&[
            ("OSRM_URL", "http://127.0.0.1:5001/roads"),
            ("OSRM_TIMEOUT_SECS", "3"),
        ])
        .unwrap();
        let roads = config.roads.unwrap();
        assert_eq!(roads.url.as_str(), "http://127.0.0.1:5001/roads");
        assert_eq!(roads.within, Duration::from_secs(3));

        // Docker Compose passes a variable that is not set as an empty one.
        assert_eq!(from(&[("OSRM_URL", "")]).unwrap().roads, None);
        // How long to wait says nothing without a service to wait for, mistyped or not.
        assert_eq!(from(&[("OSRM_TIMEOUT_SECS", "soon")]).unwrap().roads, None);
    }

    #[test]
    fn an_address_that_is_none_is_refused_and_not_repeated() {
        for (value, what) in [
            ("osrm:5000", "an address like"),
            ("ftp://osrm:5000", "an address like"),
            ("http://", "an address like"),
            ("not an address", "an address like"),
            ("http://solver:hunter2@osrm:5000", "a user or a password"),
            ("http://solver@osrm:5000", "a user or a password"),
            ("http://osrm:5000/?profile=car", "nothing after its path"),
            ("http://osrm:5000/#table", "nothing after its path"),
        ] {
            let error = from(&[("OSRM_URL", value)]).unwrap_err().to_string();
            assert!(error.contains("OSRM_URL"), "{value}: {error}");
            assert!(error.contains(what), "{value}: {error}");
            assert!(!error.contains("hunter2"), "{error}");
        }
        for value in ["0", "86401", "a while"] {
            let set = [
                ("OSRM_URL", "http://osrm:5000"),
                ("OSRM_TIMEOUT_SECS", value),
            ];
            let error = from(&set).unwrap_err().to_string();
            assert!(error.contains("OSRM_TIMEOUT_SECS"), "{value}: {error}");
        }
    }

    #[test]
    fn every_setting_can_be_set() {
        let config = from(&[
            ("SOLVER_CONCURRENCY", "3"),
            ("SOLVER_LEASE_SECS", "10"),
            ("SOLVER_HEARTBEAT_SECS", " 2 "),
            ("SOLVER_POLL_SECS", "1"),
            ("SOLVER_PROGRESS_MS", "1000"),
        ])
        .unwrap();
        assert_eq!(
            config,
            Config {
                concurrency: 3,
                lease: Duration::from_secs(10),
                heartbeat: Duration::from_secs(2),
                poll: Duration::from_secs(1),
                progress: Duration::from_secs(1),
                roads: None,
            }
        );
        assert_eq!(config.connections(), 10);
    }

    /// Docker Compose passes a variable that is not set as an empty one.
    #[test]
    fn an_empty_variable_has_its_default() {
        let config = from(&[("SOLVER_LEASE_SECS", ""), ("SOLVER_CONCURRENCY", "  ")]).unwrap();
        assert_eq!(config, from(&[]).unwrap());
    }

    #[test]
    fn values_that_make_no_sense_are_refused_by_name() {
        for (name, value) in [
            ("SOLVER_CONCURRENCY", "0"),
            ("SOLVER_CONCURRENCY", "65"),
            ("SOLVER_CONCURRENCY", "many"),
            ("SOLVER_LEASE_SECS", "-1"),
            ("SOLVER_LEASE_SECS", "86401"),
            ("SOLVER_HEARTBEAT_SECS", "0"),
            ("SOLVER_HEARTBEAT_SECS", "2.5"),
            ("SOLVER_POLL_SECS", "0"),
            ("SOLVER_POLL_SECS", "18446744073709551616"),
            ("SOLVER_PROGRESS_MS", "0"),
            ("SOLVER_PROGRESS_MS", "49"),
            ("SOLVER_PROGRESS_MS", "60001"),
            ("SOLVER_PROGRESS_MS", "often"),
        ] {
            let error = from(&[(name, value)]).unwrap_err();
            assert!(error.to_string().contains(name), "{name}={value}: {error}");
        }
    }

    #[test]
    fn the_heartbeat_must_come_well_inside_the_lease() {
        let error =
            from(&[("SOLVER_LEASE_SECS", "9"), ("SOLVER_HEARTBEAT_SECS", "5")]).unwrap_err();
        assert!(error.to_string().contains("at least twice"), "{error}");

        from(&[("SOLVER_LEASE_SECS", "10"), ("SOLVER_HEARTBEAT_SECS", "5")]).unwrap();
    }
}
