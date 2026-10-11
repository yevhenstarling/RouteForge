//! The solver's settings, read from the environment.

use std::time::Duration;

use anyhow::{Context, bail, ensure};

/// No span of time is set longer than a day. That keeps the arithmetic on them far
/// from overflowing, and a longer one would be a mistake anyway.
const LONGEST_SECS: u64 = 24 * 60 * 60;

/// Workers in one process are a convenience for running without Docker. Scaling is by
/// instances, and each worker takes up to two database connections.
const MOST_WORKERS: u32 = 64;

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
        let config = Self {
            concurrency,
            lease: seconds("SOLVER_LEASE_SECS", 30)?,
            heartbeat: seconds("SOLVER_HEARTBEAT_SECS", 5)?,
            poll: seconds("SOLVER_POLL_SECS", 5)?,
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

    /// Database connections the process needs: one that listens, and two for each
    /// worker, since marking itself as seen can coincide with any of its other calls.
    pub(crate) fn connections(&self) -> u32 {
        1 + 2 * self.concurrency
    }
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
            }
        );
        assert_eq!(config.connections(), 3);
    }

    #[test]
    fn every_setting_can_be_set() {
        let config = from(&[
            ("SOLVER_CONCURRENCY", "3"),
            ("SOLVER_LEASE_SECS", "10"),
            ("SOLVER_HEARTBEAT_SECS", " 2 "),
            ("SOLVER_POLL_SECS", "1"),
        ])
        .unwrap();
        assert_eq!(
            config,
            Config {
                concurrency: 3,
                lease: Duration::from_secs(10),
                heartbeat: Duration::from_secs(2),
                poll: Duration::from_secs(1),
            }
        );
        assert_eq!(config.connections(), 7);
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
