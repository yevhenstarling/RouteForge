//! What the three services have in common beside their types: how they log, how they
//! hear that they are to stop, and how long they wait before they try something again.
//!
//! The API, the solver and the bot each had a copy of this until Phase 12 (D81). It
//! needs neither the database nor the OpenAPI schemas, so the bot takes it without
//! either (D3).

use std::io::IsTerminal;
use std::time::Duration;

use tracing::warn;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::ParseError;

/// What is logged when `RUST_LOG` says nothing that can be used.
const EVERYTHING_FROM: &str = "info";

/// Logs to standard output: lines for people by default, JSON with `LOG_FORMAT=json`.
/// Either way an event carries the fields of the spans it happened in, which is where
/// the ids of a request, a worker or a job are (D33, D42).
///
/// `RUST_LOG` says what to log, in the syntax of `tracing`'s `EnvFilter`. Unless it is
/// set, everything from `info` up is logged. An empty one is taken as not set: it would
/// mean errors only. One that cannot be read is said to be so, once there is a log to
/// say it in, and `info` is used in its place.
pub fn init_logging() {
    let asked = std::env::var("RUST_LOG").ok();
    let (filter, unreadable) = filter(asked.as_deref());
    let json = std::env::var("LOG_FORMAT").is_ok_and(|format| format.eq_ignore_ascii_case("json"));
    let logging = tracing_subscriber::fmt()
        .with_env_filter(filter)
        // Colours for a person at a terminal. In a container's log they are in the
        // way of searching for an id.
        .with_ansi(std::io::stdout().is_terminal());
    if json {
        logging.json().init();
    } else {
        logging.init();
    }
    if let Some(error) = unreadable {
        warn!(
            rust_log = asked.as_deref().unwrap_or_default(),
            %error,
            "RUST_LOG cannot be read, so everything from {EVERYTHING_FROM} up is logged"
        );
    }
}

/// The filter `RUST_LOG` asks for, and why it could not be had if it could not.
fn filter(asked: Option<&str>) -> (EnvFilter, Option<ParseError>) {
    match asked.filter(|directives| !directives.trim().is_empty()) {
        None => (EnvFilter::new(EVERYTHING_FROM), None),
        Some(directives) => match EnvFilter::try_new(directives) {
            Ok(filter) => (filter, None),
            Err(error) => (EnvFilter::new(EVERYTHING_FROM), Some(error)),
        },
    }
}

/// Returns when the process is asked to stop: Ctrl-C anywhere, and SIGTERM where there
/// is one, which is what `docker stop` sends.
///
/// In a container a service is the first process, and the first process is not ended
/// by a signal it does not listen for. Without this, stopping the stack would wait out
/// the ten seconds Docker allows and then kill (D35, D51, D62).
pub async fn told_to_stop() {
    let interrupted = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            // Without a handler only a kill ends the process, and that needs no help.
            warn!(%error, "cannot hear Ctrl-C");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        match signal(SignalKind::terminate()) {
            Ok(mut terminated) => {
                tokio::select! {
                    () = interrupted => {}
                    _ = terminated.recv() => {}
                }
            }
            Err(error) => {
                warn!(%error, "cannot hear SIGTERM");
                interrupted.await;
            }
        }
    }
    #[cfg(not(unix))]
    interrupted.await;
}

/// How long to wait before trying again after a failure: `from` at first, then twice as
/// long each time, up to `up_to`. A success starts it from `from` again.
///
/// Every service waits like this for what it cannot reach: the solver for its database
/// (D37), the API's listener for its connection (D50), the bot for Telegram (D62).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    from: Duration,
    up_to: Duration,
    next: Duration,
}

impl Backoff {
    /// What the services wait unless a test says otherwise: a second at first, and
    /// half a minute at the most.
    pub const STANDARD: Self = Self::new(Duration::from_secs(1), Duration::from_secs(30));

    pub const fn new(from: Duration, up_to: Duration) -> Self {
        Self {
            from,
            up_to,
            next: from,
        }
    }

    /// The pause before the next try. The one after it is twice as long.
    pub fn pause(&mut self) -> Duration {
        let pause = self.next;
        self.next = self.next.saturating_mul(2).min(self.up_to);
        pause
    }

    /// After a success: the next failure waits as little as the first did.
    pub fn reset(&mut self) {
        self.next = self.from;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_log_says_what_is_logged_and_info_stands_in_for_nothing_usable() {
        let asked = "debug,sqlx=warn";
        let (debug, unreadable) = filter(Some(asked));
        // The filter writes its directives in an order of its own.
        assert_eq!(debug.to_string(), EnvFilter::new(asked).to_string());
        assert!(unreadable.is_none());

        for nothing in [None, Some(""), Some("  ")] {
            let (filter, unreadable) = filter(nothing);
            assert_eq!(filter.to_string(), EVERYTHING_FROM, "{nothing:?}");
            assert!(unreadable.is_none(), "{nothing:?}");
        }

        // A typo is not taken for a filter that lets nothing through: it is said, and
        // what is logged is what would be without it.
        let (filter, unreadable) = filter(Some("debug,sqlx=lound"));
        assert_eq!(filter.to_string(), EVERYTHING_FROM);
        assert!(unreadable.is_some());
    }

    #[test]
    fn a_pause_doubles_up_to_its_limit_and_starts_again_after_a_success() {
        let mut retry = Backoff::new(Duration::from_secs(1), Duration::from_secs(5));
        let pauses: Vec<u64> = (0..5).map(|_| retry.pause().as_secs()).collect();
        assert_eq!(pauses, [1, 2, 4, 5, 5]);

        retry.reset();
        assert_eq!(retry.pause(), Duration::from_secs(1));
        assert_eq!(retry.pause(), Duration::from_secs(2));

        // However long it has gone on, a pause never overflows.
        let mut long = Backoff::new(Duration::MAX, Duration::MAX);
        assert_eq!(long.pause(), Duration::MAX);
        assert_eq!(long.pause(), Duration::MAX);
    }

    #[test]
    fn the_services_wait_a_second_at_first_and_half_a_minute_at_most() {
        let mut retry = Backoff::STANDARD;
        let pauses: Vec<u64> = (0..7).map(|_| retry.pause().as_secs()).collect();
        assert_eq!(pauses, [1, 2, 4, 8, 16, 30, 30]);
    }
}
