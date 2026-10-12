//! The bot's side towards Telegram: updates in, messages out (D61, D62).
//!
//! Nothing is decided here. A message or the press of a button becomes an [`Event`],
//! `chat` says what goes back, and the [`Reply`] becomes a message of Telegram's: a
//! new one, or the one whose button was pressed, changed in its place.
//!
//! The bot asks Telegram for its updates by long polling, so it needs no address
//! that Telegram could reach. It works in private chats alone: what a chat is putting
//! together is one person's order.
//!
//! [`run`] returns when it is told to stop, and ends with an error when Telegram will
//! not have the token. A Telegram that cannot be reached is tried again, for as long
//! as it takes.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use chrono::Local;
use reqwest::Url;
use teloxide::dispatching::UpdateHandler;
use teloxide::prelude::*;
use teloxide::types::{
    BotCommand, ButtonRequest, InlineKeyboardButton, InlineKeyboardMarkup, KeyboardButton,
    KeyboardMarkup, MessageId, UserId,
};
use teloxide::update_listeners::Polling;
use teloxide::{ApiError, RequestError};
use tokio::sync::watch;
use tracing::{Instrument, debug, info, info_span, warn};

use crate::api::HttpApi;
use crate::chat::{COMMANDS, Chats, Command, Event, Key, Keys, Place, Reply, Sender};

/// How the bot paces itself towards Telegram.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pace {
    /// How long Telegram may keep a request for updates open when it has none. It has
    /// to be well inside the seventeen seconds the client gives any request.
    pub(crate) poll: Duration,
    /// How long to wait before Telegram is tried again when it cannot be reached:
    /// this long at first, then twice as long each time, up to `retry_up_to`. As the
    /// solver waits for its database.
    pub(crate) retry_from: Duration,
    pub(crate) retry_up_to: Duration,
    /// How long the bot has to finish what it has in hand once it is told to stop.
    /// `docker stop` waits ten seconds before it kills, and this has to be inside
    /// that.
    pub(crate) stop_within: Duration,
}

impl Default for Pace {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(10),
            retry_from: Duration::from_secs(1),
            retry_up_to: Duration::from_secs(30),
            stop_within: Duration::from_secs(8),
        }
    }
}

/// The bot as Telegram knows it by `token`, at Telegram's own address unless another
/// is given.
///
/// It is never logged: its `Debug` shows the token.
pub(crate) fn bot(token: &str, api_url: Option<Url>) -> anyhow::Result<Bot> {
    // The settings teloxide gives its own client, built here so that failing to
    // build it is an error and no panic. TLS is rustls with the roots compiled in,
    // so the image needs no certificates of its own (D12, D62).
    let client = teloxide::net::default_reqwest_settings()
        .build()
        .context("could not make the client Telegram is asked with")?;
    let bot = Bot::with_client(token, client);
    Ok(match api_url {
        Some(url) => bot.set_api_url(url),
        None => bot,
    })
}

/// Talks to Telegram until it is told to stop.
///
/// # Errors
///
/// When Telegram refuses the token. Nothing the bot could do would mend that, and a
/// process that ends says so more clearly than one that goes on trying.
pub(crate) async fn run(
    bot: Bot,
    chats: Arc<Chats<HttpApi>>,
    pace: Pace,
    mut stopping: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut dispatcher = Dispatcher::builder(bot.clone(), schema())
        .dependencies(dptree::deps![chats])
        // Telegram is asked for messages and presses alone, so nothing else comes.
        .default_handler(|update| async move {
            debug!(update_id = update.id.0, "an update the bot has no use for");
        })
        .error_handler(Arc::new(|error: RequestError| async move {
            warn!(%error, "could not answer a chat");
        }))
        .build();
    // teloxide's own handler of Ctrl-C hears no SIGTERM, which is what stops a
    // container. The bot is told to stop from outside, and passes it on (D62).
    let stop = dispatcher.shutdown_token();

    let mut retry_in = pace.retry_from;
    loop {
        let reached = tokio::select! {
            reached = bot.get_me().send() => reached,
            () = told(&mut stopping) => return Ok(()),
        };
        match reached {
            Ok(me) => {
                let username = me.user.username.as_deref().unwrap_or_default();
                info!(username, "Telegram knows the bot; listening for its chats");
                retry_in = pace.retry_from;
                if let Err(error) = bot.set_my_commands(menu()).await {
                    warn!(%error, "could not tell Telegram the bot's commands");
                }

                let listener = Polling::builder(bot.clone())
                    .timeout(pace.poll)
                    .delete_webhook()
                    .await
                    .build();
                let on_error = Arc::new(|error: RequestError| async move {
                    warn!(%error, "could not ask Telegram for updates");
                });
                let listening = dispatcher.try_dispatch_with_listener(listener, on_error);
                tokio::pin!(listening);
                tokio::select! {
                    ended = &mut listening => match ended {
                        Ok(()) => warn!("stopped listening without having been told to"),
                        Err(error) if is_refusal(&error) => bail!(refusal()),
                        Err(error) => warn!(%error, "lost Telegram before listening had begun"),
                    },
                    () = told(&mut stopping) => {
                        // The listener tells Telegram which updates it has had, so
                        // that they are not sent again, and the chats that are being
                        // answered are answered.
                        let stops = stop.shutdown().is_ok();
                        let in_time = tokio::time::timeout(pace.stop_within, &mut listening);
                        if stops && in_time.await.is_err() {
                            warn!(within = ?pace.stop_within, "was still at it; stopping all the same");
                        }
                        return Ok(());
                    }
                }
            }
            Err(error) if is_refusal(&error) => bail!(refusal()),
            Err(error) => warn!(%error, ?retry_in, "cannot reach Telegram; trying again"),
        }

        tokio::select! {
            () = tokio::time::sleep(retry_in) => {}
            () = told(&mut stopping) => return Ok(()),
        }
        retry_in = (retry_in * 2).min(pace.retry_up_to);
    }
}

/// Whether Telegram will not have the token: it knows no bot by it.
fn is_refusal(error: &RequestError) -> bool {
    matches!(error, RequestError::Api(ApiError::InvalidToken))
}

fn refusal() -> &'static str {
    "Telegram refused TELEGRAM_BOT_TOKEN: it knows no bot by that token. \
     @BotFather gives one, and gives a new one for a bot whose token was revoked"
}

/// Returns when the bot is told to stop. A teller that is gone has told it.
async fn told(stopping: &mut watch::Receiver<bool>) {
    let _ = stopping.wait_for(|&stop| stop).await;
}

/// The commands Telegram offers in its menu. `/start` is not one a person picks.
fn menu() -> Vec<BotCommand> {
    COMMANDS
        .iter()
        .filter(|(command, ..)| *command != Command::Start)
        .map(|(_, name, says)| BotCommand::new(*name, *says))
        .collect()
}

/// What the bot wants of Telegram: messages, and presses of its buttons.
fn schema() -> UpdateHandler<RequestError> {
    dptree::entry()
        .branch(Update::filter_message().endpoint(on_message))
        .branch(Update::filter_callback_query().endpoint(on_button))
}

async fn on_message(
    bot: Bot,
    chats: Arc<Chats<HttpApi>>,
    update: Update,
    message: Message,
) -> Result<(), RequestError> {
    // In a group, everybody's presses would go into one person's order.
    if !message.chat.is_private() {
        return Ok(());
    }
    let from = message.from.as_ref().map(|user| user.id);
    let Some(sender) = sender(&update, message.chat.id, from, message.id) else {
        return Ok(());
    };
    let span = span_of(&update, &sender);
    async {
        let reply = chats.handle(&sender, event_of(&message)).await;
        send(&bot, message.chat.id, None, reply).await
    }
    .instrument(span)
    .await
}

async fn on_button(
    bot: Bot,
    chats: Arc<Chats<HttpApi>>,
    update: Update,
    query: CallbackQuery,
) -> Result<(), RequestError> {
    // Whatever comes of the press, the button stops showing that it waits.
    if let Err(error) = bot.answer_callback_query(query.id.clone()).await {
        debug!(%error, "could not tell Telegram that a press was heard");
    }
    // A button of the bot's is under a message of the bot's.
    let Some(pressed_on) = &query.message else {
        return Ok(());
    };
    let chat = pressed_on.chat();
    if !chat.is_private() {
        return Ok(());
    }
    let Some(sender) = sender(&update, chat.id, Some(query.from.id), pressed_on.id()) else {
        return Ok(());
    };
    let span = span_of(&update, &sender);
    async {
        let pressed = Event::Pressed(query.data.clone().unwrap_or_default());
        let reply = chats.handle(&sender, pressed).await;
        send(&bot, chat.id, Some(pressed_on.id()), reply).await
    }
    .instrument(span)
    .await
}

/// Who sent an update, as `chat` wants to know it. `None` for a sender the bot cannot
/// have an account for: a channel, which is nobody.
fn sender(
    update: &Update,
    chat: ChatId,
    user: Option<UserId>,
    message: MessageId,
) -> Option<Sender> {
    // Telegram promises its ids at most 52 bits, so they fit.
    let telegram_id = i64::try_from(user?.0).ok()?;
    Some(Sender {
        chat: chat.0,
        telegram_id,
        message: message.0,
        // By the bot's clock, which is the zone its machine is set to.
        today: Local::now().date_naive(),
        // What the API logs the bot's requests under, and a solver the plan that
        // comes of one (D42).
        request_id: format!("tg-{}", update.id.0),
    })
}

/// The span everything about one update is logged in.
fn span_of(update: &Update, sender: &Sender) -> tracing::Span {
    info_span!(
        "update",
        update_id = update.id.0,
        chat = sender.chat,
        request_id = %sender.request_id,
    )
}

/// What a message is to `chat`: a place, words, or something else.
fn event_of(message: &Message) -> Event {
    // A place that was picked from Telegram's list comes with its name and address.
    if let Some(venue) = message.venue() {
        let named: Vec<&str> = [venue.title.trim(), venue.address.trim()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect();
        return Event::Place(Place {
            lat: venue.location.latitude,
            lon: venue.location.longitude,
            address: Some(named.join(", ")),
        });
    }
    if let Some(location) = message.location() {
        return Event::Place(Place {
            lat: location.latitude,
            lon: location.longitude,
            address: None,
        });
    }
    match message.text() {
        Some(text) => Event::Text(text.to_owned()),
        None => Event::Other,
    }
}

/// Says a reply in a chat: in the place of the message whose button was pressed, if
/// the reply is to go there, and as a new message otherwise.
async fn send(
    bot: &Bot,
    chat: ChatId,
    pressed_on: Option<MessageId>,
    reply: Reply,
) -> Result<(), RequestError> {
    if let (true, Some(message)) = (reply.in_place, pressed_on) {
        let change = bot.edit_message_text(chat, message, reply.text.clone());
        let changed = match &reply.keys {
            Keys::Under(rows) => change.reply_markup(under(rows)).await,
            // Without buttons of its own, the message loses the ones it had.
            Keys::None | Keys::ShareLocation => change.await,
        };
        match changed {
            Ok(_) => return Ok(()),
            // Pressed again with nothing new to say: it says what it said.
            Err(RequestError::Api(ApiError::MessageNotModified)) => return Ok(()),
            // Too old to be changed, or gone. It is said anew.
            Err(error) => debug!(%error, "could not change a message; sending a new one"),
        }
    }
    let message = bot.send_message(chat, reply.text);
    match &reply.keys {
        Keys::None => message.await?,
        Keys::Under(rows) => message.reply_markup(under(rows)).await?,
        Keys::ShareLocation => message.reply_markup(share_location()).await?,
    };
    Ok(())
}

/// Buttons under a message, each carrying what it means.
fn under(rows: &[Vec<Key>]) -> InlineKeyboardMarkup {
    let rows = rows.iter().map(|row| {
        row.iter()
            .map(|key| InlineKeyboardButton::callback(key.label.clone(), key.press.data()))
            .collect::<Vec<_>>()
    });
    InlineKeyboardMarkup::new(rows)
}

/// A keyboard of one button, which sends where the person is. Telegram asks them
/// first.
fn share_location() -> KeyboardMarkup {
    let share = KeyboardButton::new("Share my location").request(ButtonRequest::Location);
    KeyboardMarkup::new([[share]])
        .resize_keyboard()
        .one_time_keyboard()
}

#[cfg(test)]
mod tests;
