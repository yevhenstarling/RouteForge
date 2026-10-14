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
//! Four things hold throughout:
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
//! - **An order is placed once, however often it is asked for (D76).** A draft has a
//!   key, which goes to the API with the order. When no answer comes back, the draft
//!   stays, with its key, and the next press asks for the same order again: the API
//!   places it if it had not, and answers with it either way.
//!
//! The dialogue is in five files. This one has its types, how an event is handed on,
//! and who is asking. `draft` puts an order together and places it, `search` finds a
//! place in words, `orders` shows a person their own orders and withdraws one, and
//! `operator` has what an operator does with a day.

mod draft;
mod operator;
mod orders;
mod search;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use chrono::{Days, NaiveDate};
use routeforge_domain::{
    IdempotencyKey, LatLon, Order, OrderFilter, OrderId, OrderKind, OrderRequest, OrderStatus,
    Plan, PlanRequest, PlanStatus, Role, Session,
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
/// What is said of an order that went out and got no answer. It may be placed or not,
/// and asking again settles which, without placing it twice (D76).
const NO_ANSWER: &str = "The service did not answer, so I cannot say yet whether the order \
    was placed. Press the button again: it is placed once, however often you ask.";

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
    /// What the order is asked for with, so that it is placed once (D76). It is the
    /// key of the draft as it is: a draft that is changed gets another, and one that
    /// is only asked for again keeps the one it has.
    key: IdempotencyKey,
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
