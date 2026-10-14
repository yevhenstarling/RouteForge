//! The whole bot, against a server in the place of Telegram and one in the place of
//! the API.
//!
//! This is as near to Telegram as a test gets. What a person does in a chat reaches
//! the bot as Telegram sends it, as updates, and what the bot says is read from the
//! requests it makes. The dialogue itself is tried in `chat`: here it is that the two
//! ends are joined up.

use std::collections::BTreeSet;
use std::time::Instant;

use chrono::Days;
use routeforge_domain::{OrderKind, OrderRequest, Role, Session, UserId};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::*;
use crate::chat::Press;
use crate::testing::{
    ADA, BOT_TOKEN, SENT, Telegram, a_depot, a_user, an_order, documented, message_update,
    message_update_in, press_update, soon, telegram_says_no,
};

const KEY: &str = "the key the bot shows, in the tests of the whole of it";
const TOKEN: &str = "the.token.of-ada";

/// A bot that waits for nothing longer than a test can.
const PACE: Pace = Pace {
    poll: Duration::from_secs(1),
    retry: Backoff::new(Duration::from_millis(20), Duration::from_millis(100)),
    stop_within: Duration::from_secs(5),
};

/// The bot at work, until it is told to stop.
struct Running {
    stop: watch::Sender<bool>,
    bot: JoinHandle<anyhow::Result<()>>,
}

fn start(telegram: &Telegram, api_url: &str) -> Running {
    start_with(telegram, api_url, None)
}

/// The same bot, given something to ask where an address is, or nothing (D72).
fn start_with(telegram: &Telegram, api_url: &str, geocoder: Option<Nominatim>) -> Running {
    let bot = bot(BOT_TOKEN, Some(telegram.url())).unwrap();
    let chats = Arc::new(Chats::new(HttpApi::new(api_url, KEY).unwrap(), geocoder));
    let (stop, stopping) = watch::channel(false);
    Running {
        stop,
        bot: tokio::spawn(run(bot, chats, PACE, stopping)),
    }
}

impl Running {
    /// Tells the bot to stop. Gives how it ended, and how long it took to.
    async fn stopped(self) -> (anyhow::Result<()>, Duration) {
        let told = Instant::now();
        self.stop.send_replace(true);
        let ended = soon(self.bot).await.unwrap();
        (ended, told.elapsed())
    }
}

/// Stores the order it is asked for, as the API does.
struct Stores(UserId);

impl Respond for Stores {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let asked: OrderRequest = request.body_json().unwrap();
        ResponseTemplate::new(201).set_body_json(an_order(self.0, &asked))
    }
}

/// An API that knows Ada for a client and takes her orders.
async fn an_api() -> MockServer {
    let api = MockServer::start().await;
    let ada = a_user(Role::Client, ADA);
    let session = Session {
        token: TOKEN.to_owned(),
        expires_in: 60,
        user: ada.clone(),
    };
    let (verb, at) = documented("telegram_login");
    Mock::given(method(verb.as_str()))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_json(session))
        .mount(&api)
        .await;
    let (verb, at) = documented("create_order");
    Mock::given(method(verb.as_str()))
        .and(path(at))
        .respond_with(Stores(ada.id))
        .mount(&api)
        .await;
    api
}

fn header<'a>(request: &'a Request, name: &str) -> &'a str {
    request.headers.get(name).unwrap().to_str().unwrap()
}

fn text_of(asked: &Value) -> &str {
    asked["text"].as_str().unwrap()
}

/// The "Done when" of the phase, as far as it can be had without Telegram: a location
/// is shared, a delivery of two parcels for tomorrow is chosen, it is confirmed, and
/// the API has the order.
#[tokio::test(flavor = "multi_thread")]
async fn a_location_three_presses_and_a_confirmation_place_one_order() {
    let tomorrow = Local::now().date_naive() + Days::new(1);
    let telegram = Telegram::with(vec![
        message_update(
            1,
            10,
            json!({ "location": { "latitude": 50.4501, "longitude": 30.5234 } }),
        ),
        press_update(2, SENT, &Press::Kind(10, OrderKind::Delivery).data()),
        press_update(3, SENT, &Press::Parcels(10, 2).data()),
        press_update(4, SENT, &Press::Day(10, tomorrow).data()),
        press_update(5, SENT, &Press::Place(10).data()),
    ])
    .await;
    let api = an_api().await;
    let running = start(&telegram, &api.uri());

    // One question under the location, which then changes four times where it stands.
    let changed = telegram.until_asked("editMessageText", 4).await;
    let said = telegram.asked("sendMessage").await;
    assert_eq!(said.len(), 1, "{said:#?}");
    assert_eq!(said[0]["chat_id"], ADA);
    assert_eq!(
        text_of(&said[0]),
        "An order at 50.45010, 30.52340.\nIs it a delivery to there, or a pickup from there?"
    );
    assert_eq!(
        said[0]["reply_markup"]["inline_keyboard"],
        json!([
            [
                { "text": "Delivery", "callback_data": "k:10:delivery" },
                { "text": "Pickup", "callback_data": "k:10:pickup" },
            ],
            [{ "text": "Cancel", "callback_data": "no:10" }],
        ])
    );
    // What the bot says is text, and is not read by Telegram as anything else.
    assert!(said[0].get("parse_mode").is_none(), "{}", said[0]);

    assert_eq!(changed.len(), 4, "{changed:#?}");
    for change in &changed {
        assert_eq!(
            (&change["chat_id"], &change["message_id"]),
            (&json!(ADA), &json!(SENT))
        );
    }
    assert!(text_of(&changed[0]).ends_with("How many parcels?"));
    assert!(text_of(&changed[1]).ends_with("On which day?"));
    assert!(text_of(&changed[2]).ends_with("Place this order?"));
    assert!(
        text_of(&changed[3])
            .starts_with("Placed: a delivery of 2 parcels to 50.45010, 30.52340, on "),
        "{}",
        changed[3]
    );
    // What is placed has no button left under it.
    assert!(changed[2].get("reply_markup").is_some());
    assert!(changed[3].get("reply_markup").is_none(), "{}", changed[3]);
    // And every press was told to Telegram as heard.
    let heard: Vec<Value> = telegram
        .until_asked("answerCallbackQuery", 4)
        .await
        .iter()
        .map(|heard| heard["callback_query_id"].clone())
        .collect();
    assert_eq!(heard, ["press-2", "press-3", "press-4", "press-5"]);

    // The API was asked twice, and both times by the last press: who Ada is, with
    // the bot's key, and then for the order, with Ada's token.
    let asked = api.received_requests().await.unwrap();
    assert_eq!(asked.len(), 2, "{asked:#?}");
    let (login, order) = (&asked[0], &asked[1]);
    assert_eq!(login.url.path(), "/api/auth/telegram");
    assert_eq!(header(login, "authorization"), format!("Bearer {KEY}"));
    assert_eq!(header(login, "x-request-id"), "tg-5");
    assert_eq!(
        login.body_json::<Value>().unwrap(),
        json!({ "telegram_id": ADA })
    );
    assert_eq!(order.url.path(), "/api/orders");
    assert_eq!(header(order, "authorization"), format!("Bearer {TOKEN}"));
    assert_eq!(header(order, "x-request-id"), "tg-5");
    // With a key that makes the order safe to ask for again (D76).
    let key = header(order, "idempotency-key");
    assert!(
        key.parse::<routeforge_domain::IdempotencyKey>().is_ok(),
        "{key}"
    );
    assert_eq!(
        order.body_json::<Value>().unwrap(),
        json!({
            "kind": "delivery",
            "service_date": tomorrow,
            "location": { "lat": 50.4501, "lon": 30.5234 },
            "address": null,
            "demand": 2,
        })
    );

    // The bot asked Telegram for messages and presses and for nothing else, and
    // told it the commands there are.
    let polled = telegram.asked("getUpdates").await;
    let wanted: BTreeSet<&str> = polled[0]["allowed_updates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|kind| kind.as_str().unwrap())
        .collect();
    assert_eq!(wanted, BTreeSet::from(["callback_query", "message"]));
    let menu = &telegram.asked("setMyCommands").await[0]["commands"];
    let commands: Vec<&str> = menu
        .as_array()
        .unwrap()
        .iter()
        .map(|command| command["command"].as_str().unwrap())
        .collect();
    assert_eq!(
        commands,
        [
            "order", "orders", "cancel", "pending", "plan", "status", "help"
        ]
    );

    // Told to stop, it tells Telegram what it has had, so that none of it comes
    // again, and is gone.
    let (ended, took) = running.stopped().await;
    ended.unwrap();
    assert!(took < Duration::from_secs(3), "{took:?}");
    let last = telegram.asked("getUpdates").await.pop().unwrap();
    assert_eq!(
        (&last["offset"], &last["limit"], &last["timeout"]),
        (&json!(6), &json!(1), &json!(0))
    );
    // Nothing was placed twice on the way out.
    assert_eq!(api.received_requests().await.unwrap().len(), 2);
}

/// What Telegram sends beside a plain location and a plain text.
#[tokio::test(flavor = "multi_thread")]
async fn a_venue_is_a_place_and_what_is_neither_a_place_nor_words_is_told_so() {
    let market = json!({
        "location": { "latitude": 50.442, "longitude": 30.522 },
        "venue": {
            "location": { "latitude": 50.442, "longitude": 30.522 },
            "title": "Besarabsky Market",
            "address": "Besarabska Square, 2",
        },
    });
    let contact = json!({
        "contact": { "phone_number": "+380441234567", "first_name": "Ada" },
    });
    let group = json!({ "id": -100_123, "type": "group", "title": "Couriers" });
    let telegram = Telegram::with(vec![
        message_update(1, 10, market),
        message_update(2, 11, contact),
        // Words in a group, which is nobody's chat to put an order together in.
        message_update_in(3, 12, group, json!({ "text": "/help" })),
        message_update(4, 13, json!({ "text": "/order 50.45 30.52" })),
    ])
    .await;
    let api = an_api().await;
    let running = start(&telegram, &api.uri());

    let said = telegram.until_asked("sendMessage", 3).await;
    assert_eq!(said.len(), 3, "the group was not answered: {said:#?}");
    assert!(
        text_of(&said[0]).starts_with(
            "An order at Besarabsky Market, Besarabska Square, 2 (50.44200, 30.52200)."
        ),
        "{}",
        said[0]
    );
    assert!(text_of(&said[1]).starts_with("I take no free text."));
    assert!(text_of(&said[2]).starts_with("An order at 50.45000, 30.52000."));
    assert!(said.iter().all(|said| said["chat_id"] == ADA));

    running.stopped().await.0.unwrap();
    // None of it was the API's business.
    assert!(api.received_requests().await.unwrap().is_empty());
}

/// A bot that was given a geocoding service takes an address in words (D72): the
/// words go to that service and to nobody else, the place it found is shown with a
/// button, and the press of it begins the order.
#[tokio::test(flavor = "multi_thread")]
async fn an_address_in_words_is_asked_of_the_service_the_bot_was_given() {
    const KHRESHCHATYK: &str = "22, Хрещатик вулиця, Липки, Київ, 01001, Україна";
    let telegram = Telegram::with(vec![
        message_update(1, 10, json!({ "text": "Хрещатик 22" })),
        press_update(2, SENT, &Press::Found(10, 0).data()),
    ])
    .await;
    let api = an_api().await;
    let (verb, at) = documented("get_depot");
    Mock::given(method(verb.as_str()))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_json(a_depot()))
        .mount(&api)
        .await;
    // The service, answering as Nominatim does: the two numbers as text.
    let places = MockServer::start().await;
    let found = json!([{
        "lat": "50.4476532",
        "lon": "30.5222416",
        "display_name": KHRESHCHATYK,
    }]);
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(found))
        .mount(&places)
        .await;
    let geocoder = Nominatim::new(&places.uri().parse().unwrap()).unwrap();
    let running = start_with(&telegram, &api.uri(), Some(geocoder));

    // The place is shown under the address, with where it is from, and the press
    // puts the first question of an order where the place stood.
    let changed = telegram.until_asked("editMessageText", 1).await;
    let said = telegram.asked("sendMessage").await;
    assert_eq!(said.len(), 1, "{said:#?}");
    assert_eq!(
        text_of(&said[0]),
        format!("Is it this place?\n{KHRESHCHATYK}\n\nPlaces: © OpenStreetMap contributors.")
    );
    assert_eq!(
        said[0]["reply_markup"]["inline_keyboard"],
        json!([
            [{ "text": "Yes, an order there", "callback_data": "f:10:0" }],
            [{ "text": "No", "callback_data": "fx:10" }],
        ])
    );
    assert_eq!(
        text_of(&changed[0]),
        format!(
            "An order at {KHRESHCHATYK} (50.44765, 30.52224).\n\
             Is it a delivery to there, or a pickup from there?"
        )
    );
    assert_eq!(changed[0]["message_id"], SENT);
    running.stopped().await.0.unwrap();

    // The service was asked once: for the words, near the depot, by the bot under
    // its own name. Whose address it is, it was not told: nothing of Ada's is sent,
    // and neither is the bot's key.
    let asked = places.received_requests().await.unwrap();
    assert_eq!(asked.len(), 1, "{asked:#?}");
    let query: Vec<(String, String)> = asked[0]
        .url
        .query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    let expected = [
        ("q", "Хрещатик 22"),
        ("format", "jsonv2"),
        ("limit", "10"),
        ("viewbox", "29.7348,49.9500,31.3052,50.9500"),
    ]
    .map(|(name, value)| (name.to_owned(), value.to_owned()));
    assert_eq!(query, expected);
    assert!(header(&asked[0], "user-agent").starts_with("routeforge-bot/"));
    for (name, _) in &asked[0].headers {
        let any_request_has_it = ["accept", "accept-encoding", "host", "user-agent"];
        assert!(any_request_has_it.contains(&name.as_str()), "{name}");
    }
    assert!(asked[0].body.is_empty(), "{:?}", asked[0].body);

    // The API was asked who Ada is and, as Ada, where the depot is: to look round
    // it. Both by the message, and nothing by the press.
    let asked = api.received_requests().await.unwrap();
    let paths: Vec<&str> = asked.iter().map(|request| request.url.path()).collect();
    assert_eq!(paths, ["/api/auth/telegram", "/api/depot"]);
    assert_eq!(header(&asked[0], "authorization"), format!("Bearer {KEY}"));
    assert_eq!(
        header(&asked[1], "authorization"),
        format!("Bearer {TOKEN}")
    );
    for request in &asked {
        assert_eq!(header(request, "x-request-id"), "tg-1");
    }
}

/// `/start` shows the button that shares a location, and says who the person is.
#[tokio::test(flavor = "multi_thread")]
async fn start_offers_the_button_that_shares_a_location() {
    let telegram = Telegram::with(vec![message_update(1, 10, json!({ "text": "/start" }))]).await;
    let api = an_api().await;
    let running = start(&telegram, &api.uri());

    let said = telegram.until_asked("sendMessage", 1).await;
    assert!(text_of(&said[0]).starts_with("RouteForge takes parcel orders"));
    assert!(text_of(&said[0]).contains("You are a client here."));
    assert_eq!(
        said[0]["reply_markup"],
        json!({
            "keyboard": [[{ "text": "Share my location", "request_location": true }]],
            "resize_keyboard": true,
            "one_time_keyboard": true,
        })
    );
    running.stopped().await.0.unwrap();
}

/// A message that is too old to be changed, or is gone, cannot take a reply in its
/// place. The reply is said anew. One that would say what it says already is left.
#[tokio::test(flavor = "multi_thread")]
async fn a_reply_that_cannot_take_the_place_of_a_message_is_sent_as_a_new_one() {
    let telegram = Telegram::with(vec![
        // No order is open, so both presses are answered in the place of the message.
        press_update(1, SENT, &Press::Kind(10, OrderKind::Delivery).data()),
        press_update(2, SENT, &Press::Kind(10, OrderKind::Delivery).data()),
    ])
    .await;
    telegram
        .answers_times(
            "editMessageText",
            telegram_says_no(400, "Bad Request: message can't be edited"),
            1,
        )
        .await;
    let api = an_api().await;
    let running = start(&telegram, &api.uri());

    telegram.until_asked("editMessageText", 2).await;
    let said = telegram.until_asked("sendMessage", 1).await;
    assert!(text_of(&said[0]).starts_with("This order is no longer open"));
    running.stopped().await.0.unwrap();
    assert_eq!(
        telegram.asked("sendMessage").await.len(),
        1,
        "the second was changed in place"
    );

    // Pressed again with nothing new to show, Telegram says that nothing changed.
    // That is no reason to say it all once more.
    let telegram = Telegram::with(vec![press_update(1, SENT, "pn")]).await;
    let unchanged = "Bad Request: message is not modified: specified new message content and \
        reply markup are exactly the same as a current content and reply markup of the message";
    telegram
        .answers("editMessageText", telegram_says_no(400, unchanged))
        .await;
    let running = start(&telegram, &api.uri());
    telegram.until_asked("editMessageText", 1).await;
    running.stopped().await.0.unwrap();
    assert!(telegram.asked("sendMessage").await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_telegram_does_not_know_ends_the_bot_with_the_reason() {
    // How Telegram answers a token that is well formed and not one of its own, and
    // one that is no token at all.
    for (status, says) in [(401, "Unauthorized"), (404, "Not Found")] {
        let telegram = Telegram::unknowing().await;
        telegram
            .answers("getMe", telegram_says_no(status, says))
            .await;
        let running = start(&telegram, "http://127.0.0.1:9");

        let error = soon(running.bot).await.unwrap().unwrap_err();
        assert!(error.to_string().contains("TELEGRAM_BOT_TOKEN"), "{error}");
        assert!(error.to_string().contains("@BotFather"), "{error}");
        // The token itself is not repeated.
        assert!(!format!("{error:?}").contains(BOT_TOKEN), "{error:?}");
        // And it did not go on to ask for updates it would never be given.
        assert_eq!(telegram.asked("getMe").await.len(), 1, "{says}");
        assert!(telegram.asked("getUpdates").await.is_empty(), "{says}");
    }
}

/// The bot leans on one service that can be away (D58). It waits for it, as a solver
/// waits for its database.
#[tokio::test(flavor = "multi_thread")]
async fn a_telegram_that_does_not_answer_is_tried_again_until_it_does() {
    let telegram = Telegram::with(vec![message_update(1, 10, json!({ "text": "/order" }))]).await;
    // Three times something answers in Telegram's place that is no Telegram.
    let no_telegram = ResponseTemplate::new(200).set_body_string("<html>Come back later</html>");
    telegram.answers_times("getMe", no_telegram, 3).await;
    let running = start(&telegram, "http://127.0.0.1:9");

    // It gets there, and answers the chat that was waiting.
    let said = telegram.until_asked("sendMessage", 1).await;
    assert!(text_of(&said[0]).starts_with("To place an order,"));
    assert!(telegram.asked("getMe").await.len() >= 4);
    running.stopped().await.0.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn told_to_stop_while_telegram_is_away_the_bot_stops_at_once() {
    // Nobody listens where Telegram was said to be.
    let nobody = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let nowhere: Url = format!("http://{}", nobody.local_addr().unwrap())
        .parse()
        .unwrap();
    drop(nobody);
    let api = HttpApi::new("http://127.0.0.1:9", KEY).unwrap();
    let chats = Arc::new(Chats::new(api, None));
    let (stop, stopping) = watch::channel(false);
    // A bot that would wait a minute between its attempts.
    let patient = Pace {
        retry: Backoff::new(Duration::from_secs(60), Duration::from_secs(60)),
        ..PACE
    };
    let away = bot(BOT_TOKEN, Some(nowhere)).unwrap();
    let running = tokio::spawn(run(away, chats.clone(), patient, stopping));

    // Told while it is asking, or while it waits to ask again: which of the two
    // depends on how long the machine takes to say that nobody listens.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !running.is_finished(),
        "it gave up on a Telegram that is away"
    );
    let told = Instant::now();
    stop.send_replace(true);
    soon(running).await.unwrap().unwrap();
    assert!(
        told.elapsed() < Duration::from_secs(2),
        "{:?}",
        told.elapsed()
    );

    // Told while it waits to ask again, for certain: something answers at once in
    // Telegram's place, and the bot would wait a minute before it asks once more.
    let telegram = Telegram::unknowing().await;
    let no_telegram = ResponseTemplate::new(200).set_body_string("<html>Come back later</html>");
    telegram.answers("getMe", no_telegram).await;
    let (stop, stopping) = watch::channel(false);
    let waiting = bot(BOT_TOKEN, Some(telegram.url())).unwrap();
    let running = tokio::spawn(run(waiting, chats, patient, stopping));
    telegram.until_asked("getMe", 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !running.is_finished(),
        "it gave up on a Telegram that is away"
    );

    let told = Instant::now();
    stop.send_replace(true);
    soon(running).await.unwrap().unwrap();
    assert!(
        told.elapsed() < Duration::from_secs(2),
        "{:?}",
        told.elapsed()
    );
    assert_eq!(
        telegram.asked("getMe").await.len(),
        1,
        "it did not ask again"
    );
}
