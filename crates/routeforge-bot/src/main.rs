//! RouteForge Telegram bot: takes parcel orders in a chat, and places them through the
//! HTTP API.
//!
//! A person shares a location, and the bot asks with buttons for what an order needs
//! beside it: a delivery or a pickup, how many parcels, which day. It shows what it is
//! about to place and places it when that is confirmed. What else a client or an
//! operator may do is a command. No model is asked anything, and the bot takes no free
//! text (D58).
//!
//! The bot has no authority of its own (D9). It reaches the system through the API
//! alone, and asks there with a token of the person it is talking to, which the API
//! gives it for showing a key (D60). So whatever the bot is made to do, the API allows
//! it what it allows that person.
//!
//! Settings, all from the environment:
//!
//! | Variable | Default | |
//! |---|---|---|
//! | `TELEGRAM_BOT_TOKEN` | none | What @BotFather gave. Without it the bot idles (D8). |
//! | `BOT_SERVICE_KEY` | none, required with a token | The key the API was started with. At least 32 bytes. |
//! | `BOT_API_URL` | `http://127.0.0.1:8080` | Where the API is. |
//! | `TELEGRAM_API_URL` | Telegram's own | Where the Bot API is, for a server of one's own. |
//! | `LOG_FORMAT` | `text` | `json` for one JSON object per line. |
//! | `RUST_LOG` | `info` | What to log, in the syntax of `tracing`'s `EnvFilter`. |
//!
//! The days the bot offers are those of its clock, in the zone its machine is set to:
//! `TZ` sets it, and a container without it is on UTC.
//!
//! The decisions cited as D1, D2, … are in `docs/architecture.md`.

mod api;
mod chat;
mod config;
mod telegram;
#[cfg(test)]
mod testing;

use std::io::IsTerminal;
use std::sync::Arc;

use chrono::Local;
use tokio::sync::watch;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::api::HttpApi;
use crate::chat::Chats;
use crate::config::{Config, Secrets};
use crate::telegram::Pace;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging();
    let config = Config::from_env()?;
    let secrets = Secrets::from_env()?;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        ?config,
        today = %Local::now().format("%Y-%m-%d %:z"),
        "the bot is starting"
    );

    let (stop, mut stopping) = watch::channel(false);
    tokio::spawn(async move {
        told_to_stop().await;
        info!("told to stop");
        stop.send_replace(true);
    });

    match secrets {
        // Nobody to talk to. It holds its place in the stack and waits (D8).
        None => {
            info!(
                "TELEGRAM_BOT_TOKEN is not set, so the bot idles: \
                 @BotFather gives a token, and .env is where it goes"
            );
            let _ = stopping.wait_for(|&stop| stop).await;
        }
        Some(secrets) => {
            let api = HttpApi::new(&config.api_url, &secrets.service_key)?;
            let bot = telegram::bot(&secrets.telegram_token, config.telegram_api_url.clone())?;
            telegram::run(bot, Arc::new(Chats::new(api)), Pace::default(), stopping).await?;
        }
    }
    info!("the bot has stopped");
    Ok(())
}

/// Logs to standard output: lines for people by default, JSON with `LOG_FORMAT=json`.
/// Either way an event carries the fields of the span it happened in, which is where
/// the id is that the API logs the bot's requests under. The API and the solver set
/// their logging up the same way.
fn init_logging() {
    // An empty `RUST_LOG` would mean errors only. It is taken as not set.
    let filter = std::env::var("RUST_LOG")
        .ok()
        .filter(|directives| !directives.trim().is_empty())
        .and_then(|directives| EnvFilter::try_new(directives).ok())
        .unwrap_or_else(|| EnvFilter::new("info"));
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
}

/// Returns when the process is asked to stop: Ctrl-C anywhere, and SIGTERM where
/// there is one, which is what `docker stop` sends.
///
/// In a container the bot is the first process, and the first process is not ended by
/// a signal it does not listen for. Without this, stopping the stack would wait out
/// the ten seconds Docker allows and then kill.
async fn told_to_stop() {
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
