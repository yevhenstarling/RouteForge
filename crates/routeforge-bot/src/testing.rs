//! Shared by the tests: something in the place of the API that the dialogue can be
//! tried against, the things the API sends, and servers in the place of the API and
//! of Telegram for what really goes over HTTP.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{NaiveDate, Utc};
use routeforge_domain::{
    DepotId, LatLon, Order, OrderFilter, OrderId, OrderKind, OrderRequest, OrderStatus, Plan,
    PlanId, PlanRequest, PlanStatus, Role, RouteBudget, Session, User, UserId,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::api::{Api, Caller, Failure};

/// A day in October 2026.
pub(crate) fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, day).unwrap()
}

/// Waits for something that must not take long, so that a test fails instead of
/// hanging.
pub(crate) async fn soon<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), future)
        .await
        .expect("this should not have taken so long")
}

/// An account as the API gives it for a Telegram user.
pub(crate) fn a_user(role: Role, telegram_id: i64) -> User {
    User {
        id: UserId::generate(),
        email: None,
        telegram_id: Some(telegram_id),
        role,
        created_at: Utc::now(),
    }
}

/// An order as the API stores one for a request.
pub(crate) fn an_order(client_id: UserId, asked: &OrderRequest) -> Order {
    Order {
        id: OrderId::generate(),
        client_id,
        kind: asked.kind,
        status: OrderStatus::Pending,
        service_date: asked.service_date,
        location: asked.location,
        address: asked.address.clone(),
        demand: asked.demand,
        plan_id: None,
        created_at: Utc::now(),
    }
}

/// What asks for a delivery of so many parcels on a day, in Kyiv.
pub(crate) fn a_delivery(day: NaiveDate, parcels: u32) -> OrderRequest {
    OrderRequest {
        kind: OrderKind::Delivery,
        service_date: day,
        location: LatLon::new(50.45, 30.52).unwrap(),
        address: None,
        demand: parcels,
    }
}

/// A plan of a day as the API stores one that was just asked for.
pub(crate) fn a_plan(day: NaiveDate, created_by: UserId) -> Plan {
    Plan {
        id: PlanId::generate(),
        service_date: day,
        depot_id: DepotId::generate(),
        status: PlanStatus::Queued,
        seed: 7,
        budget: RouteBudget::new(Some(10_000), None).unwrap(),
        created_by,
        orders_total: 0,
        orders_unassigned: 0,
        routes_total: 0,
        routes_done: 0,
        error: None,
        created_at: Utc::now(),
        finished_at: None,
    }
}

/// What the dialogue asked of the API, as [`FakeApi`] noted it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Asked {
    Login {
        telegram_id: i64,
        request_id: String,
    },
    CreateOrder {
        token: String,
        request_id: String,
        order: OrderRequest,
    },
    ListOrders {
        token: String,
        filter: OrderFilter,
    },
    GetOrder {
        token: String,
        id: OrderId,
    },
    CancelOrder {
        token: String,
        id: OrderId,
    },
    CreatePlan {
        token: String,
        request_id: String,
        plan: PlanRequest,
    },
    ListPlans {
        token: String,
        day: NaiveDate,
    },
}

/// In the place of the API, for the tests of the dialogue.
///
/// It notes everything it is asked, keeps orders and plans as the API does as far as
/// the bot can tell, and can be told to refuse the next request or not to answer it.
/// Every login gives a new token, so a test can see which login a request came of.
#[derive(Clone)]
pub(crate) struct FakeApi(Arc<Mutex<Fake>>);

struct Fake {
    user: User,
    logins: u32,
    orders: Vec<Order>,
    plans: Vec<Plan>,
    asked: Vec<Asked>,
    /// What the next logins are answered with, in place of a session.
    no_login: VecDeque<Failure>,
    /// What the next requests that are no login are answered with.
    no_answer: VecDeque<Failure>,
}

impl FakeApi {
    /// The API with one Telegram user, who is a client.
    pub(crate) fn new(telegram_id: i64) -> Self {
        Self(Arc::new(Mutex::new(Fake {
            user: a_user(Role::Client, telegram_id),
            logins: 0,
            orders: Vec::new(),
            plans: Vec::new(),
            asked: Vec::new(),
            no_login: VecDeque::new(),
            no_answer: VecDeque::new(),
        })))
    }

    fn fake(&self) -> std::sync::MutexGuard<'_, Fake> {
        self.0.lock().unwrap()
    }

    /// Who the Telegram user is to the API.
    pub(crate) fn user(&self) -> User {
        self.fake().user.clone()
    }

    /// What an admin does when they change the account's role.
    pub(crate) fn make(&self, role: Role) {
        self.fake().user.role = role;
    }

    /// The token the last login gave.
    pub(crate) fn last_token(&self) -> String {
        format!("token-{}", self.fake().logins)
    }

    /// Everything that was asked so far, oldest first.
    pub(crate) fn asked(&self) -> Vec<Asked> {
        self.fake().asked.clone()
    }

    /// What was asked since this was last called.
    pub(crate) fn asked_since(&self) -> Vec<Asked> {
        std::mem::take(&mut self.fake().asked)
    }

    pub(crate) fn orders(&self) -> Vec<Order> {
        self.fake().orders.clone()
    }

    /// Stores an order as if it had been placed some time ago, and gives it.
    pub(crate) fn has(&self, order: Order) -> Order {
        self.fake().orders.push(order.clone());
        order
    }

    pub(crate) fn has_plan(&self, plan: Plan) {
        self.fake().plans.push(plan);
    }

    pub(crate) fn plans(&self) -> Vec<Plan> {
        self.fake().plans.clone()
    }

    /// The next login is answered with this in place of a session.
    pub(crate) fn fail_login(&self, failure: Failure) {
        self.fake().no_login.push_back(failure);
    }

    /// The next request that is no login is answered with this.
    pub(crate) fn fail_next(&self, failure: Failure) {
        self.fake().no_answer.push_back(failure);
    }
}

fn refused(status: u16, detail: &str) -> Failure {
    Failure::Refused {
        status,
        detail: detail.to_owned(),
    }
}

impl Fake {
    /// What every request that is no login begins with: it is noted, and refused if
    /// the test said so.
    fn take(&mut self, asked: Asked) -> Result<(), Failure> {
        self.asked.push(asked);
        self.no_answer.pop_front().map_or(Ok(()), Err)
    }

    fn is_staff(&self) -> bool {
        self.user.role != Role::Client
    }
}

impl Api for FakeApi {
    async fn telegram_login(&self, telegram_id: i64, request_id: &str) -> Result<Session, Failure> {
        let mut fake = self.fake();
        fake.asked.push(Asked::Login {
            telegram_id,
            request_id: request_id.to_owned(),
        });
        if let Some(failure) = fake.no_login.pop_front() {
            return Err(failure);
        }
        assert_eq!(
            Some(telegram_id),
            fake.user.telegram_id,
            "the fake knows one user"
        );
        fake.logins += 1;
        Ok(Session {
            token: format!("token-{}", fake.logins),
            expires_in: 60,
            user: fake.user.clone(),
        })
    }

    async fn create_order(&self, who: &Caller<'_>, order: &OrderRequest) -> Result<Order, Failure> {
        let mut fake = self.fake();
        fake.take(Asked::CreateOrder {
            token: who.token.to_owned(),
            request_id: who.request_id.to_owned(),
            order: order.clone(),
        })?;
        let stored = an_order(fake.user.id, order);
        fake.orders.push(stored.clone());
        Ok(stored)
    }

    async fn list_orders(
        &self,
        who: &Caller<'_>,
        filter: &OrderFilter,
    ) -> Result<Vec<Order>, Failure> {
        let mut fake = self.fake();
        fake.take(Asked::ListOrders {
            token: who.token.to_owned(),
            filter: *filter,
        })?;
        // As the API lists them: oldest first, after the one that is named, at most
        // as many as were asked for. A client gets their own whatever they ask.
        let client = if fake.is_staff() {
            filter.client_id
        } else {
            Some(fake.user.id)
        };
        let mut listed: Vec<Order> = fake
            .orders
            .iter()
            .filter(|order| client.is_none_or(|client| order.client_id == client))
            .filter(|order| {
                filter
                    .service_date
                    .is_none_or(|day| order.service_date == day)
            })
            .filter(|order| filter.status.is_none_or(|status| order.status == status))
            .filter(|order| filter.after.is_none_or(|after| order.id > after))
            .cloned()
            .collect();
        listed.sort_by_key(|order| order.id);
        listed.truncate(filter.limit.unwrap_or(100) as usize);
        Ok(listed)
    }

    async fn get_order(&self, who: &Caller<'_>, id: OrderId) -> Result<Order, Failure> {
        let mut fake = self.fake();
        fake.take(Asked::GetOrder {
            token: who.token.to_owned(),
            id,
        })?;
        let visible = |order: &&Order| fake.is_staff() || order.client_id == fake.user.id;
        fake.orders
            .iter()
            .filter(visible)
            .find(|order| order.id == id)
            .cloned()
            .ok_or_else(|| refused(404, "there is no such order"))
    }

    async fn cancel_order(&self, who: &Caller<'_>, id: OrderId) -> Result<Order, Failure> {
        let mut fake = self.fake();
        fake.take(Asked::CancelOrder {
            token: who.token.to_owned(),
            id,
        })?;
        // Somebody else's order is not there for a client, to withdraw as to read.
        let (staff, me) = (fake.is_staff(), fake.user.id);
        let mine = |order: &&mut Order| staff || order.client_id == me;
        let Some(order) = fake
            .orders
            .iter_mut()
            .filter(mine)
            .find(|order| order.id == id)
        else {
            return Err(refused(404, "there is no such order"));
        };
        // The API's own words for each.
        match order.status {
            OrderStatus::Pending => {
                order.status = OrderStatus::Cancelled;
                Ok(order.clone())
            }
            OrderStatus::Planned => Err(refused(
                409,
                "a plan has given the order to a courier: it can no longer be withdrawn",
            )),
            status => Err(refused(
                409,
                &format!("the order is {status} and cannot be withdrawn"),
            )),
        }
    }

    async fn create_plan(&self, who: &Caller<'_>, plan: &PlanRequest) -> Result<Plan, Failure> {
        let mut fake = self.fake();
        fake.take(Asked::CreatePlan {
            token: who.token.to_owned(),
            request_id: who.request_id.to_owned(),
            plan: *plan,
        })?;
        if !fake.is_staff() {
            return Err(refused(403, "your role does not allow this"));
        }
        let in_progress =
            |known: &Plan| known.service_date == plan.service_date && known.status.is_active();
        if fake.plans.iter().any(in_progress) {
            return Err(refused(
                409,
                "the service date already has a plan in progress",
            ));
        }
        let stored = a_plan(plan.service_date, fake.user.id);
        fake.plans.push(stored.clone());
        Ok(stored)
    }

    async fn list_plans(&self, who: &Caller<'_>, day: NaiveDate) -> Result<Vec<Plan>, Failure> {
        let mut fake = self.fake();
        fake.take(Asked::ListPlans {
            token: who.token.to_owned(),
            day,
        })?;
        if !fake.is_staff() {
            return Err(refused(403, "your role does not allow this"));
        }
        let mut listed: Vec<Plan> = fake
            .plans
            .iter()
            .filter(|plan| plan.service_date == day)
            .cloned()
            .collect();
        // Newest first.
        listed.sort_by_key(|plan| std::cmp::Reverse(plan.id));
        Ok(listed)
    }
}

// ------------------------------------------------------------------ over HTTP

/// The method and the path the API's own document gives an operation: what the bot
/// has to ask with, read from the file the API's tests hold the API to (D43).
///
/// A path has `{id}` where an id goes.
pub(crate) fn documented(operation_id: &str) -> (String, String) {
    let file = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../routeforge-api/openapi.json"
    );
    let document: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    for (path, item) in document["paths"].as_object().unwrap() {
        for (method, operation) in item.as_object().unwrap() {
            if operation["operationId"] == operation_id {
                return (method.to_uppercase(), path.clone());
            }
        }
    }
    panic!("the API's document has no operation `{operation_id}`");
}

/// The token the stand-in for Telegram knows its bot by.
pub(crate) const BOT_TOKEN: &str = "424242:a-token-made-up-for-the-tests";

/// The one person who writes to the bot in a test, and their chat.
pub(crate) const ADA: i64 = 4_242;

/// A user of Telegram, as Telegram describes one.
fn person(id: i64) -> Value {
    json!({ "id": id, "is_bot": false, "first_name": "Ada" })
}

fn private_chat(id: i64) -> Value {
    json!({ "id": id, "type": "private", "first_name": "Ada" })
}

/// An update with a message that `content` is merged into: a text, a location.
pub(crate) fn message_update(update_id: u32, message_id: i32, content: Value) -> Value {
    message_update_in(update_id, message_id, private_chat(ADA), content)
}

/// The same in a chat of the test's choosing.
pub(crate) fn message_update_in(
    update_id: u32,
    message_id: i32,
    chat: Value,
    content: Value,
) -> Value {
    let mut message = json!({
        "message_id": message_id,
        "date": 1_760_000_000,
        "chat": chat,
        "from": person(ADA),
    });
    for (key, value) in content.as_object().unwrap() {
        message[key] = value.clone();
    }
    json!({ "update_id": update_id, "message": message })
}

/// An update with the press of a button under the bot's message `on`.
pub(crate) fn press_update(update_id: u32, on: i32, data: &str) -> Value {
    json!({
        "update_id": update_id,
        "callback_query": {
            "id": format!("press-{update_id}"),
            "from": person(ADA),
            "message": a_message_of_the_bot(on, ADA, "what the button was under"),
            "chat_instance": "1",
            "data": data,
        },
    })
}

/// A message as Telegram answers with when the bot has sent or changed one.
fn a_message_of_the_bot(message_id: i32, chat: i64, text: &str) -> Value {
    json!({
        "message_id": message_id,
        "date": 1_760_000_000,
        "chat": private_chat(chat),
        "from": { "id": 777, "is_bot": true, "first_name": "RouteForge", "username": "routeforge_test_bot" },
        "text": text,
    })
}

fn ok(result: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "ok": true, "result": result }))
}

/// How Telegram refuses: with a status, and a description that says why.
pub(crate) fn telegram_says_no(status: u16, description: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(json!({
        "ok": false,
        "error_code": status,
        "description": description,
    }))
}

/// The id every message has that the stand-in says the bot sent.
pub(crate) const SENT: i32 = 100;

/// A server in the place of Telegram's Bot API.
///
/// It knows the bot, hands out the updates a test gave it, as Telegram does: those
/// from the offset on that is asked for, which is how the bot says what it has had.
/// And it takes whatever the bot sends, answering as Telegram would.
pub(crate) struct Telegram {
    pub(crate) server: MockServer,
    /// What the next thing a test says of a method is ranked with. The server asks
    /// the lowest rank first, so what was said last is what counts.
    rank: std::sync::atomic::AtomicU8,
}

/// Hands out updates the way `getUpdates` does.
struct Updates(Vec<Value>);

impl Respond for Updates {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let asked: Value = request.body_json().unwrap_or_default();
        let offset = asked["offset"].as_i64().unwrap_or(0);
        let limit = asked["limit"].as_u64().unwrap_or(100) as usize;
        let due: Vec<&Value> = self
            .0
            .iter()
            .filter(|update| update["update_id"].as_i64().unwrap() >= offset)
            .take(limit)
            .collect();
        if due.is_empty() {
            // Telegram would keep the request open for as long as it was asked to.
            // A moment is enough to keep the bot from asking in a tight loop.
            ok(json!([])).set_delay(Duration::from_millis(50))
        } else {
            ok(json!(due))
        }
    }
}

/// Answers `sendMessage` and `editMessageText` with the message that came of it.
struct Sent;

impl Respond for Sent {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let asked: Value = request.body_json().unwrap_or_default();
        let chat = asked["chat_id"].as_i64().unwrap_or(ADA);
        let text = asked["text"].as_str().unwrap_or_default();
        ok(a_message_of_the_bot(SENT, chat, text))
    }
}

impl Telegram {
    /// A Telegram that knows the bot and has these updates for it.
    pub(crate) async fn with(updates: Vec<Value>) -> Self {
        let telegram = Self::unknowing().await;
        telegram
            .answers(
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
        telegram.answers("getUpdates", Updates(updates)).await;
        telegram
    }

    /// A Telegram that answers everything but who the bot is, and updates: a test
    /// says what it answers those with.
    pub(crate) async fn unknowing() -> Self {
        let telegram = Self {
            server: MockServer::start().await,
            rank: std::sync::atomic::AtomicU8::new(250),
        };
        let no_webhook = json!({
            "url": "",
            "has_custom_certificate": false,
            "pending_update_count": 0,
        });
        telegram.answers("getWebhookInfo", ok(no_webhook)).await;
        telegram.answers("setMyCommands", ok(json!(true))).await;
        telegram
            .answers("answerCallbackQuery", ok(json!(true)))
            .await;
        telegram.answers("sendMessage", Sent).await;
        telegram.answers("editMessageText", Sent).await;
        telegram
    }

    /// From now on a method of the Bot API is answered with this. What was said of
    /// it before no longer counts.
    pub(crate) async fn answers(&self, bot_method: &str, with: impl Respond + 'static) {
        self.answers_times(bot_method, with, u64::MAX).await;
    }

    /// The next `times` calls of a method are answered with this, and the ones after
    /// them as they were before.
    pub(crate) async fn answers_times(
        &self,
        bot_method: &str,
        with: impl Respond + 'static,
        times: u64,
    ) {
        // Telegram has the token in the path, so a bot with another token is one it
        // does not know. It takes the name of a method in any case, and teloxide
        // writes `GetMe` where the documentation writes `getMe`.
        let at = format!("(?i)^/bot{}/{bot_method}$", regex_literal(BOT_TOKEN));
        Mock::given(method("POST"))
            .and(path_regex(at))
            .respond_with(with)
            .up_to_n_times(times)
            .with_priority(self.rank.fetch_sub(1, std::sync::atomic::Ordering::Relaxed))
            .mount(&self.server)
            .await;
    }

    pub(crate) fn url(&self) -> reqwest::Url {
        self.server.uri().parse().unwrap()
    }

    /// What the bot asked of a method of the Bot API, oldest first: the bodies.
    pub(crate) async fn asked(&self, bot_method: &str) -> Vec<Value> {
        let at = format!("/bot{BOT_TOKEN}/{bot_method}");
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path().eq_ignore_ascii_case(&at))
            .map(|request| request.body_json().unwrap_or_default())
            .collect()
    }

    /// Waits until the bot has asked a method at least this often, and gives what it
    /// asked.
    pub(crate) async fn until_asked(&self, bot_method: &str, times: usize) -> Vec<Value> {
        soon(async {
            loop {
                let asked = self.asked(bot_method).await;
                if asked.len() >= times {
                    return asked;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
    }
}

/// A text as a regular expression that matches it and nothing else.
fn regex_literal(text: &str) -> String {
    text.chars()
        .flat_map(|letter| {
            let escape = (!letter.is_ascii_alphanumeric()).then_some('\\');
            escape.into_iter().chain([letter])
        })
        .collect()
}
