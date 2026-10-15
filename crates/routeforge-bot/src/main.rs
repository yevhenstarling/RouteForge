//! RouteForge Telegram bot: takes parcel orders in a chat, and places them through the
//! HTTP API.
//!
//! A person shares a location, and the bot asks with buttons for what an order needs
//! beside it: a delivery or a pickup, how many parcels, which day. It shows what it is
//! about to place and places it when that is confirmed. What else a client or an
//! operator may do is a command. No model is asked anything, and the bot reads no
//! meaning into free text (D58). A bot that was given a geocoding service takes words
//! for one thing, an address, and asks that service where it is (D72).
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
//! | `GEOCODER_URL` | none | A geocoding service that speaks as Nominatim does. With it the bot takes an address in words. |
//! | `LOG_FORMAT` | `text` | `json` for one JSON object per line. |
//! | `RUST_LOG` | `info` | What to log, in the syntax of `tracing`'s `EnvFilter`. |
//!
//! The days the bot offers are those of its clock, in the zone its machine is set to:
//! `TZ` sets it, and a container without it is on UTC.
//!
//! Without `GEOCODER_URL` the bot asks nobody but Telegram and the API. With it, what a
//! person writes as an address is sent to that service (D72). Whoever sets it to the
//! public Nominatim, `https://nominatim.openstreetmap.org`, takes on its usage policy:
//! <https://operations.osmfoundation.org/policies/nominatim/>.
//!
//! The decisions cited as D1, D2, … are in `docs/architecture.md`.

mod api;
mod chat;
mod config;
mod geocode;
mod telegram;
#[cfg(test)]
mod testing;

use std::sync::Arc;

use chrono::Local;
use routeforge_domain::service::{init_logging, told_to_stop};
use tokio::sync::watch;
use tracing::info;

use crate::api::HttpApi;
use crate::chat::Chats;
use crate::config::{Config, Secrets};
use crate::geocode::Nominatim;
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
            let geocoder = match &config.geocoder_url {
                Some(url) => {
                    info!(service = %url, "what is written as an address is looked up there");
                    Some(Nominatim::new(url)?)
                }
                None => {
                    info!(
                        "GEOCODER_URL is not set, so the bot looks no address up: \
                         a place is a location, or coordinates"
                    );
                    None
                }
            };
            let chats = Arc::new(Chats::new(api, geocoder));
            let bot = telegram::bot(&secrets.telegram_token, config.telegram_api_url.clone())?;
            telegram::run(bot, chats, Pace::default(), stopping).await?;
        }
    }
    info!("the bot has stopped");
    Ok(())
}
