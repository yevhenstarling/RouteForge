//! The bot's binary as it is started: by whoever runs it, and by compose.
//!
//! What the bot says and does is tested where it is written. This is the way in to
//! it, which only the binary itself can show: what it makes of its settings, and that
//! it goes to work with the ones it was given and not with others.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const TOKEN: &str = "424242:a-token-made-up-for-the-binary";
const KEY: &str = "the key the bot is started with, in a test";
const SESSION: &str = "the.token.of-the-person";

/// The binary, with none of the settings of the machine it is tested on.
fn bot() -> Command {
    let mut bot = Command::new(env!("CARGO_BIN_EXE_routeforge-bot"));
    for setting in [
        "TELEGRAM_BOT_TOKEN",
        "BOT_SERVICE_KEY",
        "BOT_API_URL",
        "TELEGRAM_API_URL",
        "GEOCODER_URL",
        "LOG_FORMAT",
        "RUST_LOG",
    ] {
        bot.env_remove(setting);
    }
    bot
}

fn run(bot: &mut Command) -> Output {
    bot.output().expect("the binary of the bot runs")
}

/// Ends a bot that was left running when a test is over, however the test ends.
struct Started(Child);

impl Drop for Started {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Waits for something that must come about, and fails if it does not.
async fn until<T>(what: &str, mut look: impl AsyncFnMut() -> Option<T>) -> T {
    let began = Instant::now();
    loop {
        if let Some(found) = look().await {
            return found;
        }
        assert!(
            began.elapsed() < Duration::from_secs(30),
            "waited in vain for {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A fresh clone has no token (D54). The bot then holds its place and waits (D8).
#[test]
fn without_a_token_it_says_that_it_idles_and_stays() {
    let child = bot()
        .stdout(Stdio::piped())
        .spawn()
        .expect("the binary of the bot runs");
    let mut started = Started(child);
    let output = started.0.stdout.take().expect("its output was asked for");
    let (said, heard) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(output).lines().map_while(Result::ok) {
            if said.send(line).is_err() {
                break;
            }
        }
    });

    let waited = Instant::now();
    loop {
        let left = Duration::from_secs(30).saturating_sub(waited.elapsed());
        let line = heard
            .recv_timeout(left)
            .expect("the bot says that it idles");
        if line.contains("TELEGRAM_BOT_TOKEN is not set") && line.contains("idles") {
            break;
        }
    }
    // And it is still there: a service of the stack, with nothing to do.
    thread::sleep(Duration::from_millis(300));
    let ended = started.0.try_wait().expect("the bot can be asked");
    assert!(ended.is_none(), "it ended with {ended:?}");
}

#[test]
fn with_a_token_and_no_key_it_ends_and_names_what_it_misses() {
    let no_key = run(bot().env("TELEGRAM_BOT_TOKEN", TOKEN));
    assert_eq!(no_key.status.code(), Some(1), "{no_key:?}");
    let said = String::from_utf8_lossy(&no_key.stderr);
    assert!(said.contains("BOT_SERVICE_KEY"), "{said}");

    let short_key = run(bot()
        .env("TELEGRAM_BOT_TOKEN", TOKEN)
        .env("BOT_SERVICE_KEY", "too short"));
    assert_eq!(short_key.status.code(), Some(1), "{short_key:?}");
    let said = String::from_utf8_lossy(&short_key.stderr);
    assert!(said.contains("at least 32"), "{said}");

    // Neither is repeated, on either of its outputs.
    let all = format!("{no_key:?} {short_key:?}");
    assert!(!all.contains(TOKEN) && !all.contains("too short"), "{all}");
}

#[test]
fn a_setting_that_is_no_address_ends_it_by_name() {
    for setting in ["BOT_API_URL", "TELEGRAM_API_URL", "GEOCODER_URL"] {
        let ended = run(bot().env(setting, "the API"));
        assert_eq!(ended.status.code(), Some(1), "{setting}: {ended:?}");
        let said = String::from_utf8_lossy(&ended.stderr);
        assert!(said.contains(setting), "{said}");
    }
}

fn ok(result: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "ok": true, "result": result }))
}

/// Has a server answer a method of Telegram's Bot API for the bot of [`TOKEN`].
/// Telegram takes the name of a method in any case.
async fn telegram_answers(server: &MockServer, bot_method: &str, with: impl Respond + 'static) {
    Mock::given(method("POST"))
        .and(path_regex(format!("(?i)^/bot[^/]+/{bot_method}$")))
        .respond_with(with)
        .mount(server)
        .await;
}

/// Hands out one message with this text, and then nothing: as `getUpdates` does, from
/// the offset on that it is asked for.
struct OneMessage(&'static str);

impl Respond for OneMessage {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let asked: Value = request.body_json().unwrap_or_default();
        if asked["offset"].as_i64().unwrap_or(0) > 1 {
            return ok(json!([])).set_delay(Duration::from_millis(50));
        }
        ok(json!([{
            "update_id": 1,
            "message": {
                "message_id": 10,
                "date": 1_760_000_000,
                "chat": { "id": 4_242, "type": "private", "first_name": "Ada" },
                "from": { "id": 4_242, "is_bot": false, "first_name": "Ada" },
                "text": self.0,
            },
        }]))
    }
}

/// The paths of the requests a server has had, and the requests.
async fn asked(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.unwrap_or_default()
}

fn header<'a>(request: &'a Request, name: &str) -> &'a str {
    let value = request.headers.get(name);
    value.and_then(|value| value.to_str().ok()).unwrap_or("")
}

/// A server in the place of Telegram. It knows the bot of [`TOKEN`], hands it one
/// message from Ada with this text, and takes what the bot sends.
async fn a_telegram_where_ada_writes(text: &'static str) -> MockServer {
    let telegram = MockServer::start().await;
    telegram_answers(
        &telegram,
        "getMe",
        ok(json!({
            "id": 777,
            "is_bot": true,
            "first_name": "RouteForge",
            "username": "routeforge_test_bot",
            "can_join_groups": false,
            "can_read_all_group_messages": false,
            "supports_inline_queries": false,
            "can_connect_to_business": false,
            "has_main_web_app": false,
        })),
    )
    .await;
    let no_webhook =
        json!({ "url": "", "has_custom_certificate": false, "pending_update_count": 0 });
    telegram_answers(&telegram, "getWebhookInfo", ok(no_webhook)).await;
    telegram_answers(&telegram, "setMyCommands", ok(json!(true))).await;
    telegram_answers(&telegram, "getUpdates", OneMessage(text)).await;
    let sent = json!({
        "message_id": 100,
        "date": 1_760_000_000,
        "chat": { "id": 4_242, "type": "private", "first_name": "Ada" },
        "text": "what the bot said",
    });
    telegram_answers(&telegram, "sendMessage", ok(sent)).await;
    telegram
}

/// A server in the place of the API, which knows Ada for a client: as the API's
/// document describes what it sends.
async fn an_api_that_knows_ada() -> MockServer {
    let api = MockServer::start().await;
    let session = json!({
        "token": SESSION,
        "expires_in": 60,
        "user": {
            "id": "0199d3c2-7b1e-7f6a-9c0d-2f4b8a1e6d35",
            "email": null,
            "telegram_id": 4_242,
            "role": "client",
            "created_at": "2026-10-12T08:00:00Z",
        },
    });
    Mock::given(method("POST"))
        .and(path("/api/auth/telegram"))
        .respond_with(ResponseTemplate::new(200).set_body_json(session))
        .mount(&api)
        .await;
    api
}

/// Has a server in the place of the API answer a `GET` at a path with this.
async fn api_gives(api: &MockServer, at: &str, answer: Value) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer))
        .mount(api)
        .await;
}

/// What the bot sent to the chat, once it has.
async fn said_to_the_chat(telegram: &MockServer) -> Request {
    until("the bot to answer the chat", async || {
        let said = asked(telegram).await;
        said.into_iter()
            .find(|request| request.url.path().ends_with("/SendMessage"))
    })
    .await
}

/// The whole of it through `main`: the token goes to Telegram, the key and the
/// person's token go to the API, each where it belongs.
#[tokio::test(flavor = "multi_thread")]
async fn given_its_settings_it_talks_to_the_telegram_and_the_api_they_name() {
    let telegram = a_telegram_where_ada_writes("/orders").await;
    let api = an_api_that_knows_ada().await;
    api_gives(&api, "/api/orders", json!([])).await;

    let child = bot()
        .env("TELEGRAM_BOT_TOKEN", TOKEN)
        .env("TELEGRAM_API_URL", telegram.uri())
        .env("BOT_API_URL", api.uri())
        .env("BOT_SERVICE_KEY", KEY)
        .stdout(Stdio::null())
        .spawn()
        .expect("the binary of the bot runs");
    let _started = Started(child);

    // It answers the chat, having asked the API on the person's behalf.
    let said = said_to_the_chat(&telegram).await;
    // To Telegram as the bot of its token, and of no other.
    assert_eq!(said.url.path(), format!("/bot{TOKEN}/SendMessage"));
    let said: Value = said.body_json().unwrap_or_default();
    assert_eq!(said["chat_id"], 4_242);
    assert_eq!(
        said["text"],
        "You have no orders for today or later. Share a location to place one."
    );

    let asked = asked(&api).await;
    let paths: Vec<&str> = asked.iter().map(|request| request.url.path()).collect();
    assert_eq!(paths, ["/api/auth/telegram", "/api/orders"]);
    // The key for who the person is, their token for what is theirs, and the
    // update the two came of on both.
    assert_eq!(header(&asked[0], "authorization"), format!("Bearer {KEY}"));
    assert_eq!(
        header(&asked[1], "authorization"),
        format!("Bearer {SESSION}")
    );
    assert_eq!(header(&asked[0], "x-request-id"), "tg-1");
    assert_eq!(header(&asked[1], "x-request-id"), "tg-1");
}

/// What a person writes goes to a geocoding service only where the bot was started
/// with one (D72). Started without, it sends the words to nobody.
#[tokio::test(flavor = "multi_thread")]
async fn it_asks_where_an_address_is_only_of_the_service_it_was_started_with() {
    const PLACE: &str = "22, Хрещатик вулиця, Липки, Київ, 01001, Україна";
    let depot = json!({
        "id": "0199d3c2-7b1e-7f6a-9c0d-2f4b8a1e6d36",
        "name": "Depot",
        "location": { "lat": 50.45, "lon": 30.52 },
        "created_at": "2026-10-12T08:00:00Z",
    });
    // The service, which answers as Nominatim does.
    let places = MockServer::start().await;
    let found = json!([{ "lat": "50.4476532", "lon": "30.5222416", "display_name": PLACE }]);
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(found))
        .mount(&places)
        .await;

    // Without the setting: the words are words, and nobody is asked about them.
    let telegram = a_telegram_where_ada_writes("Хрещатик 22").await;
    let api = an_api_that_knows_ada().await;
    api_gives(&api, "/api/depot", depot.clone()).await;
    let child = bot()
        .env("TELEGRAM_BOT_TOKEN", TOKEN)
        .env("TELEGRAM_API_URL", telegram.uri())
        .env("BOT_API_URL", api.uri())
        .env("BOT_SERVICE_KEY", KEY)
        .stdout(Stdio::null())
        .spawn()
        .expect("the binary of the bot runs");
    let started = Started(child);
    let said: Value = said_to_the_chat(&telegram)
        .await
        .body_json()
        .unwrap_or_default();
    let text = said["text"].as_str().unwrap_or_default();
    assert!(text.starts_with("I take no free text."), "{text}");
    drop(started);
    assert!(asked(&places).await.is_empty());
    assert!(asked(&api).await.is_empty());

    // With it: the service is asked where that is, and the place is shown.
    let telegram = a_telegram_where_ada_writes("Хрещатик 22").await;
    let child = bot()
        .env("TELEGRAM_BOT_TOKEN", TOKEN)
        .env("TELEGRAM_API_URL", telegram.uri())
        .env("BOT_API_URL", api.uri())
        .env("BOT_SERVICE_KEY", KEY)
        .env("GEOCODER_URL", places.uri())
        .stdout(Stdio::null())
        .spawn()
        .expect("the binary of the bot runs");
    let _started = Started(child);
    let said: Value = said_to_the_chat(&telegram)
        .await
        .body_json()
        .unwrap_or_default();
    assert_eq!(
        said["text"],
        format!("Is it this place?\n{PLACE}\n\nPlaces: © OpenStreetMap contributors.")
    );

    let searched = asked(&places).await;
    assert_eq!(searched.len(), 1, "{searched:#?}");
    let words = searched[0].url.query_pairs().find(|(name, _)| name == "q");
    assert_eq!(
        words.map(|(_, words)| words.into_owned()).as_deref(),
        Some("Хрещатик 22")
    );
    // It says who it is, and nothing of whose address this is.
    assert!(header(&searched[0], "user-agent").starts_with("routeforge-bot/"));
    assert_eq!(header(&searched[0], "authorization"), "");
    // Where the depot is it asked the API, as Ada.
    let paths: Vec<String> = asked(&api)
        .await
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    assert_eq!(paths, ["/api/auth/telegram", "/api/depot"]);
}

/// A token that Telegram does not know ends the process, with the reason and a
/// status that says it did not go well.
#[tokio::test(flavor = "multi_thread")]
async fn a_token_telegram_refuses_ends_it_with_the_reason() {
    let telegram = MockServer::start().await;
    let refusal = json!({ "ok": false, "error_code": 401, "description": "Unauthorized" });
    telegram_answers(
        &telegram,
        "getMe",
        ResponseTemplate::new(401).set_body_json(refusal),
    )
    .await;

    let mut bot = bot();
    bot.env("TELEGRAM_BOT_TOKEN", TOKEN)
        .env("TELEGRAM_API_URL", telegram.uri())
        .env("BOT_SERVICE_KEY", KEY);
    let ended = tokio::task::spawn_blocking(move || run(&mut bot))
        .await
        .expect("the bot was run");

    assert_eq!(ended.status.code(), Some(1), "{ended:?}");
    let said = String::from_utf8_lossy(&ended.stderr);
    assert!(
        said.contains("Telegram refused TELEGRAM_BOT_TOKEN"),
        "{said}"
    );
    let all = format!("{ended:?}");
    assert!(!all.contains(TOKEN) && !all.contains(KEY), "{all}");
}
