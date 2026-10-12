//! What the bot says and does: every chat's side of it, with no Telegram in it (D61).
//!
//! [`Chats::handle`] takes what came from a chat, an [`Event`], and gives what goes
//! back, a [`Reply`]: a text, and the buttons under it. `telegram` turns Telegram's
//! updates into the one and the other into Telegram's messages. So everything the bot
//! decides is decided here, and can be tried against a stand-in for the API with no
//! Telegram anywhere.
//!
//! No model is asked anything (D58). An order begins with a place, a location that was
//! shared or coordinates that were typed. The bot asks with buttons for what an order
//! needs beside it, shows what it is about to place, and places it when that is
//! confirmed. Everything else is a command.
//!
//! A bot that was given a geocoding service takes a place in words as well (D72). It
//! asks the service where that is, shows the places it found, and the one that is
//! picked begins the order. Words that are no command are then an address. The bot
//! reads no more into them than that, and never acts on them: what a search finds is
//! shown, and picked by a button.
//!
//! Three things hold throughout:
//!
//! - **The bot has no authority of its own (D9).** What it asks of the API it asks
//!   with a token of the person it is talking to, which it is given anew each time. So
//!   the API answers as it would answer them, and a role an admin has changed applies
//!   to the next thing they ask. Where the bot looks at a role itself it is to say
//!   sooner what the API would say anyway.
//! - **What changes something is confirmed with a button**, and is done by the press
//!   of that button and by nothing before it: placing an order, withdrawing one,
//!   asking for a plan.
//! - **An order that is being put together is in memory**, one for each chat, and is
//!   gone when the bot stops. A button says which draft it belongs to, so one that is
//!   left over from an earlier draft is told that it is out of date.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use chrono::{Days, NaiveDate};
use routeforge_domain::{
    LatLon, Order, OrderFilter, OrderId, OrderKind, OrderRequest, OrderStatus, Plan, PlanRequest,
    PlanStatus, Role, Session,
};
use tracing::{error, info, warn};

use crate::api::{Api, Caller, Failure};
use crate::geocode::{Found, Geocoder, MOST_FOUND, Unfound};

/// How many drafts are kept. One who begins an order and never ends it leaves a draft
/// behind, and nothing else would ever take it away. As many searches for an address
/// are kept, for the same reason.
const MOST_DRAFTS: usize = 1024;

/// The longest text that is looked up as an address. No address is longer, and what
/// is, is not sent to a service to make something of.
const ADDRESS_ASKED_LONGEST: usize = 200;

/// The most parcels a button offers.
const MOST_PARCELS: u32 = 5;

/// How far ahead a button offers a day: today, tomorrow and the day after.
const DAYS_AHEAD: u64 = 2;

/// How many orders `/orders` shows, each as a button.
const ORDERS_SHOWN: usize = 10;

/// The orders the API gives at once, at the most (D44), and how many times the bot
/// asks for as many before it takes what it has.
const PAGE: u32 = 1_000;
const MOST_PAGES: usize = 20;

/// The longest address the API takes for an order.
const ADDRESS_LONGEST: usize = 300;

const NOT_UNDERSTOOD: &str = "I take no free text. An order begins with a location, and \
    everything else is a command: /help lists them.";
const HOW_TO_BEGIN: &str = "share its place: the button below, or the paperclip and then \
    Location. Where a location cannot be shared, write its coordinates: /order 50.4501 30.5234";
const STALE: &str = "This order is no longer open: it was placed or dropped, or you began \
    another. Share a location to begin a new one.";
const AWAY: &str = "The service does not answer right now. Try again in a moment.";

// The same two, for a bot that looks addresses up (D72).
const NOT_UNDERSTOOD_BY_ADDRESS: &str = "An order begins with a location or with an address \
    in words, and everything else is a command: /help lists them.";
const HOW_TO_BEGIN_BY_ADDRESS: &str = "share its place: the button below, or the paperclip \
    and then Location. Or write its address, with the town: Khreshchatyk 22, Kyiv. \
    Coordinates will do as well: /order 50.4501 30.5234";
const OTHER_WAYS: &str = "Share the location, or write its coordinates: /order 50.4501 30.5234";
const STALE_SEARCH: &str = "That search is over: a place was picked, or you searched again. \
    Write the address once more to look for it.";
/// Where the places of a search are from. Whoever shows them owes that word.
const CREDIT: &str = "Places: © OpenStreetMap contributors.";

/// What the bot understands beside a location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Start,
    Help,
    Order,
    Orders,
    Cancel,
    Pending,
    Plan,
    Status,
}

/// Every command: how it is written after the slash, and what Telegram's menu says
/// of it. In the order of the menu.
pub(crate) const COMMANDS: &[(Command, &str, &str)] = &[
    (
        Command::Order,
        "order",
        "Place an order: share a location, or give its coordinates",
    ),
    (Command::Orders, "orders", "Your orders from today on"),
    (
        Command::Cancel,
        "cancel",
        "Drop the order you are putting together",
    ),
    (
        Command::Pending,
        "pending",
        "For operators: the pending orders of a day",
    ),
    (Command::Plan, "plan", "For operators: plan a day"),
    (
        Command::Status,
        "status",
        "For operators: how far the plan of a day is",
    ),
    (Command::Help, "help", "What I understand"),
    (Command::Start, "start", "What this is"),
];

impl Command {
    /// The command a text is, with what follows it. `None` if it is none.
    fn read(text: &str) -> Option<(Self, &str)> {
        let rest = text.trim_start().strip_prefix('/')?;
        let (word, arguments) = match rest.split_once(char::is_whitespace) {
            Some((word, arguments)) => (word, arguments.trim()),
            None => (rest, ""),
        };
        // In a group a command names the bot it is for, as in `/help@SomeBot`, and in
        // a private chat it may.
        let name = word.split_once('@').map_or(word, |(name, _)| name);
        COMMANDS
            .iter()
            .find(|(_, known, _)| known.eq_ignore_ascii_case(name))
            .map(|(command, ..)| (*command, arguments))
    }
}

/// Who sent something, and when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sender {
    /// The chat, which in a private chat is the person.
    pub(crate) chat: i64,
    /// The id Telegram gives the person.
    pub(crate) telegram_id: i64,
    /// The message that was sent, or the one whose button was pressed.
    pub(crate) message: i32,
    /// The day it is, by the bot's clock.
    pub(crate) today: NaiveDate,
    /// The id the requests to the API carry that come of this (D42).
    pub(crate) request_id: String,
}

/// What came from a chat.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Event {
    /// Words. A command, if they are one.
    Text(String),
    /// A location was shared, or a venue with its address.
    Place(Place),
    /// A button was pressed. This is what it carried.
    Pressed(String),
    /// Anything else: a picture, a sticker, a voice.
    Other,
}

/// Where an order is to be, as it came from the chat: not yet checked.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Place {
    pub(crate) lat: f64,
    pub(crate) lon: f64,
    pub(crate) address: Option<String>,
}

/// What goes back to the chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reply {
    pub(crate) text: String,
    pub(crate) keys: Keys,
    /// Whether it takes the place of the message whose button was pressed. Then that
    /// message changes, and its old buttons go with it.
    pub(crate) in_place: bool,
}

/// The buttons a reply comes with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Keys {
    None,
    /// Buttons under the message, row by row.
    Under(Vec<Vec<Key>>),
    /// A keyboard of one button, which shares the person's location.
    ShareLocation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Key {
    pub(crate) label: String,
    pub(crate) press: Press,
}

impl Reply {
    fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            keys: Keys::None,
            in_place: false,
        }
    }

    fn in_place(text: impl Into<String>) -> Self {
        Self {
            in_place: true,
            ..Self::new(text)
        }
    }

    fn with(mut self, rows: Vec<Vec<Key>>) -> Self {
        self.keys = Keys::Under(rows);
        self
    }
}

fn key(label: impl Into<String>, press: Press) -> Vec<Key> {
    vec![Key {
        label: label.into(),
        press,
    }]
}

/// What a button means. It travels to Telegram and back as the button's data, which
/// may be 64 bytes long.
///
/// The buttons of an order that is being put together name the draft they belong to,
/// by the message that began it. The others say everything themselves, an order or a
/// day, so they still work when the bot has been started again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Press {
    Kind(i32, OrderKind),
    Parcels(i32, u32),
    Day(i32, NaiveDate),
    /// Place the order: the confirmation.
    Place(i32),
    Drop(i32),
    Show(OrderId),
    /// Asks whether to withdraw. [`Press::WithdrawYes`] does it.
    Withdraw(OrderId),
    WithdrawYes(OrderId),
    Keep(OrderId),
    Pending(NaiveDate),
    /// Asks whether to plan. [`Press::PlanYes`] does it.
    Plan(NaiveDate),
    PlanYes(NaiveDate),
    PlanNo,
    Status(NaiveDate),
    /// One of the places a search for an address found: the search by the message
    /// that asked it, and the place by where it stands among those found.
    Found(i32, u8),
    /// None of them.
    NotFound(i32),
}

impl Press {
    /// What the button carries.
    pub(crate) fn data(self) -> String {
        match self {
            Self::Found(search, place) => format!("f:{search}:{place}"),
            Self::NotFound(search) => format!("fx:{search}"),
            Self::Kind(draft, kind) => format!("k:{draft}:{kind}"),
            Self::Parcels(draft, parcels) => format!("n:{draft}:{parcels}"),
            Self::Day(draft, day) => format!("d:{draft}:{day}"),
            Self::Place(draft) => format!("ok:{draft}"),
            Self::Drop(draft) => format!("no:{draft}"),
            Self::Show(order) => format!("o:{order}"),
            Self::Withdraw(order) => format!("w:{order}"),
            Self::WithdrawYes(order) => format!("wy:{order}"),
            Self::Keep(order) => format!("wn:{order}"),
            Self::Pending(day) => format!("pe:{day}"),
            Self::Plan(day) => format!("p:{day}"),
            Self::PlanYes(day) => format!("py:{day}"),
            Self::PlanNo => "pn".to_owned(),
            Self::Status(day) => format!("s:{day}"),
        }
    }

    /// The meaning of what a button carried. `None` for what no button of this build
    /// carries: one of a build before it, or something a person made up.
    pub(crate) fn read(data: &str) -> Option<Self> {
        let mut parts = data.split(':');
        let press = match (parts.next()?, parts.next(), parts.next()) {
            ("k", Some(draft), Some(kind)) => Self::Kind(draft.parse().ok()?, kind.parse().ok()?),
            ("n", Some(draft), Some(parcels)) => {
                Self::Parcels(draft.parse().ok()?, parcels.parse().ok()?)
            }
            ("d", Some(draft), Some(day)) => Self::Day(draft.parse().ok()?, day.parse().ok()?),
            ("ok", Some(draft), None) => Self::Place(draft.parse().ok()?),
            ("no", Some(draft), None) => Self::Drop(draft.parse().ok()?),
            ("o", Some(order), None) => Self::Show(order.parse().ok()?),
            ("w", Some(order), None) => Self::Withdraw(order.parse().ok()?),
            ("wy", Some(order), None) => Self::WithdrawYes(order.parse().ok()?),
            ("wn", Some(order), None) => Self::Keep(order.parse().ok()?),
            ("pe", Some(day), None) => Self::Pending(day.parse().ok()?),
            ("p", Some(day), None) => Self::Plan(day.parse().ok()?),
            ("py", Some(day), None) => Self::PlanYes(day.parse().ok()?),
            ("pn", None, None) => Self::PlanNo,
            ("s", Some(day), None) => Self::Status(day.parse().ok()?),
            ("f", Some(search), Some(place)) => {
                Self::Found(search.parse().ok()?, place.parse().ok()?)
            }
            ("fx", Some(search), None) => Self::NotFound(search.parse().ok()?),
            _ => return None,
        };
        parts.next().is_none().then_some(press)
    }
}

/// An order that is being put together.
#[derive(Clone, Debug, PartialEq)]
struct Draft {
    /// The message that began it, which its buttons name.
    id: i32,
    /// When it was begun, among all the drafts there are: the oldest go first when
    /// there are too many.
    age: u64,
    location: LatLon,
    address: Option<String>,
    kind: Option<OrderKind>,
    parcels: Option<u32>,
    day: Option<NaiveDate>,
}

impl Draft {
    /// Asks for the first thing the order still needs. When it needs nothing more,
    /// shows it and asks whether to place it.
    fn question(&self, today: NaiveDate, in_place: bool) -> Reply {
        let place = place(self.location, self.address.as_deref());
        let drop = key("Cancel", Press::Drop(self.id));
        let (text, mut rows) = match (self.kind, self.parcels, self.day) {
            (None, ..) => (
                format!("An order at {place}.\nIs it a delivery to there, or a pickup from there?"),
                vec![vec![
                    Key {
                        label: "Delivery".to_owned(),
                        press: Press::Kind(self.id, OrderKind::Delivery),
                    },
                    Key {
                        label: "Pickup".to_owned(),
                        press: Press::Kind(self.id, OrderKind::Pickup),
                    },
                ]],
            ),
            (Some(kind), None, _) => (
                format!(
                    "{}\nHow many parcels?",
                    sentence(&format!("{} {place}", way(kind)))
                ),
                vec![
                    (1..=MOST_PARCELS)
                        .map(|parcels| Key {
                            label: parcels.to_string(),
                            press: Press::Parcels(self.id, parcels),
                        })
                        .collect(),
                ],
            ),
            (Some(kind), Some(parcels), None) => (
                format!("{}\nOn which day?", sentence(&order(kind, parcels, &place))),
                days(today, |day| Press::Day(self.id, day)),
            ),
            (Some(kind), Some(parcels), Some(day)) => (
                format!(
                    "{}\nPlace this order?",
                    sentence(&format!(
                        "{}, on {}",
                        order(kind, parcels, &place),
                        name(day)
                    ))
                ),
                vec![key("Place the order", Press::Place(self.id))],
            ),
        };
        rows.push(drop);
        Reply {
            text,
            keys: Keys::Under(rows),
            in_place,
        }
    }
}

/// The places a search for an address found, until one of them is picked.
#[derive(Clone, Debug, PartialEq)]
struct Search {
    /// The message that asked, which the buttons of its places name.
    id: i32,
    /// When it was made, among all the drafts and searches there are.
    age: u64,
    found: Vec<Found>,
}

/// The chats the bot is in, the API it asks on their behalf, and what it asks where an
/// address is.
pub(crate) struct Chats<A, G> {
    api: A,
    /// What finds where an address is. A bot that has none takes none (D72).
    geocoder: G,
    /// The order each chat is putting together, if it is.
    drafts: Mutex<HashMap<i64, Draft>>,
    /// The search for an address each chat has made last, if its places still wait
    /// for one of them to be picked.
    searches: Mutex<HashMap<i64, Search>>,
    /// How many drafts and searches were ever begun: the age of the next one.
    begun: AtomicU64,
}

impl<A: Api, G: Geocoder> Chats<A, G> {
    pub(crate) fn new(api: A, geocoder: G) -> Self {
        Self {
            api,
            geocoder,
            drafts: Mutex::new(HashMap::new()),
            searches: Mutex::new(HashMap::new()),
            begun: AtomicU64::new(0),
        }
    }

    /// What goes back for what came. It always answers: what goes wrong on the way is
    /// said in the reply, in words for the person.
    pub(crate) async fn handle(&self, sender: &Sender, event: Event) -> Reply {
        match event {
            Event::Text(text) => match Command::read(&text) {
                Some((command, arguments)) => self.command(sender, command, arguments).await,
                None if !self.geocoder.is_on() => Reply::new(NOT_UNDERSTOOD),
                // What begins as a command and is none was meant as one, and is no
                // address to ask anybody about.
                None if text.trim_start().starts_with('/') => Reply::new(NOT_UNDERSTOOD_BY_ADDRESS),
                // Other words are an address, to a bot that looks addresses up.
                None => self.look_up(sender, &text).await,
            },
            Event::Place(place) => self.begin(sender, place, false),
            Event::Pressed(data) => match Press::read(&data) {
                Some(press) => self.pressed(sender, press).await,
                None => Reply::in_place(
                    "That button is not one I know any more. /help lists what I understand.",
                ),
            },
            Event::Other if self.geocoder.is_on() => Reply::new(NOT_UNDERSTOOD_BY_ADDRESS),
            Event::Other => Reply::new(NOT_UNDERSTOOD),
        }
    }

    /// How an order is begun, in the words for a bot that takes addresses or for one
    /// that does not.
    fn how_to_begin(&self) -> &'static str {
        if self.geocoder.is_on() {
            HOW_TO_BEGIN_BY_ADDRESS
        } else {
            HOW_TO_BEGIN
        }
    }

    async fn command(&self, sender: &Sender, command: Command, arguments: &str) -> Reply {
        match command {
            Command::Start => self.help(sender, true).await,
            Command::Help => self.help(sender, false).await,
            Command::Order => self.order(sender, arguments).await,
            Command::Orders => self.orders(sender).await,
            Command::Cancel => self.cancel(sender),
            Command::Pending => {
                self.ask_day(sender, "The pending orders of which day?", Press::Pending)
                    .await
            }
            Command::Plan => self.ask_day(sender, "Plan which day?", Press::Plan).await,
            Command::Status => {
                self.ask_day(sender, "The plan of which day?", Press::Status)
                    .await
            }
        }
    }

    async fn pressed(&self, sender: &Sender, press: Press) -> Reply {
        match press {
            Press::Kind(draft, kind) => self.fill(sender, draft, |draft| draft.kind = Some(kind)),
            Press::Parcels(draft, parcels) => self.fill(sender, draft, |draft| {
                // What no button offers is not taken, whoever sent it.
                draft.parcels = (1..=MOST_PARCELS).contains(&parcels).then_some(parcels);
            }),
            Press::Day(draft, day) => self.fill_day(sender, draft, day),
            Press::Place(draft) => self.place(sender, draft).await,
            Press::Drop(draft) => self.drop_draft(sender, draft),
            Press::Show(order) => self.show(sender, order, false).await,
            Press::Withdraw(order) => self.ask_to_withdraw(sender, order).await,
            Press::WithdrawYes(order) => self.withdraw(sender, order).await,
            Press::Keep(order) => self.show(sender, order, true).await,
            Press::Pending(day) => self.pending(sender, day).await,
            Press::Plan(day) => self.ask_to_plan(sender, day).await,
            Press::PlanYes(day) => self.plan(sender, day).await,
            Press::PlanNo => Reply::in_place("Nothing was planned."),
            Press::Status(day) => self.status(sender, day).await,
            Press::Found(search, place) => self.pick(sender, search, place),
            Press::NotFound(search) => self.pick_none(sender, search),
        }
    }

    // ------------------------------------------------------------ who is asking

    /// A token of the person, as they are to the API at this moment.
    async fn login(&self, sender: &Sender) -> Result<Session, Reply> {
        self.api
            .telegram_login(sender.telegram_id, &sender.request_id)
            .await
            .map_err(|failure| Reply::new(no_session(&failure)))
    }

    /// A token of the person, if they are an operator or an admin.
    ///
    /// The API would refuse anybody else whatever the bot did (D9). This says so
    /// before the person has pressed their way to the refusal.
    async fn staff(&self, sender: &Sender) -> Result<Session, Reply> {
        let session = self.login(sender).await?;
        if is_staff(&session) {
            Ok(session)
        } else {
            Err(Reply::new(not_for(&session)))
        }
    }

    async fn help(&self, sender: &Sender, greeting: bool) -> Reply {
        let mut text = String::new();
        if greeting {
            text.push_str(
                "RouteForge takes parcel orders: a delivery to you, or a pickup from you.\n\n",
            );
        }
        text.push_str(&format!(
            "To place an order, {}\n\n\
             /orders shows your orders from today on, to look at or to withdraw.\n\
             /cancel drops the order you are putting together.",
            self.how_to_begin()
        ));
        // Who is asking decides what else is worth saying. With the API away, what
        // was said so far is true for everybody.
        match self
            .api
            .telegram_login(sender.telegram_id, &sender.request_id)
            .await
        {
            Ok(session) if is_staff(&session) => text.push_str(&format!(
                "\n\nYou are {} here, so these are yours as well:\n\
                 /pending shows the pending orders of a day.\n\
                 /plan plans a day.\n\
                 /status says how far the plan of a day is.",
                a(session.user.role)
            )),
            Ok(session) => text.push_str(&format!(
                "\n\nYou are a client here. Your account is {}: an admin can make it an \
                 operator's.",
                session.user.id
            )),
            Err(failure) => text.push_str(&format!("\n\n{}", no_session(&failure))),
        }
        Reply {
            text,
            keys: Keys::ShareLocation,
            in_place: false,
        }
    }

    // ------------------------------------------------------------ placing an order

    async fn order(&self, sender: &Sender, arguments: &str) -> Reply {
        if arguments.is_empty() {
            return Reply {
                text: format!("To place an order, {}", self.how_to_begin()),
                keys: Keys::ShareLocation,
                in_place: false,
            };
        }
        match coordinates(arguments) {
            Some((lat, lon)) => {
                let place = Place {
                    lat,
                    lon,
                    address: None,
                };
                self.begin(sender, place, false)
            }
            // What is no pair of numbers is an address, to a bot that looks them up.
            None if self.geocoder.is_on() => self.look_up(sender, arguments).await,
            None => Reply::new(
                "After /order I read a latitude and a longitude in degrees, as in \
                 /order 50.4501 30.5234",
            ),
        }
    }

    // ------------------------------------------------------------ a place in words

    /// Asks where an address is, and shows what was found, each place with a button.
    /// Nothing is begun here: the press of one of those buttons begins the order.
    async fn look_up(&self, sender: &Sender, words: &str) -> Reply {
        let words = words.trim();
        if words.is_empty() {
            return Reply::new(NOT_UNDERSTOOD_BY_ADDRESS);
        }
        if words.chars().count() > ADDRESS_ASKED_LONGEST {
            return Reply::new(format!(
                "That is too long for an address: I read up to {ADDRESS_ASKED_LONGEST} \
                 letters of one. {OTHER_WAYS}"
            ));
        }
        let near = self.depot(sender).await;
        let found = match self.geocoder.find(words, near).await {
            // The service is somebody else's, and what it says goes into a message. So:
            // as many places as there are buttons for, and of each name as much as
            // the address of an order may be.
            Ok(found) => found
                .into_iter()
                .take(MOST_FOUND)
                .map(|place| Found {
                    name: place.name.chars().take(ADDRESS_LONGEST).collect(),
                    ..place
                })
                .collect::<Vec<_>>(),
            Err(Unfound::Busy) => {
                return Reply::new(
                    "Too many addresses are being looked up at this moment. Try again in a \
                     few seconds.",
                );
            }
            Err(unfound @ Unfound::Away(_)) => {
                warn!(%unfound, "could not look an address up");
                return Reply::new(format!(
                    "The address service does not answer right now. {OTHER_WAYS}"
                ));
            }
        };
        if found.is_empty() {
            return Reply::new(format!(
                "I found no place by that. Write the street and the number, with the town. \
                 Or: {}",
                lower_first(OTHER_WAYS)
            ));
        }

        let none = Press::NotFound(sender.message);
        let place = |at: u8| Press::Found(sender.message, at);
        let (text, rows) = match found.as_slice() {
            [only] => (
                format!("Is it this place?\n{}\n\n{CREDIT}", only.name),
                vec![key("Yes, an order there", place(0)), key("No", none)],
            ),
            many => {
                // A button carries where its place stands, counted from nought, and
                // shows the number the list gives it, counted from one.
                let listed: Vec<String> = (1..)
                    .zip(many)
                    .map(|(number, found)| format!("{number}. {}", found.name))
                    .collect();
                let numbers = (0..)
                    .zip(many)
                    .map(|(at, _)| Key {
                        label: (u32::from(at) + 1).to_string(),
                        press: place(at),
                    })
                    .collect();
                (
                    format!("Which of these is it?\n{}\n\n{CREDIT}", listed.join("\n")),
                    vec![numbers, key("None of these", none)],
                )
            }
        };
        self.remember(
            sender.chat,
            Search {
                id: sender.message,
                age: self.begun.fetch_add(1, Ordering::Relaxed),
                found,
            },
        );
        Reply::new(text).with(rows)
    }

    /// Where the depot is, so that an address is looked for round it: a street has
    /// its name in many towns. `None` when the API does not say, and then nothing is
    /// preferred.
    async fn depot(&self, sender: &Sender) -> Option<LatLon> {
        let session = self
            .api
            .telegram_login(sender.telegram_id, &sender.request_id)
            .await
            .ok()?;
        let depot = self.api.get_depot(&caller(&session, sender)).await.ok()??;
        Some(depot.location)
    }

    /// Keeps what a search found, in the place of what the chat searched for before.
    fn remember(&self, chat: i64, search: Search) {
        let mut searches = self.searches();
        searches.insert(chat, search);
        if searches.len() > MOST_DRAFTS {
            let oldest = searches
                .iter()
                .min_by_key(|(_, search)| search.age)
                .map(|(chat, _)| *chat);
            if let Some(oldest) = oldest {
                searches.remove(&oldest);
            }
        }
    }

    /// Begins an order at the place of a search that was picked. The search is over
    /// with that: its other places go with the message they were listed in.
    ///
    /// A press that names no place of the chat's search picks nothing, and ends
    /// nothing: the places are still there for a button that names one.
    fn pick(&self, sender: &Sender, id: i32, place: u8) -> Reply {
        let picked = {
            let mut searches = self.searches();
            let picked = searches
                .get(&sender.chat)
                .filter(|search| search.id == id)
                .and_then(|search| search.found.get(usize::from(place)).cloned());
            if picked.is_some() {
                searches.remove(&sender.chat);
            }
            picked
        };
        match picked {
            Some(found) => {
                let place = Place {
                    lat: found.location.lat(),
                    lon: found.location.lon(),
                    address: Some(found.name),
                };
                self.begin(sender, place, true)
            }
            None => Reply::in_place(STALE_SEARCH),
        }
    }

    fn pick_none(&self, sender: &Sender, id: i32) -> Reply {
        let mut searches = self.searches();
        match searches.get(&sender.chat) {
            Some(search) if search.id == id => {
                searches.remove(&sender.chat);
                Reply::in_place(format!(
                    "Then I do not know the place. Write more of its address, with the \
                     town. Or: {}",
                    lower_first(OTHER_WAYS)
                ))
            }
            _ => Reply::in_place(STALE_SEARCH),
        }
    }

    fn searches(&self) -> MutexGuard<'_, HashMap<i64, Search>> {
        // Nothing is ever done under the lock that could leave the map half changed.
        self.searches.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Begins an order at a place. An order the chat was putting together gives way.
    ///
    /// The first question is a message of its own under a location that was shared,
    /// and takes the place of the list a place was picked from.
    fn begin(&self, sender: &Sender, place: Place, in_place: bool) -> Reply {
        let Ok(location) = LatLon::new(place.lat, place.lon) else {
            return Reply::new(
                "That is no position on Earth: a latitude runs from -90 to 90 and a \
                 longitude from -180 to 180.",
            );
        };
        let address = place
            .address
            .map(|address| {
                address
                    .trim()
                    .chars()
                    .take(ADDRESS_LONGEST)
                    .collect::<String>()
            })
            .filter(|address| !address.is_empty());
        let draft = Draft {
            id: sender.message,
            age: self.begun.fetch_add(1, Ordering::Relaxed),
            location,
            address,
            kind: None,
            parcels: None,
            day: None,
        };
        let question = draft.question(sender.today, in_place);

        let mut drafts = self.drafts();
        drafts.insert(sender.chat, draft);
        if drafts.len() > MOST_DRAFTS {
            let oldest = drafts
                .iter()
                .min_by_key(|(_, draft)| draft.age)
                .map(|(chat, _)| *chat);
            if let Some(oldest) = oldest {
                drafts.remove(&oldest);
            }
        }
        question
    }

    /// Notes what a button said of the chat's draft, and asks for what comes next.
    fn fill(&self, sender: &Sender, id: i32, note: impl FnOnce(&mut Draft)) -> Reply {
        let mut drafts = self.drafts();
        match drafts.get_mut(&sender.chat).filter(|draft| draft.id == id) {
            Some(draft) => {
                note(draft);
                draft.question(sender.today, true)
            }
            None => Reply::in_place(STALE),
        }
    }

    fn fill_day(&self, sender: &Sender, id: i32, day: NaiveDate) -> Reply {
        if on_offer(day, sender.today) {
            return self.fill(sender, id, |draft| draft.day = Some(day));
        }
        // A button that was left unpressed overnight names a day that has passed.
        let mut asked_again = self.fill(sender, id, |draft| draft.day = None);
        if asked_again.keys != Keys::None {
            asked_again.text = format!("That day is no longer on offer.\n\n{}", asked_again.text);
        }
        asked_again
    }

    /// Places the order the chat has put together: the one thing here that changes
    /// anything, done by the press of the button that says so and by nothing else.
    async fn place(&self, sender: &Sender, id: i32) -> Reply {
        // Taken out before the API is asked. A second press that arrives while the
        // first is on its way finds nothing to place.
        let taken = {
            let mut drafts = self.drafts();
            match drafts.get(&sender.chat) {
                Some(draft) if draft.id == id => drafts.remove(&sender.chat),
                _ => None,
            }
        };
        let Some(mut draft) = taken else {
            return Reply::in_place(STALE);
        };
        let (Some(kind), Some(parcels), Some(day)) = (draft.kind, draft.parcels, draft.day) else {
            // Nothing offers this button before the order is whole.
            let question = draft.question(sender.today, true);
            self.put_back(sender.chat, draft);
            return question;
        };
        if !on_offer(day, sender.today) {
            draft.day = None;
            let mut question = draft.question(sender.today, true);
            question.text = format!("That day is no longer on offer.\n\n{}", question.text);
            self.put_back(sender.chat, draft);
            return question;
        }

        let asked = OrderRequest {
            kind,
            service_date: day,
            location: draft.location,
            address: draft.address.clone(),
            demand: parcels,
        };
        let not_placed = match self.login(sender).await {
            Err(reply) => format!("{}\nThe order was not placed.", reply.text),
            Ok(session) => match self
                .api
                .create_order(&caller(&session, sender), &asked)
                .await
            {
                Ok(placed) => {
                    info!(order_id = %placed.id, service_date = %day, "placed an order");
                    return Reply::in_place(format!(
                        "Placed: {}.\nIt is pending until that day is planned. /orders shows it.",
                        described(&placed)
                    ));
                }
                // The request went out and no answer came back. The API may have
                // stored the order all the same.
                Err(failure @ Failure::Away(_)) => {
                    warn!(%failure, "do not know whether an order was placed");
                    "The service did not answer, so I cannot say whether the order was placed. \
                     /orders shows whether it is there: look before you press again."
                        .to_owned()
                }
                Err(failure) => {
                    format!("{}\nThe order was not placed.", refused(&failure, &session))
                }
            },
        };
        // Still the chat's to place, or to drop.
        let mut question = draft.question(sender.today, true);
        question.text = format!("{not_placed}\n\n{}", question.text);
        self.put_back(sender.chat, draft);
        question
    }

    fn drop_draft(&self, sender: &Sender, id: i32) -> Reply {
        let mut drafts = self.drafts();
        match drafts.get(&sender.chat) {
            Some(draft) if draft.id == id => {
                drafts.remove(&sender.chat);
                Reply::in_place("Dropped: nothing was placed.")
            }
            _ => Reply::in_place(STALE),
        }
    }

    fn cancel(&self, sender: &Sender) -> Reply {
        // A search whose places still wait to be picked is the beginning of an order.
        let searched = self.searches().remove(&sender.chat).is_some();
        let drafted = self.drafts().remove(&sender.chat).is_some();
        if searched || drafted {
            Reply::new("Dropped: nothing was placed.")
        } else {
            Reply::new("You are not putting an order together, so there is nothing to drop.")
        }
    }

    fn drafts(&self) -> MutexGuard<'_, HashMap<i64, Draft>> {
        // Nothing is ever done under the lock that could leave the map half changed.
        self.drafts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Gives a draft back to its chat, unless the chat has begun another meanwhile.
    fn put_back(&self, chat: i64, draft: Draft) {
        self.drafts().entry(chat).or_insert(draft);
    }

    // ------------------------------------------------------------ a client's orders

    async fn orders(&self, sender: &Sender) -> Reply {
        let session = match self.login(sender).await {
            Ok(session) => session,
            Err(reply) => return reply,
        };
        // Their own, also for an operator, who would otherwise be shown everybody's.
        let mine = OrderFilter {
            client_id: Some(session.user.id),
            ..OrderFilter::default()
        };
        let orders = match self.every_order(&caller(&session, sender), mine).await {
            Ok(orders) => orders,
            Err(failure) => return Reply::new(refused(&failure, &session)),
        };
        let mut coming: Vec<&Order> = orders
            .iter()
            .filter(|order| order.service_date >= sender.today)
            .filter(|order| order.status != OrderStatus::Cancelled)
            .collect();
        coming.sort_by_key(|order| (order.service_date, order.id));
        if coming.is_empty() {
            return Reply::new(
                "You have no orders for today or later. Share a location to place one.",
            );
        }

        let text = if coming.len() > ORDERS_SHOWN {
            format!(
                "You have {} orders for today or later. These are the first {ORDERS_SHOWN}. \
                 Press one to look at it, or to withdraw it.",
                coming.len()
            )
        } else {
            format!(
                "You have {} for today or later. Press one to look at it, or to withdraw it.",
                some(coming.len(), "order", "orders")
            )
        };
        let rows = coming
            .iter()
            .take(ORDERS_SHOWN)
            .map(|order| {
                key(
                    format!(
                        "{}: {} of {}, {}",
                        name(order.service_date),
                        order.kind,
                        order.demand,
                        order.status
                    ),
                    Press::Show(order.id),
                )
            })
            .collect();
        Reply::new(text).with(rows)
    }

    /// Every order that passes the filter, read a page at a time: each page is asked
    /// for by the id of the last order of the page before (D44).
    async fn every_order(
        &self,
        who: &Caller<'_>,
        mut filter: OrderFilter,
    ) -> Result<Vec<Order>, Failure> {
        filter.limit = Some(PAGE);
        let mut orders = Vec::new();
        for _ in 0..MOST_PAGES {
            let page = self.api.list_orders(who, &filter).await?;
            // A page that is not full is the last.
            let next = page
                .last()
                .map(|last| last.id)
                .filter(|_| page.len() >= PAGE as usize);
            orders.extend(page);
            match next {
                Some(last) => filter.after = Some(last),
                None => return Ok(orders),
            }
        }
        warn!(
            read = orders.len(),
            "there are more orders than the bot reads at once"
        );
        Ok(orders)
    }

    async fn show(&self, sender: &Sender, id: OrderId, in_place: bool) -> Reply {
        let session = match self.login(sender).await {
            Ok(session) => session,
            Err(reply) => return reply,
        };
        let reply = match self.api.get_order(&caller(&session, sender), id).await {
            Ok(order) => {
                let text = format!(
                    "{}\n{}\nOrder {}.",
                    sentence(&described(&order)),
                    standing(order.status),
                    order.id
                );
                if order.status == OrderStatus::Pending {
                    Reply::new(text).with(vec![key("Withdraw it", Press::Withdraw(order.id))])
                } else {
                    Reply::new(text)
                }
            }
            Err(failure) => Reply::new(refused(&failure, &session)),
        };
        Reply { in_place, ..reply }
    }

    /// Asks whether to withdraw an order. Nothing is withdrawn here.
    async fn ask_to_withdraw(&self, sender: &Sender, id: OrderId) -> Reply {
        let session = match self.login(sender).await {
            Ok(session) => session,
            Err(reply) => return reply,
        };
        match self.api.get_order(&caller(&session, sender), id).await {
            Ok(order) if order.status == OrderStatus::Pending => Reply::in_place(format!(
                "Withdraw this order?\n{}",
                sentence(&described(&order))
            ))
            .with(vec![
                key("Yes, withdraw it", Press::WithdrawYes(id)),
                key("No, keep it", Press::Keep(id)),
            ]),
            // It is no longer one that can be withdrawn. How it stands says why.
            Ok(_) => self.show(sender, id, true).await,
            Err(failure) => Reply::in_place(refused(&failure, &session)),
        }
    }

    async fn withdraw(&self, sender: &Sender, id: OrderId) -> Reply {
        let session = match self.login(sender).await {
            Ok(session) => session,
            Err(reply) => return reply,
        };
        match self.api.cancel_order(&caller(&session, sender), id).await {
            Ok(order) => {
                info!(order_id = %order.id, "withdrew an order");
                Reply::in_place(format!("Withdrawn: {}.", described(&order)))
            }
            Err(failure) => Reply::in_place(format!(
                "{}\nThe order was not withdrawn.",
                refused(&failure, &session)
            )),
        }
    }

    // ------------------------------------------------------------ an operator's day

    /// Asks which day an operator means, with a button for each day on offer.
    async fn ask_day(
        &self,
        sender: &Sender,
        question: &str,
        press: fn(NaiveDate) -> Press,
    ) -> Reply {
        match self.staff(sender).await {
            Ok(_) => Reply::new(question).with(days(sender.today, press)),
            Err(reply) => reply,
        }
    }

    async fn pending(&self, sender: &Sender, day: NaiveDate) -> Reply {
        let session = match self.staff(sender).await {
            Ok(session) => session,
            Err(reply) => return reply,
        };
        let waiting = OrderFilter {
            service_date: Some(day),
            status: Some(OrderStatus::Pending),
            ..OrderFilter::default()
        };
        let orders = match self.every_order(&caller(&session, sender), waiting).await {
            Ok(orders) => orders,
            Err(failure) => return Reply::in_place(refused(&failure, &session)),
        };
        if orders.is_empty() {
            return Reply::in_place(format!("{}: no order is pending.", name(day)));
        }
        let parcels: u64 = orders.iter().map(|order| u64::from(order.demand)).sum();
        let deliveries = orders
            .iter()
            .filter(|order| order.kind == OrderKind::Delivery)
            .count();
        let text = format!(
            "{}: {} pending, {parcels} parcels in all: {} and {}.",
            name(day),
            if orders.len() == 1 {
                "1 order is".to_owned()
            } else {
                format!("{} orders are", orders.len())
            },
            some(deliveries, "delivery", "deliveries"),
            some(orders.len() - deliveries, "pickup", "pickups"),
        );
        Reply::in_place(text).with(vec![key(format!("Plan {}", name(day)), Press::Plan(day))])
    }

    /// Asks whether to plan a day. Nothing is planned here.
    async fn ask_to_plan(&self, sender: &Sender, day: NaiveDate) -> Reply {
        if let Err(reply) = self.staff(sender).await {
            return reply;
        }
        if day < sender.today {
            return Reply::in_place(format!(
                "{} has passed. /plan asks for a day again.",
                name(day)
            ));
        }
        Reply::in_place(format!(
            "Plan {}?\nIts pending orders are given to the couriers, and a route is searched \
             for each of them.",
            name(day)
        ))
        .with(vec![
            key("Yes, plan it", Press::PlanYes(day)),
            key("No", Press::PlanNo),
        ])
    }

    async fn plan(&self, sender: &Sender, day: NaiveDate) -> Reply {
        let session = match self.staff(sender).await {
            Ok(session) => session,
            Err(reply) => return reply,
        };
        // The seed and the budget are left to the API, which gives a plan its own (D45).
        let asked = PlanRequest {
            service_date: day,
            seed: None,
            budget: None,
        };
        match self
            .api
            .create_plan(&caller(&session, sender), &asked)
            .await
        {
            Ok(plan) => {
                info!(plan_id = %plan.id, service_date = %day, "asked for a plan");
                Reply::in_place(format!(
                    "Asked for: the plan of {} is {}. /status says how far it is.",
                    name(day),
                    plan.status
                ))
            }
            Err(failure) => Reply::in_place(format!(
                "{}\nNothing was planned.",
                refused(&failure, &session)
            )),
        }
    }

    async fn status(&self, sender: &Sender, day: NaiveDate) -> Reply {
        let session = match self.staff(sender).await {
            Ok(session) => session,
            Err(reply) => return reply,
        };
        let text = match self.api.list_plans(&caller(&session, sender), day).await {
            // The newest is first, and is the one that counts: the others are over.
            Ok(plans) => match plans.first() {
                Some(plan) => format!("{}: {}", name(day), how_far(plan)),
                None => format!("{}: no plan has been asked for.", name(day)),
            },
            Err(failure) => refused(&failure, &session),
        };
        // The days stay under it. Pressing one again looks again.
        Reply::in_place(text).with(days(sender.today, Press::Status))
    }
}

/// How a request is made in the person's name.
fn caller<'a>(session: &'a Session, sender: &'a Sender) -> Caller<'a> {
    Caller {
        token: &session.token,
        request_id: &sender.request_id,
    }
}

fn is_staff(session: &Session) -> bool {
    matches!(session.user.role, Role::Operator | Role::Admin)
}

/// Whether a day is one the buttons offer: today, or one of the days after it.
fn on_offer(day: NaiveDate, today: NaiveDate) -> bool {
    let last = today.checked_add_days(Days::new(DAYS_AHEAD));
    day >= today && last.is_some_and(|last| day <= last)
}

/// A row for each day on offer, which says the day it means: nobody has to wonder
/// what the bot's clock takes for tomorrow.
fn days(today: NaiveDate, press: impl Fn(NaiveDate) -> Press) -> Vec<Vec<Key>> {
    (0..=DAYS_AHEAD)
        .filter_map(|ahead| Some((ahead, today.checked_add_days(Days::new(ahead))?)))
        .map(|(ahead, day)| {
            let label = match ahead {
                0 => format!("Today, {}", name(day)),
                1 => format!("Tomorrow, {}", name(day)),
                _ => name(day),
            };
            key(label, press(day))
        })
        .collect()
}

/// A day as a person says it: `Tue 13 Oct`.
fn name(day: NaiveDate) -> String {
    day.format("%a %-d %b").to_string()
}

/// Two numbers with a space or a comma between them: a latitude and a longitude.
fn coordinates(text: &str) -> Option<(f64, f64)> {
    let mut numbers = text
        .split(|letter: char| letter == ',' || letter == ';' || letter.is_whitespace())
        .filter(|part| !part.is_empty())
        .map(str::parse::<f64>);
    let (lat, lon) = (numbers.next()?.ok()?, numbers.next()?.ok()?);
    numbers.next().is_none().then_some((lat, lon))
}

/// A place as a person reads it: its address, if it has one, and where it is.
fn place(location: LatLon, address: Option<&str>) -> String {
    let position = format!("{:.5}, {:.5}", location.lat(), location.lon());
    match address {
        Some(address) => format!("{address} ({position})"),
        None => position,
    }
}

/// Which way the parcels go: `a delivery to`, `a pickup from`.
fn way(kind: OrderKind) -> &'static str {
    match kind {
        OrderKind::Delivery => "a delivery to",
        OrderKind::Pickup => "a pickup from",
    }
}

/// An order in words, without its day: `a delivery of 2 parcels to 50.45000, 30.52000`.
fn order(kind: OrderKind, parcels: u32, place: &str) -> String {
    let (what, to) = match kind {
        OrderKind::Delivery => ("a delivery", "to"),
        OrderKind::Pickup => ("a pickup", "from"),
    };
    let parcels = some(parcels as usize, "parcel", "parcels");
    format!("{what} of {parcels} {to} {place}")
}

/// An order of the API's in words, with its day.
fn described(stored: &Order) -> String {
    format!(
        "{}, on {}",
        order(
            stored.kind,
            stored.demand,
            &place(stored.location, stored.address.as_deref())
        ),
        name(stored.service_date)
    )
}

fn standing(status: OrderStatus) -> &'static str {
    match status {
        OrderStatus::Pending => "It is pending: no plan has given it to a courier yet.",
        OrderStatus::Planned => "It is planned: a courier has it on a route.",
        OrderStatus::Cancelled => "It was withdrawn.",
    }
}

/// How far a plan has got, in a sentence that follows the day and a colon.
fn how_far(plan: &Plan) -> String {
    let unassigned = match plan.orders_unassigned {
        0 => String::new(),
        1 => " 1 order fits no courier and is still pending.".to_owned(),
        left => format!(" {left} orders fit no courier and are still pending."),
    };
    let routed = plan.orders_total.saturating_sub(plan.orders_unassigned) as usize;
    match plan.status {
        PlanStatus::Queued => "the plan is queued, and waits for a solver.".to_owned(),
        PlanStatus::Routing => format!(
            "the plan is being routed: {} of {} stored, for {}.{unassigned}",
            plan.routes_done,
            some(plan.routes_total as usize, "route", "routes"),
            some(routed, "order", "orders")
        ),
        PlanStatus::Done => format!(
            "the plan is done: {} for {}.{unassigned}",
            some(plan.routes_total as usize, "route", "routes"),
            some(routed, "order", "orders")
        ),
        PlanStatus::Failed => format!(
            "the plan failed. {}",
            sentence(plan.error.as_deref().unwrap_or("no reason was stored"))
        ),
        PlanStatus::Cancelled => "the plan was cancelled. Its orders are pending again.".to_owned(),
        PlanStatus::Discarded => {
            "the plan was discarded. Its orders are pending again, and the day can be planned afresh."
                .to_owned()
        }
    }
}

/// A sentence as it goes on after a colon: without the capital at its start.
fn lower_first(words: &str) -> String {
    let mut letters = words.chars();
    match letters.next() {
        Some(first) => first.to_lowercase().chain(letters).collect(),
        None => String::new(),
    }
}

/// A count with its noun: `no orders`, `1 order`, `7 orders`.
fn some(count: usize, one: &str, many: &str) -> String {
    match count {
        0 => format!("no {many}"),
        1 => format!("1 {one}"),
        more => format!("{more} {many}"),
    }
}

/// A role with its article: `an operator`.
fn a(role: Role) -> &'static str {
    match role {
        Role::Client => "a client",
        Role::Operator => "an operator",
        Role::Admin => "an admin",
    }
}

/// What the API said, as a sentence: with a capital at its start and a stop at its
/// end. The API writes its details for a person, and without either (D39).
fn sentence(words: &str) -> String {
    let mut letters = words.trim().chars();
    let Some(first) = letters.next() else {
        return String::new();
    };
    let mut sentence: String = first.to_uppercase().chain(letters).collect();
    if !sentence.ends_with(['.', '!', '?']) {
        sentence.push('.');
    }
    sentence
}

/// What to tell somebody whom the API refused, or did not answer.
fn refused(failure: &Failure, session: &Session) -> String {
    match failure {
        Failure::Away(_) => {
            warn!(%failure, "the API did not answer");
            AWAY.to_owned()
        }
        // The bot took them for an operator and the API did not: their role was
        // changed between the bot's two requests.
        Failure::Refused { status: 403, .. } if is_staff(session) => {
            "The service says that your role does not allow this.".to_owned()
        }
        Failure::Refused { status: 403, .. } => not_for(session),
        // The token is good for a minute, and this came after it. It will not again.
        Failure::Refused { status: 401, .. } => "That took too long. Try it again.".to_owned(),
        Failure::Refused { status, .. } if *status >= 500 => {
            warn!(%failure, "the API failed");
            "Something went wrong at the service. Try again in a moment.".to_owned()
        }
        Failure::Refused { detail, .. } => sentence(detail),
    }
}

/// What to tell somebody the bot could get no token for.
fn no_session(failure: &Failure) -> String {
    match failure {
        Failure::Away(_) => {
            warn!(%failure, "the API did not answer");
            AWAY.to_owned()
        }
        // The API will not have the bot: its key is not the API's, or the API has
        // none. That is for whoever runs the two to put right, and for the log.
        Failure::Refused { .. } => {
            error!(%failure, "the API gave the bot no token: is BOT_SERVICE_KEY the one the API has?");
            "The bot is not set up to reach the service. Whoever runs it has to put that right."
                .to_owned()
        }
    }
}

/// What to tell somebody who asked for what only an operator may.
fn not_for(session: &Session) -> String {
    format!(
        "That is for operators, and you are {} here. An admin can make your account an \
         operator's: it is {}.",
        a(session.user.role),
        session.user.id
    )
}

#[cfg(test)]
mod tests;
