//! The dialogue, step by step, against what stands in for the API.
//!
//! A test says what a person sends and presses the buttons the bot made, by their
//! labels, as a person does. What the bot asked of the API meanwhile is in the fake.

use std::sync::atomic::AtomicU32;

use routeforge_domain::{PlanId, UserId};

use super::*;
use crate::testing::{Asked, FakeApi, a_delivery, a_plan, an_order, date};

/// The one person who writes to the bot, and their chat.
const ADA: i64 = 42;

/// The day it is in a test unless it says otherwise: Monday, 12 October 2026.
const TODAY: u32 = 12;

struct Bot {
    chats: Chats<FakeApi>,
    api: FakeApi,
    /// How many updates the bot has had: each has a request id of its own.
    updates: AtomicU32,
}

fn bot() -> Bot {
    let api = FakeApi::new(ADA);
    Bot {
        chats: Chats::new(api.clone()),
        api,
        updates: AtomicU32::new(0),
    }
}

impl Bot {
    /// Who sends the next update: Ada, on a day, with a message of this id.
    fn sender_on(&self, day: u32, message: i32) -> Sender {
        let update = self.updates.fetch_add(1, Ordering::Relaxed) + 1;
        Sender {
            chat: ADA,
            telegram_id: ADA,
            message,
            today: date(day),
            request_id: format!("tg-{update}"),
        }
    }

    /// The id the requests of the last update carried.
    fn last_request(&self) -> String {
        format!("tg-{}", self.updates.load(Ordering::Relaxed))
    }

    async fn says(&self, message: i32, text: &str) -> Reply {
        let sender = self.sender_on(TODAY, message);
        self.chats
            .handle(&sender, Event::Text(text.to_owned()))
            .await
    }

    async fn shares(&self, message: i32, lat: f64, lon: f64) -> Reply {
        let place = Place {
            lat,
            lon,
            address: None,
        };
        let sender = self.sender_on(TODAY, message);
        self.chats.handle(&sender, Event::Place(place)).await
    }

    /// Presses the button with this label under a reply of the bot's.
    async fn presses(&self, under: &Reply, label: &str) -> Reply {
        self.presses_on(TODAY, under, label).await
    }

    async fn presses_on(&self, day: u32, under: &Reply, label: &str) -> Reply {
        self.sends_on(day, &data_of(under, label)).await
    }

    /// Sends what a button would carry, be there such a button or not.
    async fn sends(&self, data: &str) -> Reply {
        self.sends_on(TODAY, data).await
    }

    async fn sends_on(&self, day: u32, data: &str) -> Reply {
        let sender = self.sender_on(day, 100);
        self.chats
            .handle(&sender, Event::Pressed(data.to_owned()))
            .await
    }

    /// An order at a place, with everything said of it but the confirmation.
    async fn whole_order(&self) -> Reply {
        let mut asked = self.shares(10, 50.4501, 30.5234).await;
        for label in ["Delivery", "2", "Tomorrow, Tue 13 Oct"] {
            asked = self.presses(&asked, label).await;
        }
        asked
    }
}

/// The labels of the buttons under a reply, row by row.
fn labels(reply: &Reply) -> Vec<Vec<&str>> {
    match &reply.keys {
        Keys::Under(rows) => rows
            .iter()
            .map(|row| row.iter().map(|key| key.label.as_str()).collect())
            .collect(),
        _ => Vec::new(),
    }
}

/// What the button with this label carries, as Telegram would hand it back.
fn data_of(reply: &Reply, label: &str) -> String {
    let Keys::Under(rows) = &reply.keys else {
        panic!("there are no buttons under {reply:?}");
    };
    let key = rows
        .iter()
        .flatten()
        .find(|key| key.label == label)
        .unwrap_or_else(|| panic!("no button `{label}` among {:?}", labels(reply)));
    let data = key.press.data();
    assert!(data.len() <= 64, "{data} is more than a button carries");
    assert_eq!(Press::read(&data), Some(key.press), "{data}");
    data
}

fn only_logins(asked: &[Asked]) -> bool {
    asked
        .iter()
        .all(|asked| matches!(asked, Asked::Login { .. }))
}

/// The "Done when" of the phase: a location, a delivery of two parcels for tomorrow,
/// and a confirmation.
#[tokio::test]
async fn a_delivery_of_two_parcels_for_tomorrow_is_placed_by_the_last_press_and_by_no_other() {
    let bot = bot();

    let asked = bot.shares(10, 50.4501, 30.5234).await;
    assert_eq!(
        asked.text,
        "An order at 50.45010, 30.52340.\nIs it a delivery to there, or a pickup from there?"
    );
    assert_eq!(labels(&asked), [vec!["Delivery", "Pickup"], vec!["Cancel"]]);
    assert!(!asked.in_place, "a message of its own, under the location");

    let asked = bot.presses(&asked, "Delivery").await;
    assert_eq!(
        asked.text,
        "A delivery to 50.45010, 30.52340.\nHow many parcels?"
    );
    assert_eq!(
        labels(&asked),
        [vec!["1", "2", "3", "4", "5"], vec!["Cancel"]]
    );
    assert!(asked.in_place, "the question changes where it stands");

    let asked = bot.presses(&asked, "2").await;
    assert_eq!(
        asked.text,
        "A delivery of 2 parcels to 50.45010, 30.52340.\nOn which day?"
    );
    assert_eq!(
        labels(&asked),
        [
            vec!["Today, Mon 12 Oct"],
            vec!["Tomorrow, Tue 13 Oct"],
            vec!["Wed 14 Oct"],
            vec!["Cancel"]
        ]
    );

    let shown = bot.presses(&asked, "Tomorrow, Tue 13 Oct").await;
    assert_eq!(
        shown.text,
        "A delivery of 2 parcels to 50.45010, 30.52340, on Tue 13 Oct.\nPlace this order?"
    );
    assert_eq!(labels(&shown), [vec!["Place the order"], vec!["Cancel"]]);

    // All of that was between the person and the bot. The API has heard nothing.
    assert_eq!(bot.api.asked(), []);

    let placed = bot.presses(&shown, "Place the order").await;
    assert_eq!(
        placed.text,
        "Placed: a delivery of 2 parcels to 50.45010, 30.52340, on Tue 13 Oct.\n\
         It is pending until that day is planned. /orders shows it."
    );
    assert_eq!(placed.keys, Keys::None);
    assert!(placed.in_place);

    // Asked as the person, with a token that was got for it, under the id of the
    // update that confirmed.
    let order = OrderRequest {
        kind: OrderKind::Delivery,
        service_date: date(13),
        location: LatLon::new(50.4501, 30.5234).unwrap(),
        address: None,
        demand: 2,
    };
    assert_eq!(
        bot.api.asked(),
        [
            Asked::Login {
                telegram_id: ADA,
                request_id: bot.last_request(),
            },
            Asked::CreateOrder {
                token: "token-1".to_owned(),
                request_id: bot.last_request(),
                order,
            },
        ]
    );
    assert_eq!(bot.api.orders().len(), 1);

    // The button went with the message it was under. Pressed all the same, by a
    // second tap that was already on its way, it places nothing.
    let again = bot.presses(&shown, "Place the order").await;
    assert_eq!((again.text.as_str(), again.in_place), (STALE, true));
    assert_eq!(bot.api.orders().len(), 1);
}

#[tokio::test]
async fn a_place_with_an_address_is_shown_by_it_and_ordered_with_it() {
    let bot = bot();
    let market = Place {
        lat: 50.442,
        lon: 30.522,
        address: Some("  Besarabsky Market, Besarabska Square, 2 ".to_owned()),
    };
    let at = "Besarabsky Market, Besarabska Square, 2 (50.44200, 30.52200)";

    let sender = bot.sender_on(TODAY, 10);
    let asked = bot.chats.handle(&sender, Event::Place(market)).await;
    assert_eq!(
        asked.text,
        format!("An order at {at}.\nIs it a delivery to there, or a pickup from there?")
    );
    let asked = bot.presses(&asked, "Pickup").await;
    assert_eq!(
        asked.text,
        format!("A pickup from {at}.\nHow many parcels?")
    );
    let asked = bot.presses(&asked, "1").await;
    assert_eq!(
        asked.text,
        format!("A pickup of 1 parcel from {at}.\nOn which day?")
    );
    let shown = bot.presses(&asked, "Today, Mon 12 Oct").await;
    assert_eq!(
        shown.text,
        format!("A pickup of 1 parcel from {at}, on Mon 12 Oct.\nPlace this order?")
    );
    let placed = bot.presses(&shown, "Place the order").await;
    assert_eq!(
        placed.text,
        format!(
            "Placed: a pickup of 1 parcel from {at}, on Mon 12 Oct.\n\
             It is pending until that day is planned. /orders shows it."
        )
    );

    let stored = &bot.api.orders()[0];
    assert_eq!(
        (stored.kind, stored.demand, stored.service_date),
        (OrderKind::Pickup, 1, date(12))
    );
    assert_eq!(
        stored.address.as_deref(),
        Some("Besarabsky Market, Besarabska Square, 2")
    );

    // An address of nothing is none, and one longer than the API takes is cut to
    // what it takes.
    for (address, kept) in [("   ".to_owned(), 0), ("я".repeat(400), 300)] {
        let bot = self::bot();
        let place = Place {
            lat: 50.442,
            lon: 30.522,
            address: Some(address),
        };
        let sender = bot.sender_on(TODAY, 10);
        let mut asked = bot.chats.handle(&sender, Event::Place(place)).await;
        for label in ["Delivery", "1", "Today, Mon 12 Oct", "Place the order"] {
            asked = bot.presses(&asked, label).await;
        }
        let stored = &bot.api.orders()[0];
        let length = stored.address.as_ref().map_or(0, |a| a.chars().count());
        assert_eq!(length, kept);
    }
}

#[tokio::test]
async fn coordinates_that_are_typed_begin_an_order_as_a_shared_location_does() {
    let bot = bot();
    let shared = bot.shares(10, 50.4501, 30.5234).await;
    for typed in [
        "/order 50.4501 30.5234",
        "/order 50.4501, 30.5234",
        "/order   50.4501;30.5234  ",
        "/ORDER@RouteForgeBot 50.4501 30.5234",
    ] {
        assert_eq!(bot.says(10, typed).await, shared, "{typed}");
    }
    // And it goes on as any other does.
    let asked = bot.presses(&shared, "Pickup").await;
    assert!(asked.text.ends_with("How many parcels?"), "{}", asked.text);

    for unread in [
        "/order here",
        "/order 50.4501",
        "/order 50.4501 30.5234 7",
        "/order 50,4501 30,5234",
        "/order north east",
    ] {
        let hint = bot.says(11, unread).await;
        assert!(
            hint.text.contains("a latitude and a longitude"),
            "{unread}: {}",
            hint.text
        );
        assert_eq!(hint.keys, Keys::None);
    }
    for nowhere in [
        "/order 95 30",
        "/order 50 181",
        "/order NaN 30",
        "/order 50 inf",
    ] {
        let refused = bot.says(12, nowhere).await;
        assert!(
            refused.text.contains("no position on Earth"),
            "{nowhere}: {}",
            refused.text
        );
    }
    // None of those took the place of the order that was begun.
    assert!(
        bot.presses(&asked, "1")
            .await
            .text
            .ends_with("On which day?")
    );

    // Without coordinates the command asks for the place, and offers the button.
    let how = bot.says(13, "/order").await;
    assert!(how.text.contains("/order 50.4501 30.5234"), "{}", how.text);
    assert_eq!(how.keys, Keys::ShareLocation);

    assert_eq!(bot.api.asked(), []);
}

#[tokio::test]
async fn an_order_is_dropped_at_any_step_and_nothing_is_placed() {
    for steps in 0..=3 {
        let bot = bot();
        let mut asked = bot.shares(10, 50.45, 30.52).await;
        for label in ["Pickup", "3", "Today, Mon 12 Oct"].iter().take(steps) {
            asked = bot.presses(&asked, label).await;
        }

        let dropped = bot.presses(&asked, "Cancel").await;
        assert_eq!(dropped.text, "Dropped: nothing was placed.", "{steps}");
        assert_eq!((&dropped.keys, dropped.in_place), (&Keys::None, true));
        // And it is gone.
        assert_eq!(bot.presses(&asked, "Cancel").await.text, STALE);
        assert_eq!(bot.api.asked(), []);
    }

    // The command drops it too, whatever message its buttons are under.
    let bot = bot();
    let asked = bot.shares(10, 50.45, 30.52).await;
    let dropped = bot.says(11, "/cancel").await;
    assert_eq!(dropped.text, "Dropped: nothing was placed.");
    assert_eq!(bot.presses(&asked, "Delivery").await.text, STALE);
    let nothing = bot.says(12, "/cancel").await;
    assert!(nothing.text.contains("nothing to drop"), "{}", nothing.text);
    assert_eq!(bot.api.asked(), []);
}

#[tokio::test]
async fn a_button_of_an_order_that_is_no_longer_open_is_told_so() {
    let bot = bot();
    let first = bot.shares(10, 50.45, 30.52).await;
    // A second place, before the first order was finished: the first gives way.
    let second = bot.shares(20, 50.40, 30.60).await;

    let late = bot.presses(&first, "Delivery").await;
    assert_eq!((late.text.as_str(), late.in_place), (STALE, true));
    assert_eq!(late.keys, Keys::None, "its buttons go with it");

    // The second is as it was.
    let asked = bot.presses(&second, "Pickup").await;
    assert_eq!(
        asked.text,
        "A pickup from 50.40000, 30.60000.\nHow many parcels?"
    );

    // What one chat is putting together is nobody else's, whatever they send.
    let stranger = Sender {
        chat: 77,
        telegram_id: 77,
        ..bot.sender_on(TODAY, 100)
    };
    let pressed = Event::Pressed(data_of(&asked, "1"));
    assert_eq!(bot.chats.handle(&stranger, pressed).await.text, STALE);
    assert!(
        bot.presses(&asked, "1")
            .await
            .text
            .ends_with("On which day?")
    );

    // A bot that was started again has forgotten what was being put together.
    let restarted = self::bot();
    assert_eq!(restarted.presses(&asked, "1").await.text, STALE);
    assert_eq!(bot.api.asked(), []);
}

/// The buttons name the day they mean. One that was left overnight means a day that
/// is over.
#[tokio::test]
async fn a_day_that_has_passed_since_it_was_offered_is_not_taken() {
    let bot = bot();
    let mut days = bot.shares(10, 50.4501, 30.5234).await;
    for label in ["Delivery", "2"] {
        days = bot.presses(&days, label).await;
    }

    // Offered on the 12th, pressed on the 13th.
    let again = bot.presses_on(13, &days, "Today, Mon 12 Oct").await;
    assert_eq!(
        again.text,
        "That day is no longer on offer.\n\n\
         A delivery of 2 parcels to 50.45010, 30.52340.\nOn which day?"
    );
    assert_eq!(
        labels(&again),
        [
            vec!["Today, Tue 13 Oct"],
            vec!["Tomorrow, Wed 14 Oct"],
            vec!["Thu 15 Oct"],
            vec!["Cancel"]
        ]
    );
    // What was tomorrow is today now, and is still a day to order for.
    let shown = bot.presses_on(13, &days, "Tomorrow, Tue 13 Oct").await;
    assert!(
        shown.text.ends_with("on Tue 13 Oct.\nPlace this order?"),
        "{}",
        shown.text
    );

    // The same for an order that was left unconfirmed until its day was over.
    let late = bot.presses_on(14, &shown, "Place the order").await;
    assert!(
        late.text.starts_with("That day is no longer on offer.\n\n"),
        "{}",
        late.text
    );
    assert_eq!(labels(&late)[0], ["Today, Wed 14 Oct"]);
    // And for a day that no button ever offered.
    let far = bot.sends_on(14, &Press::Day(10, date(31)).data()).await;
    assert!(far.text.starts_with("That day is no longer on offer."));

    assert_eq!(bot.api.asked(), [], "nothing was placed");
    // It is still there to be finished.
    let shown = bot.presses_on(14, &late, "Today, Wed 14 Oct").await;
    let placed = bot.presses_on(14, &shown, "Place the order").await;
    assert!(placed.text.starts_with("Placed:"), "{}", placed.text);
    assert_eq!(bot.api.orders()[0].service_date, date(14));
}

/// What a button carries comes back from Telegram, and whoever is at the other end
/// can send anything in its place.
#[tokio::test]
async fn what_no_button_of_the_bots_carries_is_not_taken_for_one() {
    let bot = bot();
    let asked = bot.shares(10, 50.45, 30.52).await;
    for made_up in [
        "",
        "x",
        "k",
        "k:10",
        "k:10:teleport",
        "k:ten:delivery",
        "k:10:delivery:again",
        "n:10:two",
        "n:10:-1",
        "d:10:2026-13-01",
        "ok",
        "ok:ten",
        "o:not-an-id",
        "pn:1",
        "DROP TABLE orders",
    ] {
        let reply = bot.sends(made_up).await;
        assert!(
            reply.text.contains("not one I know"),
            "{made_up:?}: {}",
            reply.text
        );
    }

    // A number of parcels that no button offers is asked for again.
    let asked = bot.presses(&asked, "Delivery").await;
    for parcels in [0, 6, u32::MAX] {
        let again = bot.sends(&Press::Parcels(10, parcels).data()).await;
        assert_eq!(again.text, asked.text, "{parcels}");
    }
    // And the confirmation of an order that is not whole confirms nothing.
    let early = bot.sends(&Press::Place(10).data()).await;
    assert_eq!(early.text, asked.text);

    assert_eq!(bot.api.asked(), []);
    assert!(
        bot.presses(&asked, "5")
            .await
            .text
            .ends_with("On which day?")
    );
}

#[test]
fn every_button_carries_what_it_means_within_what_telegram_lets_it() {
    let order = OrderId::generate();
    let day = date(31);
    for press in [
        Press::Kind(i32::MIN, OrderKind::Delivery),
        Press::Kind(i32::MAX, OrderKind::Pickup),
        Press::Parcels(i32::MIN, u32::MAX),
        Press::Day(i32::MIN, day),
        Press::Place(i32::MIN),
        Press::Drop(-1),
        Press::Show(order),
        Press::Withdraw(order),
        Press::WithdrawYes(order),
        Press::Keep(order),
        Press::Pending(day),
        Press::Plan(day),
        Press::PlanYes(day),
        Press::PlanNo,
        Press::Status(day),
    ] {
        let data = press.data();
        assert!(data.len() <= 64, "{data}");
        assert_eq!(Press::read(&data), Some(press), "{data}");
    }
}

#[test]
fn a_command_is_read_whatever_its_case_and_whoever_it_names() {
    assert_eq!(Command::read("/orders"), Some((Command::Orders, "")));
    assert_eq!(Command::read("  /Orders  "), Some((Command::Orders, "")));
    assert_eq!(
        Command::read("/order@RouteForgeBot 50.45 30.52"),
        Some((Command::Order, "50.45 30.52"))
    );
    assert_eq!(
        Command::read("/order\n 50.45\t30.52 "),
        Some((Command::Order, "50.45\t30.52"))
    );
    for none in [
        "",
        "orders",
        "/",
        "/ orders",
        "/orderss",
        "/@orders",
        "see /orders",
    ] {
        assert_eq!(Command::read(none), None, "{none:?}");
    }

    // Every command is read by its own name, and Telegram takes it for its menu:
    // lower-case letters, and a few words on what it is for.
    for (command, name, says) in COMMANDS {
        assert_eq!(Command::read(&format!("/{name}")), Some((*command, "")));
        assert!(name.bytes().all(|byte| byte.is_ascii_lowercase()), "{name}");
        assert!((1..=32).contains(&name.len()), "{name}");
        assert!((3..=256).contains(&says.len()), "{says}");
    }
}

/// The bot takes no free text (D58), and says what it does take.
#[tokio::test]
async fn what_is_no_place_and_no_command_is_told_what_the_bot_takes() {
    let bot = bot();
    for said in [
        "two parcels to Khreshchatyk, tomorrow",
        "orders",
        "/fly",
        "/",
        "see /orders",
    ] {
        assert_eq!(bot.says(1, said).await.text, NOT_UNDERSTOOD, "{said}");
    }
    let sender = bot.sender_on(TODAY, 2);
    let reply = bot.chats.handle(&sender, Event::Other).await;
    assert_eq!(
        (reply.text.as_str(), &reply.keys, reply.in_place),
        (NOT_UNDERSTOOD, &Keys::None, false)
    );
    assert_eq!(bot.api.asked(), []);
}

#[tokio::test]
async fn help_says_what_the_person_may_do_and_start_what_this_is() {
    let bot = bot();
    let me = bot.api.user().id;

    let help = bot.says(1, "/help").await;
    assert_eq!(help.keys, Keys::ShareLocation);
    for said in [
        "/order 50.4501 30.5234",
        "/orders",
        "/cancel",
        "You are a client here",
        &me.to_string(),
    ] {
        assert!(help.text.contains(said), "{said}: {}", help.text);
    }
    assert!(!help.text.contains("/plan"), "{}", help.text);

    let start = bot.says(2, "/start").await;
    assert!(start.text.starts_with("RouteForge takes parcel orders"));
    assert!(start.text.ends_with(&help.text), "{}", start.text);
    assert_eq!(start.keys, Keys::ShareLocation);

    // What an admin has made of the account shows the next time it is asked.
    for (role, named) in [(Role::Operator, "an operator"), (Role::Admin, "an admin")] {
        bot.api.make(role);
        let help = bot.says(3, "/help").await;
        for said in [named, "/pending", "/plan", "/status", "/orders"] {
            assert!(help.text.contains(said), "{said}: {}", help.text);
        }
    }

    // With the API away it says what is true for everybody, and that it is away.
    bot.api
        .fail_login(Failure::Away("connection refused".to_owned()));
    let help = bot.says(4, "/help").await;
    assert!(help.text.contains("/orders") && help.text.ends_with(AWAY));
    assert!(!help.text.contains("/plan"), "{}", help.text);

    assert!(only_logins(&bot.api.asked()));
}

#[tokio::test]
async fn the_orders_of_a_person_are_listed_from_today_on_the_nearest_first() {
    let bot = bot();
    let me = bot.api.user().id;
    let none = bot.says(1, "/orders").await;
    assert!(none.text.contains("no orders for today or later"));
    assert_eq!(none.keys, Keys::None);

    // Placed in no order of days: one that is over, three to come, one withdrawn,
    // and somebody else's.
    bot.api.has(an_order(me, &a_delivery(date(11), 1)));
    let wednesday = bot.api.has(an_order(me, &a_delivery(date(14), 3)));
    let mut pickup = an_order(me, &a_delivery(date(12), 1));
    pickup.kind = OrderKind::Pickup;
    let today = bot.api.has(pickup);
    let mut withdrawn = an_order(me, &a_delivery(date(13), 2));
    withdrawn.status = OrderStatus::Cancelled;
    bot.api.has(withdrawn);
    let mut planned = an_order(me, &a_delivery(date(13), 2));
    planned.status = OrderStatus::Planned;
    planned.plan_id = Some(PlanId::generate());
    let tomorrow = bot.api.has(planned);
    bot.api
        .has(an_order(UserId::generate(), &a_delivery(date(12), 9)));

    bot.api.asked_since();
    let listed = bot.says(2, "/orders").await;
    assert_eq!(
        listed.text,
        "You have 3 orders for today or later. Press one to look at it, or to withdraw it."
    );
    assert_eq!(
        labels(&listed),
        [
            ["Mon 12 Oct: pickup of 1, pending"],
            ["Tue 13 Oct: delivery of 2, planned"],
            ["Wed 14 Oct: delivery of 3, pending"],
        ]
    );
    for (label, order) in [
        ("Mon 12 Oct: pickup of 1, pending", &today),
        ("Tue 13 Oct: delivery of 2, planned", &tomorrow),
        ("Wed 14 Oct: delivery of 3, pending", &wednesday),
    ] {
        assert_eq!(data_of(&listed, label), Press::Show(order.id).data());
    }
    assert!(!listed.in_place);

    // Asked for as their own, a page of the API's at a time.
    let theirs = OrderFilter {
        client_id: Some(me),
        limit: Some(1_000),
        ..OrderFilter::default()
    };
    assert_eq!(
        bot.api.asked_since(),
        [
            Asked::Login {
                telegram_id: ADA,
                request_id: bot.last_request(),
            },
            Asked::ListOrders {
                token: bot.api.last_token(),
                filter: theirs,
            },
        ]
    );

    // An operator is shown their own as well, and not everybody's, which is what
    // the API would give them unasked.
    bot.api.make(Role::Operator);
    let listed = bot.says(3, "/orders").await;
    assert_eq!(labels(&listed).len(), 3, "{:?}", labels(&listed));

    // One alone is said as one.
    let lone = self::bot();
    lone.api
        .has(an_order(lone.api.user().id, &a_delivery(date(12), 1)));
    assert!(
        lone.says(1, "/orders")
            .await
            .text
            .starts_with("You have 1 order for today or later.")
    );
}

#[tokio::test]
async fn a_list_longer_than_a_page_is_read_a_page_at_a_time() {
    let bot = bot();
    let me = bot.api.user().id;
    let mut ids = Vec::new();
    for _ in 0..2_001 {
        ids.push(bot.api.has(an_order(me, &a_delivery(date(13), 1))).id);
    }

    let listed = bot.says(1, "/orders").await;
    assert!(
        listed
            .text
            .starts_with("You have 2001 orders for today or later. These are the first 10."),
        "{}",
        listed.text
    );
    assert_eq!(labels(&listed).len(), 10);
    // The oldest ten of the day, each by its own id.
    assert_eq!(
        data_of(&listed, "Tue 13 Oct: delivery of 1, pending"),
        Press::Show(ids[0]).data()
    );

    // Each page was asked for by the last order of the one before (D44), and the
    // page that came short was the last.
    let pages: Vec<Option<OrderId>> = bot
        .api
        .asked()
        .iter()
        .filter_map(|asked| match asked {
            Asked::ListOrders { filter, .. } => Some(filter.after),
            _ => None,
        })
        .collect();
    assert_eq!(pages, [None, Some(ids[999]), Some(ids[1_999])]);
}

#[tokio::test]
async fn an_order_is_withdrawn_by_the_press_that_says_yes_and_by_no_other() {
    let bot = bot();
    let order = bot
        .api
        .has(an_order(bot.api.user().id, &a_delivery(date(13), 2)));
    let cancels = |bot: &Bot| {
        let asked = bot.api.asked();
        let cancels = asked
            .iter()
            .filter(|asked| matches!(asked, Asked::CancelOrder { .. }));
        cancels.count()
    };

    let listed = bot.says(1, "/orders").await;
    let shown = bot
        .presses(&listed, "Tue 13 Oct: delivery of 2, pending")
        .await;
    assert_eq!(
        shown.text,
        format!(
            "A delivery of 2 parcels to 50.45000, 30.52000, on Tue 13 Oct.\n\
             It is pending: no plan has given it to a courier yet.\n\
             Order {}.",
            order.id
        )
    );
    assert_eq!(labels(&shown), [["Withdraw it"]]);
    assert!(!shown.in_place, "the list stays where it is");

    let asked = bot.presses(&shown, "Withdraw it").await;
    assert_eq!(
        asked.text,
        "Withdraw this order?\nA delivery of 2 parcels to 50.45000, 30.52000, on Tue 13 Oct."
    );
    assert_eq!(labels(&asked), [["Yes, withdraw it"], ["No, keep it"]]);
    assert!(asked.in_place);
    // It was asked, and that is all.
    assert_eq!(cancels(&bot), 0);

    let kept = bot.presses(&asked, "No, keep it").await;
    assert_eq!((&kept.text, &kept.keys), (&shown.text, &shown.keys));
    assert!(kept.in_place, "where the question stood");
    assert_eq!(cancels(&bot), 0);
    assert_eq!(bot.api.orders()[0].status, OrderStatus::Pending);

    let asked = bot.presses(&kept, "Withdraw it").await;
    bot.api.asked_since();
    let withdrawn = bot.presses(&asked, "Yes, withdraw it").await;
    assert_eq!(
        withdrawn.text,
        "Withdrawn: a delivery of 2 parcels to 50.45000, 30.52000, on Tue 13 Oct."
    );
    assert_eq!((&withdrawn.keys, withdrawn.in_place), (&Keys::None, true));
    assert!(matches!(
        bot.api.asked_since().as_slice(),
        [Asked::Login { .. }, Asked::CancelOrder { id, .. }] if *id == order.id
    ));
    assert_eq!(bot.api.orders()[0].status, OrderStatus::Cancelled);

    // Once. A second yes is the API's to refuse, and the bot says what it said.
    let again = bot.presses(&asked, "Yes, withdraw it").await;
    assert_eq!(
        again.text,
        "The order is cancelled and cannot be withdrawn.\nThe order was not withdrawn."
    );
    // And looked at again, it is shown as it is, with nothing left to press.
    let shown = bot.sends(&Press::Show(order.id).data()).await;
    assert!(shown.text.contains("It was withdrawn."), "{}", shown.text);
    assert_eq!(shown.keys, Keys::None);
}

#[tokio::test]
async fn an_order_a_plan_has_taken_is_shown_and_cannot_be_withdrawn() {
    let bot = bot();
    let mut planned = an_order(bot.api.user().id, &a_delivery(date(13), 2));
    planned.status = OrderStatus::Planned;
    planned.plan_id = Some(PlanId::generate());
    let order = bot.api.has(planned);

    let shown = bot.sends(&Press::Show(order.id).data()).await;
    assert!(
        shown
            .text
            .contains("It is planned: a courier has it on a route."),
        "{}",
        shown.text
    );
    assert_eq!(shown.keys, Keys::None);

    // A button from before the plan took it asks no question that has no answer:
    // it shows how the order stands now.
    let asked = bot.sends(&Press::Withdraw(order.id).data()).await;
    assert_eq!((&asked.text, &asked.keys), (&shown.text, &Keys::None));
    assert!(asked.in_place);
    // And a yes from before is refused by the API, in the API's words.
    let refused = bot.sends(&Press::WithdrawYes(order.id).data()).await;
    assert_eq!(
        refused.text,
        "A plan has given the order to a courier: it can no longer be withdrawn.\n\
         The order was not withdrawn."
    );
    assert_eq!(bot.api.orders()[0].status, OrderStatus::Planned);

    // An order that is not there, or is somebody else's, is not there (D39).
    let theirs = bot
        .api
        .has(an_order(UserId::generate(), &a_delivery(date(13), 1)));
    for id in [theirs.id, OrderId::generate()] {
        for press in [Press::Show(id), Press::Withdraw(id), Press::WithdrawYes(id)] {
            let missed = bot.sends(&press.data()).await;
            assert!(
                missed.text.starts_with("There is no such order."),
                "{press:?}: {}",
                missed.text
            );
        }
    }
}

#[tokio::test]
async fn an_operator_looks_at_a_day_and_plans_it_by_the_press_that_says_yes() {
    let bot = bot();
    bot.api.make(Role::Operator);
    let somebody = UserId::generate();
    for parcels in [2, 1, 3] {
        bot.api
            .has(an_order(somebody, &a_delivery(date(13), parcels)));
    }
    let mut pickup = an_order(somebody, &a_delivery(date(13), 1));
    pickup.kind = OrderKind::Pickup;
    bot.api.has(pickup);
    // Not pending, or not of that day.
    let mut withdrawn = an_order(somebody, &a_delivery(date(13), 5));
    withdrawn.status = OrderStatus::Cancelled;
    bot.api.has(withdrawn);
    bot.api.has(an_order(somebody, &a_delivery(date(12), 4)));
    let plans = |bot: &Bot| {
        let asked = bot.api.asked();
        let plans = asked
            .iter()
            .filter(|asked| matches!(asked, Asked::CreatePlan { .. }));
        plans.count()
    };

    let which = bot.says(1, "/pending").await;
    assert_eq!(which.text, "The pending orders of which day?");
    assert_eq!(
        labels(&which),
        [
            ["Today, Mon 12 Oct"],
            ["Tomorrow, Tue 13 Oct"],
            ["Wed 14 Oct"]
        ]
    );

    bot.api.asked_since();
    let pending = bot.presses(&which, "Tomorrow, Tue 13 Oct").await;
    assert_eq!(
        pending.text,
        "Tue 13 Oct: 4 orders are pending, 7 parcels in all: 3 deliveries and 1 pickup."
    );
    assert_eq!(labels(&pending), [["Plan Tue 13 Oct"]]);
    assert!(pending.in_place);
    // Everybody's, of that day, that are pending.
    let of_the_day = OrderFilter {
        service_date: Some(date(13)),
        status: Some(OrderStatus::Pending),
        limit: Some(1_000),
        ..OrderFilter::default()
    };
    assert!(matches!(
        bot.api.asked_since().as_slice(),
        [Asked::Login { .. }, Asked::ListOrders { filter, .. }] if *filter == of_the_day
    ));
    // A day with one, and a day with none.
    let one = bot.presses(&which, "Today, Mon 12 Oct").await;
    assert_eq!(
        one.text,
        "Mon 12 Oct: 1 order is pending, 4 parcels in all: 1 delivery and no pickups."
    );
    let none = bot.presses(&which, "Wed 14 Oct").await;
    assert_eq!(none.text, "Wed 14 Oct: no order is pending.");
    assert_eq!(none.keys, Keys::None);

    let asked = bot.presses(&pending, "Plan Tue 13 Oct").await;
    assert_eq!(
        asked.text,
        "Plan Tue 13 Oct?\nIts pending orders are given to the couriers, and a route is \
         searched for each of them."
    );
    assert_eq!(labels(&asked), [["Yes, plan it"], ["No"]]);
    assert!(asked.in_place);
    // It was asked, and that is all.
    assert_eq!(plans(&bot), 0);
    let no = bot.presses(&asked, "No").await;
    assert_eq!(
        (no.text.as_str(), no.in_place),
        ("Nothing was planned.", true)
    );
    assert_eq!(plans(&bot), 0);

    bot.api.asked_since();
    let planned = bot.presses(&asked, "Yes, plan it").await;
    assert_eq!(
        planned.text,
        "Asked for: the plan of Tue 13 Oct is queued. /status says how far it is."
    );
    assert_eq!((&planned.keys, planned.in_place), (&Keys::None, true));
    // The day and nothing else: the API gives a plan its seed and its budget (D45).
    // The id of the update goes with it, into the logs of the solvers (D42).
    assert_eq!(
        bot.api.asked_since(),
        [
            Asked::Login {
                telegram_id: ADA,
                request_id: bot.last_request(),
            },
            Asked::CreatePlan {
                token: bot.api.last_token(),
                request_id: bot.last_request(),
                plan: PlanRequest {
                    service_date: date(13),
                    seed: None,
                    budget: None,
                },
            },
        ]
    );

    // A second yes is the API's to refuse: the day has its plan in progress.
    let again = bot.presses(&asked, "Yes, plan it").await;
    assert_eq!(
        again.text,
        "The service date already has a plan in progress.\nNothing was planned."
    );
    assert_eq!(plans(&bot), 1, "it was asked for again");
    assert_eq!(bot.api.plans().len(), 1, "and stored once");

    // The command asks for the day itself, and goes on as the button did.
    let which = bot.says(2, "/plan").await;
    assert_eq!(which.text, "Plan which day?");
    let asked = bot.presses(&which, "Today, Mon 12 Oct").await;
    assert!(
        asked.text.starts_with("Plan Mon 12 Oct?\n"),
        "{}",
        asked.text
    );
    // A day that is over is not planned from a message that was left standing.
    let late = bot.presses_on(14, &which, "Tomorrow, Tue 13 Oct").await;
    assert_eq!(
        late.text,
        "Tue 13 Oct has passed. /plan asks for a day again."
    );
    assert_eq!(late.keys, Keys::None);
}

#[tokio::test]
async fn the_status_of_a_day_says_how_far_its_plan_is() {
    let bot = bot();
    bot.api.make(Role::Operator);
    let me = bot.api.user().id;

    let which = bot.says(1, "/status").await;
    assert_eq!(which.text, "The plan of which day?");
    bot.api.asked_since();
    let none = bot.presses(&which, "Tomorrow, Tue 13 Oct").await;
    assert_eq!(none.text, "Tue 13 Oct: no plan has been asked for.");
    // The days stay under it, to be pressed again.
    assert_eq!((&none.keys, none.in_place), (&which.keys, true));
    assert!(matches!(
        bot.api.asked_since().as_slice(),
        [Asked::Login { .. }, Asked::ListPlans { day, .. }] if *day == date(13)
    ));

    // The newest plan of the day is the one that is spoken of. Each of these is
    // newer than the one before.
    let queued = a_plan(date(13), me);
    let routing = Plan {
        status: PlanStatus::Routing,
        orders_total: 300,
        orders_unassigned: 2,
        routes_total: 4,
        routes_done: 3,
        ..a_plan(date(13), me)
    };
    let done = Plan {
        status: PlanStatus::Done,
        orders_total: 300,
        routes_total: 4,
        routes_done: 4,
        ..a_plan(date(13), me)
    };
    let one = Plan {
        status: PlanStatus::Done,
        orders_total: 3,
        orders_unassigned: 1,
        routes_total: 1,
        routes_done: 1,
        ..a_plan(date(13), me)
    };
    let failed = Plan {
        status: PlanStatus::Failed,
        error: Some("the search for a route panicked".to_owned()),
        ..a_plan(date(13), me)
    };
    let cancelled = Plan {
        status: PlanStatus::Cancelled,
        ..a_plan(date(13), me)
    };
    let discarded = Plan {
        status: PlanStatus::Discarded,
        ..a_plan(date(13), me)
    };
    for (plan, said) in [
        (queued, "the plan is queued, and waits for a solver."),
        (
            routing,
            "the plan is being routed: 3 of 4 routes stored, for 298 orders. \
             2 orders fit no courier and are still pending.",
        ),
        (done, "the plan is done: 4 routes for 300 orders."),
        (
            one,
            "the plan is done: 1 route for 2 orders. 1 order fits no courier and is still pending.",
        ),
        (failed, "the plan failed. The search for a route panicked."),
        (
            cancelled,
            "the plan was cancelled. Its orders are pending again.",
        ),
        (
            discarded,
            "the plan was discarded. Its orders are pending again, and the day can be planned \
             afresh.",
        ),
    ] {
        bot.api.has_plan(plan);
        let status = bot.presses(&which, "Tomorrow, Tue 13 Oct").await;
        assert_eq!(status.text, format!("Tue 13 Oct: {said}"));
        assert_eq!((&status.keys, status.in_place), (&which.keys, true));
    }
    // Another day has none of them.
    let other = bot.presses(&which, "Wed 14 Oct").await;
    assert_eq!(other.text, "Wed 14 Oct: no plan has been asked for.");
}

/// The API is what keeps a client from planning (D9). The bot says so sooner, and
/// says what the API says where it did not see it coming.
#[tokio::test]
async fn what_is_an_operators_is_refused_to_a_client() {
    let bot = bot();
    let me = bot.api.user().id;
    let not_theirs = format!(
        "That is for operators, and you are a client here. An admin can make your account an \
         operator's: it is {me}."
    );

    for command in ["/pending", "/plan", "/status"] {
        let refused = bot.says(1, command).await;
        assert_eq!(refused.text, not_theirs, "{command}");
        assert_eq!(refused.keys, Keys::None);
    }
    // A button from when they were an operator, or one they made up.
    for press in [
        Press::Pending(date(13)),
        Press::Plan(date(13)),
        Press::PlanYes(date(13)),
        Press::Status(date(13)),
    ] {
        assert_eq!(bot.sends(&press.data()).await.text, not_theirs, "{press:?}");
    }
    // Who they are is all that was asked of the API.
    assert!(only_logins(&bot.api.asked()));

    // An account that stops being an operator's between the bot's two requests is
    // refused by the API, and the bot passes that on.
    bot.api.make(Role::Operator);
    bot.api.fail_next(Failure::Refused {
        status: 403,
        detail: "your role does not allow this".to_owned(),
    });
    let refused = bot.sends(&Press::PlanYes(date(13)).data()).await;
    assert_eq!(
        refused.text,
        "The service says that your role does not allow this.\nNothing was planned."
    );
}

#[tokio::test]
async fn a_role_an_admin_has_changed_applies_to_the_next_thing_that_is_asked() {
    let bot = bot();
    assert!(
        bot.says(1, "/plan")
            .await
            .text
            .starts_with("That is for operators")
    );

    bot.api.make(Role::Operator);
    let which = bot.says(2, "/plan").await;
    assert_eq!(which.text, "Plan which day?");
    let asked = bot.presses(&which, "Tomorrow, Tue 13 Oct").await;

    bot.api.make(Role::Client);
    let refused = bot.presses(&asked, "Yes, plan it").await;
    assert!(refused.text.starts_with("That is for operators"));

    // Whatever was asked of the API was asked with the token of the login before
    // it: one for each update, and none kept from an earlier one.
    let asked = bot.api.asked();
    assert!(only_logins(&asked), "{asked:#?}");
    bot.api.make(Role::Operator);
    bot.presses(&which, "Today, Mon 12 Oct").await;
    bot.sends(&Press::PlanYes(date(12)).data()).await;
    bot.says(3, "/orders").await;
    let mut logins = 0;
    let mut others = 0;
    for asked in bot.api.asked() {
        let token = match asked {
            Asked::Login { .. } => {
                logins += 1;
                continue;
            }
            Asked::CreateOrder { token, .. }
            | Asked::ListOrders { token, .. }
            | Asked::GetOrder { token, .. }
            | Asked::CancelOrder { token, .. }
            | Asked::CreatePlan { token, .. }
            | Asked::ListPlans { token, .. } => token,
        };
        assert_eq!(token, format!("token-{logins}"));
        others += 1;
    }
    assert_eq!(others, 2, "a plan and a list of orders");
}

#[tokio::test]
async fn an_order_that_could_not_be_placed_is_kept_and_placed_when_the_api_answers() {
    let bot = bot();
    let shown = bot.whole_order().await;

    // No token to place it with: the API is away.
    bot.api
        .fail_login(Failure::Away("connection refused".to_owned()));
    let kept = bot.presses(&shown, "Place the order").await;
    assert_eq!(
        kept.text,
        format!("{AWAY}\nThe order was not placed.\n\n{}", shown.text)
    );
    assert_eq!((&kept.keys, kept.in_place), (&shown.keys, true));

    // The API refuses the order, and says why.
    bot.api.fail_next(Failure::Refused {
        status: 422,
        detail: "the request has fields that cannot be accepted; demand must be at least 1"
            .to_owned(),
    });
    let kept = bot.presses(&kept, "Place the order").await;
    assert_eq!(
        kept.text,
        format!(
            "The request has fields that cannot be accepted; demand must be at least 1.\n\
             The order was not placed.\n\n{}",
            shown.text
        )
    );
    // The API fails. What went wrong there is its own business (D39).
    bot.api.fail_next(Failure::Refused {
        status: 500,
        detail: "something went wrong on the server".to_owned(),
    });
    let kept = bot.presses(&kept, "Place the order").await;
    assert!(
        kept.text.starts_with(
            "Something went wrong at the service. Try again in a moment.\n\
             The order was not placed.\n\n"
        ),
        "{}",
        kept.text
    );
    assert!(bot.api.orders().is_empty());

    // The order went out and no answer came back. It may be stored all the same,
    // so the bot does not say that it is not.
    bot.api
        .fail_next(Failure::Away("the answer took too long".to_owned()));
    let unknown = bot.presses(&kept, "Place the order").await;
    assert!(
        unknown.text.starts_with(
            "The service did not answer, so I cannot say whether the order was placed."
        ),
        "{}",
        unknown.text
    );
    assert!(!unknown.text.contains("was not placed"), "{}", unknown.text);
    assert_eq!(unknown.keys, shown.keys);

    // And when the API answers, the order that was kept is placed, once.
    let placed = bot.presses(&unknown, "Place the order").await;
    assert!(placed.text.starts_with("Placed:"), "{}", placed.text);
    assert_eq!(bot.api.orders().len(), 1);
    assert_eq!(bot.presses(&unknown, "Place the order").await.text, STALE);
}

/// An order that is on its way to the API is no longer the chat's to change. If the
/// chat begins another meanwhile, that one stays when the first comes back refused.
#[tokio::test]
async fn an_order_that_comes_back_refused_does_not_take_the_place_of_a_newer_one() {
    let bot = bot();
    let shown = bot.whole_order().await;

    // What `place` does round its request: it takes the order out, and gives it
    // back when the API would not have it. Here another was begun in between.
    let on_its_way = bot.chats.drafts().remove(&ADA).unwrap();
    let newer = bot.shares(20, 50.40, 30.60).await;
    bot.chats.put_back(ADA, on_its_way);

    assert_eq!(bot.presses(&shown, "Place the order").await.text, STALE);
    assert!(
        bot.presses(&newer, "Pickup")
            .await
            .text
            .starts_with("A pickup from 50.40000, 30.60000.")
    );
}

#[tokio::test]
async fn when_the_api_will_not_have_the_bot_the_person_is_told_whom_to_tell() {
    let bot = bot();
    let shown = bot.whole_order().await;
    let unwelcome = Failure::Refused {
        status: 401,
        detail: "this is for the bot, which shows its key as `Authorization: Bearer <key>`"
            .to_owned(),
    };
    let not_set_up =
        "The bot is not set up to reach the service. Whoever runs it has to put that right.";

    bot.api.fail_login(unwelcome.clone());
    assert_eq!(bot.says(1, "/orders").await.text, not_set_up);
    bot.api.fail_login(unwelcome.clone());
    assert_eq!(bot.says(2, "/plan").await.text, not_set_up);
    bot.api.fail_login(unwelcome);
    let kept = bot.presses(&shown, "Place the order").await;
    assert_eq!(
        kept.text,
        format!("{not_set_up}\nThe order was not placed.\n\n{}", shown.text)
    );
    // What the API said of the key is for the log, and not for the chat.
    assert!(!kept.text.contains("Bearer"));
}

/// Somebody who begins an order and never ends it leaves a draft behind.
#[tokio::test]
async fn the_orders_that_are_being_put_together_are_not_kept_without_end() {
    let bot = bot();
    let chats = MOST_DRAFTS as i64 + 1;
    let of = |chat: i64| Sender {
        chat,
        telegram_id: chat,
        ..bot.sender_on(TODAY, 10)
    };
    let here = Place {
        lat: 50.45,
        lon: 30.52,
        address: None,
    };
    for chat in 1..=chats {
        bot.chats
            .handle(&of(chat), Event::Place(here.clone()))
            .await;
    }
    assert_eq!(bot.chats.drafts().len(), MOST_DRAFTS);

    // The oldest made room. The others are as they were.
    let delivery = || Event::Pressed(Press::Kind(10, OrderKind::Delivery).data());
    assert_eq!(bot.chats.handle(&of(1), delivery()).await.text, STALE);
    for chat in [2, chats] {
        let asked = bot.chats.handle(&of(chat), delivery()).await;
        assert!(asked.text.ends_with("How many parcels?"), "{chat}");
    }
    assert_eq!(bot.api.asked(), []);
}
