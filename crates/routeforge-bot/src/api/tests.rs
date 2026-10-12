//! The bot's requests as they go over HTTP, against a server in the place of the API.
//!
//! The server answers an operation at the method and the path the API's own document
//! gives it, and nowhere else. So a request the API would not know is not answered
//! here either, and the bot cannot drift from the API without a test saying so.

use std::collections::BTreeMap;
use std::net::TcpListener;

use routeforge_domain::{FieldError, OrderStatus, Role, UserId};
use serde::Serialize;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;
use crate::testing::{a_delivery, a_depot, a_plan, a_user, an_order, date, documented};

const KEY: &str = "the key the bot shows, in the tests of its requests";
const TOKEN: &str = "the.token.of-the-person";
const WHO: Caller<'static> = Caller {
    token: TOKEN,
    request_id: "tg-7",
};

fn api(server: &MockServer) -> HttpApi {
    HttpApi::new(&server.uri(), KEY).unwrap()
}

/// Has the server answer one operation of the API's, at the method and the path its
/// document gives it, with the id filled in where the path has one.
async fn answers(
    server: &MockServer,
    operation: &str,
    id: Option<OrderId>,
    with: ResponseTemplate,
) {
    let (verb, template) = documented(operation);
    let at = match id {
        Some(id) => template.replace("{id}", &id.to_string()),
        None => template,
    };
    assert!(!at.contains('{'), "{at} still wants an id");
    Mock::given(method(verb.as_str()))
        .and(path(at))
        .respond_with(with)
        .mount(server)
        .await;
}

fn sent(status: u16, body: &impl Serialize) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(body)
}

/// A refusal as the API sends one (D39).
fn problem(status: u16, detail: Option<&str>, errors: &[(&str, &str)]) -> ResponseTemplate {
    let problem = Problem {
        kind: "about:blank".to_owned(),
        title: "What The Status Says".to_owned(),
        status,
        detail: detail.map(str::to_owned),
        errors: errors
            .iter()
            .map(|(field, message)| FieldError {
                field: (*field).to_owned(),
                message: (*message).to_owned(),
            })
            .collect(),
    };
    let body = serde_json::to_vec(&problem).unwrap();
    ResponseTemplate::new(status).set_body_raw(body, "application/problem+json")
}

/// The one request the server has had since it was last asked.
async fn asked(server: &MockServer) -> Request {
    let mut requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "one request for one question");
    requests.remove(0)
}

fn header<'a>(request: &'a Request, name: &str) -> &'a str {
    request
        .headers
        .get(name)
        .unwrap_or_else(|| panic!("no {name} on the request"))
        .to_str()
        .unwrap()
}

/// The one request the server has had, which has to have come as the person: with
/// their token and never with the bot's key, under the id of the update it came of.
async fn asked_as_the_person(server: &MockServer) -> Request {
    let request = asked(server).await;
    assert_eq!(header(&request, "authorization"), format!("Bearer {TOKEN}"));
    assert_eq!(header(&request, X_REQUEST_ID), "tg-7");
    request
}

/// The one thing the bot does in its own name (D60).
#[tokio::test]
async fn the_bot_shows_its_key_to_be_given_a_token_of_a_telegram_user() {
    let server = MockServer::start().await;
    let session = Session {
        token: TOKEN.to_owned(),
        expires_in: 60,
        user: a_user(Role::Operator, 4_242),
    };
    answers(&server, "telegram_login", None, sent(200, &session)).await;

    let given = api(&server).telegram_login(4_242, "tg-7").await.unwrap();
    assert_eq!(given, session);

    let request = asked(&server).await;
    assert_eq!(header(&request, "authorization"), format!("Bearer {KEY}"));
    assert_eq!(header(&request, X_REQUEST_ID), "tg-7");
    assert_eq!(header(&request, "content-type"), "application/json");
    assert_eq!(
        request.body_json::<Value>().unwrap(),
        json!({ "telegram_id": 4_242 })
    );
}

#[tokio::test]
async fn everything_else_is_asked_with_the_token_of_the_person() {
    let server = MockServer::start().await;
    // An address with a slash at its end is the same address.
    let api = HttpApi::new(&format!("{}/", server.uri()), KEY).unwrap();
    let me = UserId::generate();
    let order = an_order(me, &a_delivery(date(13), 2));
    let plan = a_plan(date(13), me);

    answers(&server, "create_order", None, sent(201, &order)).await;
    let placed = api.create_order(&WHO, &a_delivery(date(13), 2)).await;
    assert_eq!(placed.unwrap(), order);
    let request = asked_as_the_person(&server).await;
    assert_eq!(
        request.body_json::<Value>().unwrap(),
        json!({
            "kind": "delivery",
            "service_date": "2026-10-13",
            "location": { "lat": 50.45, "lon": 30.52 },
            "address": null,
            "demand": 2,
        })
    );
    server.reset().await;

    answers(&server, "get_order", Some(order.id), sent(200, &order)).await;
    assert_eq!(api.get_order(&WHO, order.id).await.unwrap(), order);
    assert!(asked_as_the_person(&server).await.body.is_empty());
    server.reset().await;

    let withdrawn = Order {
        status: OrderStatus::Cancelled,
        ..order.clone()
    };
    answers(
        &server,
        "cancel_order",
        Some(order.id),
        sent(200, &withdrawn),
    )
    .await;
    assert_eq!(api.cancel_order(&WHO, order.id).await.unwrap(), withdrawn);
    assert!(asked_as_the_person(&server).await.body.is_empty());
    server.reset().await;

    answers(&server, "create_plan", None, sent(201, &plan)).await;
    let day = PlanRequest {
        service_date: date(13),
        seed: None,
        budget: None,
    };
    assert_eq!(api.create_plan(&WHO, &day).await.unwrap(), plan);
    let request = asked_as_the_person(&server).await;
    assert_eq!(
        request.body_json::<Value>().unwrap(),
        json!({ "service_date": "2026-10-13", "seed": null, "budget": null })
    );
    server.reset().await;

    answers(&server, "list_plans", None, sent(200, &[&plan])).await;
    assert_eq!(api.list_plans(&WHO, date(13)).await.unwrap(), [plan]);
    let request = asked_as_the_person(&server).await;
    assert_eq!(request.url.query(), Some("service_date=2026-10-13"));
}

/// Where the depot is, which the bot asks so that an address is looked for near it.
#[tokio::test]
async fn the_depot_is_read_as_the_person_and_one_that_is_not_set_is_none() {
    let server = MockServer::start().await;
    let api = api(&server);
    let depot = a_depot();
    answers(&server, "get_depot", None, sent(200, &depot)).await;
    assert_eq!(api.get_depot(&WHO).await.unwrap(), Some(depot));
    assert!(asked_as_the_person(&server).await.body.is_empty());
    server.reset().await;

    // What the API answers before an admin has set one.
    let not_yet = problem(404, Some("no depot has been set yet"), &[]);
    answers(&server, "get_depot", None, not_yet).await;
    assert_eq!(api.get_depot(&WHO).await.unwrap(), None);
    server.reset().await;

    // Any other refusal is a refusal.
    let stale = problem(401, Some("the token is not valid"), &[]);
    answers(&server, "get_depot", None, stale).await;
    let refused = api.get_depot(&WHO).await.unwrap_err();
    assert!(
        matches!(refused, Failure::Refused { status: 401, .. }),
        "{refused:?}"
    );
}

#[tokio::test]
async fn what_narrows_a_list_of_orders_travels_in_the_query_and_nothing_else_does() {
    let server = MockServer::start().await;
    let api = api(&server);
    let me = UserId::generate();
    let orders = [
        an_order(me, &a_delivery(date(13), 1)),
        an_order(me, &a_delivery(date(13), 2)),
    ];
    answers(&server, "list_orders", None, sent(200, &orders)).await;

    let after = OrderId::generate();
    let narrowed = OrderFilter {
        client_id: Some(me),
        service_date: Some(date(13)),
        status: Some(OrderStatus::Pending),
        after: Some(after),
        limit: Some(1_000),
    };
    assert_eq!(api.list_orders(&WHO, &narrowed).await.unwrap(), orders);
    let request = asked_as_the_person(&server).await;
    let query: BTreeMap<String, String> = request.url.query_pairs().into_owned().collect();
    assert_eq!(
        query,
        BTreeMap::from([
            ("after".to_owned(), after.to_string()),
            ("client_id".to_owned(), me.to_string()),
            ("limit".to_owned(), "1000".to_owned()),
            ("service_date".to_owned(), "2026-10-13".to_owned()),
            ("status".to_owned(), "pending".to_owned()),
        ])
    );
    server.reset().await;

    // What is not set is not sent: the API would read an empty value as a wrong one.
    answers(&server, "list_orders", None, sent(200, &orders)).await;
    let every = OrderFilter::default();
    assert_eq!(api.list_orders(&WHO, &every).await.unwrap(), orders);
    let request = asked_as_the_person(&server).await;
    assert_eq!(request.url.query().unwrap_or_default(), "");
}

#[tokio::test]
async fn a_refusal_of_the_apis_is_read_as_the_problem_it_is() {
    let server = MockServer::start().await;
    let api = api(&server);
    let id = OrderId::generate();
    let held = "a plan has given the order to a courier: it can no longer be withdrawn";
    let fields = [
        ("demand", "must be at least 1"),
        ("address", "must be at most 300 characters long"),
    ];

    for (status, refusal, said) in [
        (409, problem(409, Some(held), &[]), held),
        (
            404,
            problem(404, Some("there is no such order"), &[]),
            "there is no such order",
        ),
        (
            403,
            problem(403, Some("your role does not allow this"), &[]),
            "your role does not allow this",
        ),
        (
            422,
            problem(
                422,
                Some("the request has fields that cannot be accepted"),
                &fields,
            ),
            "the request has fields that cannot be accepted; demand must be at least 1; \
             address must be at most 300 characters long",
        ),
        // A problem that says no more than its status has its title for a detail.
        (500, problem(500, None, &[]), "What The Status Says"),
    ] {
        answers(&server, "cancel_order", Some(id), refusal).await;
        let refused = api.cancel_order(&WHO, id).await.unwrap_err();
        assert_eq!(
            refused,
            Failure::Refused {
                status,
                detail: said.to_owned(),
            }
        );
        server.reset().await;
    }
}

/// Something between the bot and the API may answer in the API's place, and a path
/// the API does not serve is answered by whatever is there.
#[tokio::test]
async fn a_refusal_that_is_no_problem_has_its_status_to_go_by() {
    let server = MockServer::start().await;
    let api = api(&server);
    let id = OrderId::generate();
    let gateway = ResponseTemplate::new(502).set_body_string("<html>Bad Gateway</html>");
    answers(&server, "get_order", Some(id), gateway).await;

    let refused = api.get_order(&WHO, id).await.unwrap_err();
    assert_eq!(
        refused,
        Failure::Refused {
            status: 502,
            detail: "bad gateway".to_owned(),
        }
    );
    // Nothing answers for any other order, as the API answers for no other path.
    let missed = api.get_order(&WHO, OrderId::generate()).await.unwrap_err();
    assert_eq!(
        missed,
        Failure::Refused {
            status: 404,
            detail: "not found".to_owned(),
        }
    );
}

#[tokio::test]
async fn an_api_that_does_not_answer_is_told_from_one_that_refuses() {
    // Nobody listens where the API was said to be.
    let nobody = TcpListener::bind("127.0.0.1:0").unwrap();
    let nowhere = format!("http://{}", nobody.local_addr().unwrap());
    drop(nobody);
    let away = HttpApi::new(&nowhere, KEY)
        .unwrap()
        .telegram_login(4_242, "tg-7")
        .await
        .unwrap_err();
    assert!(matches!(away, Failure::Away(_)), "{away:?}");

    // An answer that says yes and is not what was asked for.
    let server = MockServer::start().await;
    let id = OrderId::generate();
    let welcome = ResponseTemplate::new(200).set_body_string("<html>Welcome</html>");
    answers(&server, "get_order", Some(id), welcome).await;
    let unread = api(&server).get_order(&WHO, id).await.unwrap_err();
    assert!(
        matches!(&unread, Failure::Away(why) if why.contains("cannot be read")),
        "{unread:?}"
    );
    server.reset().await;

    // An answer that takes longer than the person is made to wait.
    let me = UserId::generate();
    let late = sent(200, &an_order(me, &a_delivery(date(13), 1))).set_delay(Duration::from_secs(5));
    answers(&server, "get_order", Some(id), late).await;
    let patient_for = Duration::from_millis(200);
    let slow = HttpApi::within(&server.uri(), KEY, patient_for, patient_for).unwrap();
    let given_up = slow.get_order(&WHO, id).await.unwrap_err();
    assert!(matches!(given_up, Failure::Away(_)), "{given_up:?}");

    // Whatever went wrong, neither the key nor the token is in what is said of it.
    for failure in [away, unread, given_up] {
        let said = format!("{failure} {failure:?}");
        assert!(!said.contains(KEY) && !said.contains(TOKEN), "{said}");
    }
}
