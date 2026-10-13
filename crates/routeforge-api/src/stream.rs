//! Event streams: what the endpoints that answer for as long as they are listened to
//! have in common (D50).
//!
//! A stream is `text/event-stream`, written here by hand. It opens with what there is
//! to show and then sends what changes:
//!
//! - It reads what it shows again when the hub passes on a reason to
//!   ([`Heard::Changed`]), and every so often without one, since a notification is a
//!   hint and can be missed (D26). It sends only what differs from what it last sent.
//! - What is news by itself, the progress of a search, it sends on as it comes
//!   ([`Heard::Frame`]). One thing is both, news and a reason to read
//!   ([`Heard::ChangedWith`]): that a plan has changed, to the stream of the queue.
//! - A comment goes out now and then, so that nothing between the API and the reader
//!   takes the silence of a quiet plan for a dead connection.
//!
//! A stream ends when whoever reads it has gone, when the token it was opened with
//! expires, when the API is told to stop, and when a read fails. In the last three it
//! simply ends, and the reader, who can open it again, is answered then.
//!
//! A stream is a value that its response owns. No task stands behind it, so there is
//! nothing to stop and nothing that could be left running when the response is
//! dropped: the rule of D34, kept here as well.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::Duration;

use actix_web::http::header;
use actix_web::web::{self, Bytes};
use actix_web::{HttpResponse, HttpResponseBuilder};
use futures_util::stream;
use serde::Serialize;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, watch};
use tokio::time::{Instant, Interval, MissedTickBehavior};
use tracing::{Span, info, warn};

use crate::app::AppState;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::events::Signal;

/// The media type of an event stream.
pub(crate) const EVENT_STREAM: &str = "text/event-stream";

/// A line that starts with a colon is a comment, which a reader passes over.
const KEEP_ALIVE: &[u8] = b": keep-alive\n\n";

/// How the streams pace themselves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pace {
    /// The least time between two reads by one stream. A burst of changes, such as a
    /// worker claiming one job as it completes another, is then read once.
    pub(crate) settle: Duration,
    /// How often the stream of a plan reads without having been given a reason.
    pub(crate) plan_every: Duration,
    /// The same for the stream of the queue and its workers. It is short, because a
    /// worker comes, and falls silent, without a word to anybody (D27).
    pub(crate) stats_every: Duration,
    /// The same for the stream of somebody's own orders (D77). As for a plan: all
    /// that changes an order is announced, and this is for the word that was lost.
    pub(crate) orders_every: Duration,
    /// How often a comment is sent.
    pub(crate) keep_alive: Duration,
}

impl Default for Pace {
    fn default() -> Self {
        Self {
            settle: Duration::from_millis(200),
            plan_every: Duration::from_secs(10),
            stats_every: Duration::from_secs(2),
            orders_every: Duration::from_secs(10),
            keep_alive: Duration::from_secs(15),
        }
    }
}

/// What a signal from the hub is to one stream.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Heard {
    /// Nothing: it is about something the stream does not show.
    Nothing,
    /// A reason to read again.
    Changed,
    /// An event to send on as it is.
    Frame(Bytes),
    /// An event to send on as it is, and a reason to read again as well.
    ChangedWith(Bytes),
}

/// What one kind of stream shows.
pub(crate) trait Watch: 'static {
    /// What a signal from the hub is to this stream.
    fn hears(&self, signal: &Signal) -> Heard;

    /// Reads what the stream shows, and gives the events for whatever differs from
    /// what was last sent. All of it, the first time.
    async fn read(&mut self, state: &AppState) -> Result<Vec<Bytes>, ApiError>;

    /// How often to read without having been given a reason.
    fn unprompted(&self, pace: &Pace) -> Duration;
}

/// One event: its name, and its data as JSON on a line of its own.
pub(crate) fn frame(event: &str, data: &impl Serialize) -> Result<Bytes, ApiError> {
    // serde_json breaks no line by itself, and writes a line break inside a text as
    // `\n`. So the data is one line, as an event needs it to be.
    let data = serde_json::to_string(data).map_err(ApiError::internal)?;
    Ok(Bytes::from(format!("event: {event}\ndata: {data}\n\n")))
}

/// Opens a stream of what `watch` shows, for a caller who has passed the gate.
///
/// # Errors
///
/// What the first read fails with. Nothing has been sent by then, so the caller is
/// refused as by any other endpoint: a plan that does not exist is a `404`.
pub(crate) async fn open<W: Watch>(
    state: web::Data<AppState>,
    user: AuthUser,
    mut watch: W,
) -> Result<HttpResponse, ApiError> {
    // Subscribed before the first read. What changes after that read is then waiting
    // in the receiver when the stream comes to look.
    let subscription = state.events.subscribe();
    let first = watch.read(&state).await?;

    let pace = state.pace;
    let now = Instant::now();
    let mut keep_alive = tokio::time::interval_at(now + pace.keep_alive, pace.keep_alive);
    keep_alive.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // A token is accepted through the second it expires in, and refused from the next.
    // The stream ends then and not a moment sooner, so that whoever opens it again is
    // told to log in, and is not let in for the rest of a second.
    let seconds_left = user
        .expires_at
        .saturating_sub(jsonwebtoken::get_current_timestamp())
        .saturating_add(1);
    let feed = Feed {
        read_at: now + watch.unprompted(&pace),
        last_read: now,
        // A token that never expires does not exist. Should one claim to be good for
        // longer than the clock can count, the stream is simply not ended by it.
        expires: now.checked_add(Duration::from_secs(seconds_left)),
        state,
        signals: subscription.signals,
        closed: subscription.closed,
        watch,
        ready: first.into(),
        keep_alive,
        ending: Ending::begin(),
    };

    let events = stream::unfold(feed, |mut feed| async move {
        let frame = feed.next().await?;
        Some((Ok::<_, Infallible>(frame), feed))
    });
    Ok(response().streaming(events))
}

fn response() -> HttpResponseBuilder {
    let mut response = HttpResponse::Ok();
    response
        .content_type(EVENT_STREAM)
        .insert_header((header::CACHE_CONTROL, "no-cache"))
        // For a proxy that would otherwise collect the events before it passes them on.
        .insert_header(("X-Accel-Buffering", "no"));
    response
}

/// A stream between two things it sends.
struct Feed<W> {
    state: web::Data<AppState>,
    signals: broadcast::Receiver<Signal>,
    closed: watch::Receiver<bool>,
    watch: W,
    /// Events that are ready to be sent, oldest first.
    ready: VecDeque<Bytes>,
    /// When to read next: the unprompted read, or sooner once there is a reason.
    read_at: Instant,
    last_read: Instant,
    keep_alive: Interval,
    /// When the token the stream was opened with stops being accepted.
    expires: Option<Instant>,
    ending: Ending,
}

impl<W: Watch> Feed<W> {
    /// The next thing to send. `None` ends the stream.
    async fn next(&mut self) -> Option<Bytes> {
        loop {
            if let Some(frame) = self.ready.pop_front() {
                return Some(frame);
            }
            tokio::select! {
                () = told_to_close(&mut self.closed) => return self.end("the API is stopping"),
                () = at(self.expires) => return self.end("the token it was opened with has expired"),
                signal = self.signals.recv() => match signal {
                    Ok(signal) => match self.watch.hears(&signal) {
                        Heard::Nothing => {}
                        Heard::Changed => self.read_soon(),
                        Heard::Frame(frame) => self.ready.push_back(frame),
                        Heard::ChangedWith(frame) => {
                            self.ready.push_back(frame);
                            self.read_soon();
                        }
                    },
                    // It fell behind, and cannot know what it was not told.
                    Err(RecvError::Lagged(_)) => self.read_soon(),
                    Err(RecvError::Closed) => return self.end("the API is stopping"),
                },
                () = tokio::time::sleep_until(self.read_at) => {
                    match self.watch.read(&self.state).await {
                        Ok(frames) => self.ready.extend(frames),
                        Err(error) => {
                            warn!(?error, "an event stream could not read what it shows");
                            return self.end("a read failed");
                        }
                    }
                    self.last_read = Instant::now();
                    self.read_at = self.last_read + self.watch.unprompted(&self.state.pace);
                }
                _ = self.keep_alive.tick() => self.ready.push_back(Bytes::from_static(KEEP_ALIVE)),
            }
        }
    }

    /// Brings the next read forward: to now, or to where it is `settle` after the last.
    fn read_soon(&mut self) {
        let soonest = (self.last_read + self.state.pace.settle).max(Instant::now());
        self.read_at = self.read_at.min(soonest);
    }

    fn end(&mut self, reason: &'static str) -> Option<Bytes> {
        self.ending.reason = reason;
        None
    }
}

/// Returns when the hub is closed. A hub that is gone is closed too.
async fn told_to_close(closed: &mut watch::Receiver<bool>) {
    let _ = closed.wait_for(|&closed| closed).await;
}

/// Returns at `moment`, or never if there is none.
async fn at(moment: Option<Instant>) {
    match moment {
        Some(moment) => tokio::time::sleep_until(moment).await,
        None => std::future::pending().await,
    }
}

/// Says in the log that a stream is over, and why.
///
/// It does so when it is dropped, which is when the stream is: at an end of its own
/// making, or because the server dropped a response nobody was reading any more. The
/// line that says a request was handled is logged when its response starts, so for a
/// stream this is the line that says when it ended.
struct Ending {
    /// The span of the request, which is no longer entered when the stream is dropped.
    span: Span,
    opened: std::time::Instant,
    reason: &'static str,
}

impl Ending {
    fn begin() -> Self {
        Self {
            span: Span::current(),
            opened: std::time::Instant::now(),
            reason: "its reader has gone",
        }
    }
}

impl Drop for Ending {
    fn drop(&mut self) {
        let _request = self.span.enter();
        info!(
            reason = self.reason,
            open_for = ?self.opened.elapsed(),
            "the event stream has ended"
        );
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn an_event_is_its_name_and_its_data_on_one_line() {
        let event = frame("plan", &json!({ "status": "routing", "routes_done": 1 })).unwrap();
        assert_eq!(
            event,
            "event: plan\ndata: {\"routes_done\":1,\"status\":\"routing\"}\n\n"
        );

        // A line break in a text would end the event half way. It is written as `\n`.
        let event = frame("plan", &json!({ "error": "first line\nsecond line\r\n" })).unwrap();
        let text = std::str::from_utf8(&event).unwrap();
        assert_eq!(text.matches('\n').count(), 3, "{text:?}");
        assert!(text.ends_with("\n\n") && !text.contains('\r'));
        assert!(text.contains(r"first line\nsecond line\r\n"), "{text:?}");
    }

    #[test]
    fn a_keep_alive_is_a_comment() {
        let text = std::str::from_utf8(KEEP_ALIVE).unwrap();
        assert!(text.starts_with(':') && text.ends_with("\n\n"));
    }
}
