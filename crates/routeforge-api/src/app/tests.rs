//! What holds for the application as a whole: who gets in where, how it describes
//! itself, the id every request carries, and how it refuses what it cannot read.

use std::collections::BTreeSet;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use actix_web::http::StatusCode;
use actix_web::http::header;
use routeforge_domain::Role;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::OPENAPI_PATH;
use crate::auth::Tokens;
use crate::error::PROBLEM_JSON;
use crate::request_id::X_REQUEST_ID;
use crate::testing::{self, Actor, Api, Reply, date};

/// The least a caller has to be for an endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Needs {
    /// Nothing: no token is asked for.
    Nobody,
    /// A token, of any role.
    Anyone,
    /// An operator or an admin.
    Staff,
    Admin,
}

/// Every endpoint, and who may call it. This is the table the API is held to.
///
/// It is written out by hand and not read from the code, so that a gate that was
/// changed by mistake does not change what is expected along with it.
const MATRIX: &[(&str, &str, Needs)] = &[
    ("get", "/healthz", Needs::Nobody),
    ("get", "/readyz", Needs::Nobody),
    ("post", "/api/auth/register", Needs::Nobody),
    ("post", "/api/auth/login", Needs::Nobody),
    ("post", "/api/orders", Needs::Anyone),
    ("get", "/api/orders", Needs::Anyone),
    ("get", "/api/orders/{id}", Needs::Anyone),
    ("post", "/api/orders/{id}/cancel", Needs::Anyone),
    ("get", "/api/depot", Needs::Anyone),
    ("put", "/api/depot", Needs::Admin),
    ("post", "/api/couriers", Needs::Staff),
    ("get", "/api/couriers", Needs::Staff),
    ("get", "/api/couriers/{id}", Needs::Staff),
    ("patch", "/api/couriers/{id}", Needs::Staff),
    ("get", "/api/users", Needs::Admin),
    ("get", "/api/users/{id}", Needs::Admin),
    ("put", "/api/users/{id}/role", Needs::Admin),
    ("post", "/api/plans", Needs::Staff),
    ("get", "/api/plans", Needs::Staff),
    ("get", "/api/plans/{id}", Needs::Staff),
    ("get", "/api/plans/{id}/routes", Needs::Staff),
    ("get", "/api/plans/{id}/events", Needs::Staff),
    ("post", "/api/plans/{id}/cancel", Needs::Staff),
    ("post", "/api/plans/{id}/discard", Needs::Staff),
    ("get", "/api/system/stats", Needs::Staff),
    ("get", "/api/system/stats/events", Needs::Staff),
    ("post", "/api/demo/orders", Needs::Admin),
];

impl Needs {
    /// Whether a caller with this role, or with no token at all, is let in.
    fn lets_in(self, caller: Option<Role>) -> bool {
        match (self, caller) {
            (Self::Nobody, _) => true,
            (_, None) => false,
            (Self::Anyone, Some(_)) => true,
            (Self::Staff, Some(role)) => matches!(role, Role::Operator | Role::Admin),
            (Self::Admin, Some(role)) => role == Role::Admin,
        }
    }
}

async fn document(api: &Api) -> Value {
    let served = api.get(None, OPENAPI_PATH).await;
    assert_eq!(served.status, StatusCode::OK);
    served.body
}

/// The operations a document describes, as (method, path).
fn operations(document: &Value) -> BTreeSet<(String, String)> {
    let paths = document["paths"].as_object().expect("a document has paths");
    let mut found = BTreeSet::new();
    for (path, item) in paths {
        for method in ["get", "post", "put", "patch", "delete", "head", "options"] {
            if item.get(method).is_some() {
                found.insert((method.to_owned(), path.clone()));
            }
        }
    }
    found
}

/// An endpoint that is added without saying who may call it fails here.
#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn every_endpoint_has_a_row_in_the_matrix(pool: PgPool) {
    let api = testing::api(&pool);
    let document = document(&api).await;

    let served = operations(&document);
    let listed: BTreeSet<(String, String)> = MATRIX
        .iter()
        .map(|(method, path, _)| ((*method).to_owned(), (*path).to_owned()))
        .collect();
    assert_eq!(listed.len(), MATRIX.len(), "a row is there twice");
    assert_eq!(served, listed);

    // The document asks for a token exactly where the API does, so that Swagger UI
    // sends one there.
    for (method, path, needs) in MATRIX {
        let operation = &document["paths"][path][method];
        assert_eq!(
            operation["security"] == json!([{ "bearer": [] }]),
            *needs != Needs::Nobody,
            "{method} {path}"
        );
        // And says what a caller who is kept out is answered.
        let documented = |status: &str| operation["responses"].get(status).is_some();
        assert!(
            documented("401") || *needs == Needs::Nobody,
            "{method} {path}"
        );
        assert_eq!(
            documented("403"),
            matches!(needs, Needs::Staff | Needs::Admin),
            "{method} {path}"
        );
    }
}

/// Each role against each endpoint: nobody gets past a gate they have no key to, and
/// everybody gets past the ones they have.
#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn each_role_gets_in_where_it_may_and_nowhere_else(pool: PgPool) {
    let api = testing::api(&pool);
    let mut callers: Vec<(Option<Role>, Option<Actor>)> = vec![(None, None)];
    for &role in Role::ALL {
        callers.push((Some(role), Some(api.actor(role).await)));
    }

    for (method, path, needs) in MATRIX {
        for (role, actor) in &callers {
            // An id that names nothing, and a body that says nothing. The gate comes
            // before either is looked at.
            let path = path.replace("{id}", &Uuid::now_v7().to_string());
            let body = if *method == "get" {
                Value::Null
            } else {
                json!({})
            };
            let reply = api
                .with_body(&method.to_uppercase(), actor.as_ref(), &path, body)
                .await;

            let who = role.map_or("nobody".to_owned(), |role| role.to_string());
            let outcome = format!("{method} {path} as {who}: {}", reply.status);
            if needs.lets_in(*role) {
                assert!(
                    !matches!(
                        reply.status,
                        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
                    ),
                    "{outcome}"
                );
                assert!(!reply.status.is_server_error(), "{outcome}");
            } else {
                let kept_out = if role.is_none() {
                    StatusCode::UNAUTHORIZED
                } else {
                    StatusCode::FORBIDDEN
                };
                assert_eq!(reply.status, kept_out, "{outcome}");
                assert_eq!(
                    reply.content_type.as_deref(),
                    Some(PROBLEM_JSON),
                    "{outcome}"
                );
            }
        }
    }
}

#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn a_request_without_a_good_token_is_told_to_log_in(pool: PgPool) {
    let api = testing::api(&pool);
    let client = api.actor(Role::Client).await;
    let with = async |authorization: Option<String>| {
        let request = testing::request("GET", "/api/orders", None);
        let request = match authorization {
            Some(value) => request.insert_header((header::AUTHORIZATION, value)),
            None => request,
        };
        api.send(request).await
    };

    // What another API would give an admin: well made, and signed with another key.
    let elsewhere = Tokens::new(b"the key of some other deployment", Duration::from_secs(60));
    let forged = elsewhere.issue(client.user.clone()).unwrap().token;

    let none = with(None).await;
    assert_eq!(none.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        none.headers.get(header::WWW_AUTHENTICATE).unwrap(),
        "Bearer"
    );
    assert_eq!(none.body["title"], "Unauthorized");
    assert!(none.body["detail"].as_str().unwrap().contains("log in"));

    for bad in [
        "Bearer".to_owned(),
        "Bearer ".to_owned(),
        "Bearer not-a-token".to_owned(),
        format!("Basic {}", client.token),
        client.token.clone(),
        format!("Bearer {forged}"),
    ] {
        let refused = with(Some(bad.clone())).await;
        assert_eq!(refused.status, StatusCode::UNAUTHORIZED, "{bad}");
    }

    // The scheme is not case-sensitive, and the token is.
    for good in [
        format!("Bearer {}", client.token),
        format!("bearer {}", client.token),
    ] {
        assert_eq!(with(Some(good)).await.status, StatusCode::OK);
    }
}

#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn an_account_is_opened_and_logged_in_to(pool: PgPool) {
    let api = testing::api(&pool);
    let ada = json!({ "email": "Ada@Example.com", "password": "correct horse" });

    let registered = api.post(None, "/api/auth/register", ada.clone()).await;
    assert_eq!(registered.status, StatusCode::CREATED);
    assert_eq!(registered.body["email"], "ada@example.com");
    // Whoever registers is a client, whatever they say they are.
    let mut pushy = json!({ "email": "eve@example.com", "password": "correct horse" });
    pushy["role"] = json!("admin");
    let eve = api.post(None, "/api/auth/register", pushy).await;
    assert_eq!(eve.body["role"], "client");
    // Nothing of the password comes back.
    assert!(!registered.text.contains("horse") && !registered.text.contains("argon"));

    // The address is one account, however it is typed.
    let again = json!({ "email": "ADA@example.com", "password": "another one" });
    let taken = api.post(None, "/api/auth/register", again).await;
    assert_eq!(taken.status, StatusCode::CONFLICT);
    assert_eq!(taken.content_type.as_deref(), Some(PROBLEM_JSON));

    let session = api.post(None, "/api/auth/login", ada).await;
    assert_eq!(session.status, StatusCode::OK);
    assert_eq!(session.body["expires_in"], 3_600);
    assert_eq!(session.body["user"], registered.body);
    // The token it gives opens what a client may open, and is hers.
    let token = session.body["token"].as_str().unwrap();
    let request = testing::request("POST", "/api/orders", None)
        .insert_header((header::AUTHORIZATION, format!("Bearer {token}")))
        .set_json(json!({
            "kind": "pickup",
            "service_date": date(12),
            "location": { "lat": 50.45, "lon": 30.52 },
            "demand": 1,
        }));
    let placed = api.send(request).await;
    assert_eq!(placed.status, StatusCode::CREATED);
    assert_eq!(placed.body["client_id"], registered.body["id"]);

    // A wrong password and an address nobody has are refused alike.
    let wrong = json!({ "email": "ada@example.com", "password": "wrong horse" });
    let nobody = json!({ "email": "nobody@example.com", "password": "correct horse" });
    let wrong = api.post(None, "/api/auth/login", wrong).await;
    let nobody = api.post(None, "/api/auth/login", nobody).await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(nobody.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.body, nobody.body);

    // An account needs an address and a password worth the name.
    let weak = json!({ "email": "not an address", "password": "short" });
    let refused = api.post(None, "/api/auth/register", weak).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        refused.body["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|error| error["field"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["email", "password"]
    );
}

#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn every_response_carries_the_id_of_its_request(pool: PgPool) {
    let api = testing::api(&pool);
    let id_of = |reply: &Reply| {
        let id = reply
            .headers
            .get(X_REQUEST_ID)
            .expect("an id on the response");
        id.to_str().unwrap().to_owned()
    };
    let sent_with = async |path: &str, id: &str| {
        let request = testing::request("GET", path, None).insert_header((X_REQUEST_ID, id));
        api.send(request).await
    };

    // One is made for a request that brings none: a UUIDv7, and a new one each time.
    let first = id_of(&api.get(None, "/healthz").await);
    let second = id_of(&api.get(None, "/healthz").await);
    assert_ne!(first, second);
    assert_eq!(first.parse::<Uuid>().unwrap().get_version_num(), 7);

    // One that is brought is kept, on a refusal as on an answer.
    assert_eq!(
        id_of(&sent_with("/healthz", "demo-run_7.a").await),
        "demo-run_7.a"
    );
    let refused = sent_with("/api/orders", "demo-run_7.b").await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert_eq!(id_of(&refused), "demo-run_7.b");
    let missed = sent_with("/no/such/path", "demo-run_7.c").await;
    assert_eq!(missed.status, StatusCode::NOT_FOUND);
    assert_eq!(id_of(&missed), "demo-run_7.c");

    // One that could do harm in a log is replaced.
    for harmful in ["two words", "a;b", &"x".repeat(65)] {
        let made = id_of(&sent_with("/healthz", harmful).await);
        assert_ne!(made, harmful);
        assert!(made.parse::<Uuid>().is_ok(), "{made}");
    }
}

#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn what_cannot_be_read_or_found_is_refused_as_a_problem(pool: PgPool) {
    let api = testing::api(&pool);
    let client = api.actor(Role::Client).await;
    let order = json!({
        "kind": "delivery",
        "service_date": date(12),
        "location": { "lat": 50.45, "lon": 30.52 },
        "demand": 1,
    });
    let post = |body: &str, content_type: &str| {
        testing::request("POST", "/api/orders", Some(&client))
            .insert_header((header::CONTENT_TYPE, content_type))
            .set_payload(body.to_owned())
    };

    let cases = [
        // Not JSON at all, and JSON that stops half way.
        (post("an order, please", "application/json"), 400),
        (post(r#"{"kind": "delivery""#, "application/json"), 400),
        // JSON that is not said to be JSON.
        (post(&order.to_string(), "text/plain"), 415),
        // More than the API reads.
        (
            post(
                &json!({ "address": "x".repeat(70_000) }).to_string(),
                "application/json",
            ),
            413,
        ),
        // A path nothing serves, and a method a path does not take.
        (testing::request("GET", "/api/parcels", Some(&client)), 404),
        (
            testing::request("DELETE", "/api/orders", Some(&client)),
            404,
        ),
        // An id that is none.
        (
            testing::request("GET", "/api/orders/the-first-one", Some(&client)),
            400,
        ),
    ];
    for (request, status) in cases {
        let reply = api.send(request).await;
        assert_eq!(reply.status.as_u16(), status, "{}", reply.text);
        assert_eq!(
            reply.content_type.as_deref(),
            Some(PROBLEM_JSON),
            "{status}"
        );
        assert_eq!(reply.body["status"], status);
        assert_eq!(reply.body["type"], "about:blank");
        assert!(reply.body["detail"].is_string(), "{}", reply.text);
    }

    // And what can be read is taken.
    let taken = api.send(post(&order.to_string(), "application/json")).await;
    assert_eq!(taken.status, StatusCode::CREATED);
}

#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn swagger_ui_comes_out_of_the_binary(pool: PgPool) {
    let api = testing::api(&pool);

    let bare = api.get(None, "/docs").await;
    assert!(bare.status.is_redirection(), "{}", bare.status);
    assert_eq!(bare.headers.get(header::LOCATION).unwrap(), "/docs/");

    let page = api.get(None, "/docs/").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.text.contains("swagger-ui"), "{}", page.text);
    // The page asks for nothing from another host: the demo works without a network.
    assert!(!page.text.contains("http://") && !page.text.contains("https://"));

    let setup = api.get(None, "/docs/swagger-initializer.js").await;
    assert!(setup.text.contains(OPENAPI_PATH), "{}", setup.text);
    for asset in ["swagger-ui-bundle.js", "swagger-ui.css"] {
        let served = api.get(None, &format!("/docs/{asset}")).await;
        assert_eq!(served.status, StatusCode::OK, "{asset}");
        assert!(served.text.len() > 10_000, "{asset}");
    }
}

/// The document is the API's contract with whoever calls it: the page of Phase 5, the
/// bot of Phase 7. A change to it is a change to them, so it is committed and a
/// change shows up in review.
#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn the_document_is_the_one_that_is_committed(pool: PgPool) {
    let api = testing::api(&pool);
    let mut served = serde_json::to_string_pretty(&document(&api).await).unwrap();
    served.push('\n');

    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("openapi.json");
    if std::env::var_os("UPDATE_OPENAPI").is_some() {
        std::fs::write(&path, &served).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        served == committed,
        "the OpenAPI document is no longer the one in {}.\n\
         If the change is meant, run this test again with UPDATE_OPENAPI=1 and commit the file.",
        path.display()
    );
}

#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn the_document_describes_what_the_api_takes_and_gives(pool: PgPool) {
    let api = testing::api(&pool);
    let document = document(&api).await;
    assert_eq!(document["info"]["title"], "RouteForge");
    assert_eq!(document["info"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        document["components"]["securitySchemes"]["bearer"]["scheme"],
        "bearer"
    );

    let schemas = &document["components"]["schemas"];
    for named in [
        "Order",
        "OrderRequest",
        "Plan",
        "PlanRequest",
        "RouteBudget",
        "Route",
        "RouteStop",
        "Courier",
        "Depot",
        "User",
        "Login",
        "Session",
        "SystemStats",
        "QueueDepth",
        "WorkerInfo",
        "Problem",
        "FieldError",
        "LatLon",
        // What the events of a stream carry. No response has them as its body, so
        // they are in the document only because they are named for it.
        "RouteJob",
        "RouteProgress",
        "JobProgress",
        "JobStatus",
    ] {
        assert!(schemas.get(named).is_some(), "no schema for {named}");
    }
    // A stream says that it is one, and what a caller who is kept out is answered.
    for path in ["/api/plans/{id}/events", "/api/system/stats/events"] {
        let responses = &document["paths"][path]["get"]["responses"];
        let stream = &responses["200"]["content"]["text/event-stream"];
        assert_eq!(stream["schema"]["type"], "string", "{path}");
        let refused = &responses["401"]["content"]["application/problem+json"];
        assert!(refused.is_object(), "{path}");
    }
    // The day whose plans are listed has to be said.
    assert_eq!(
        document["paths"]["/api/plans"]["get"]["parameters"],
        json!([{
            "name": "service_date",
            "in": "query",
            "description": "The day the plans are for.",
            "required": true,
            "schema": { "type": "string", "format": "date" },
        }])
    );
    // The enumerations say their values, the new status among them.
    let statuses = schemas["PlanStatus"]["enum"].as_array().unwrap();
    assert!(statuses.contains(&json!("discarded")), "{statuses:?}");
    // And the limits a type keeps are said where a client can read them.
    assert_eq!(schemas["LatLon"]["properties"]["lat"]["maximum"], 90.0);
    assert_eq!(schemas["LatLon"]["properties"]["lon"]["minimum"], -180.0);

    // A list says how it is filtered and paged, and an id that it is one.
    let filters: Vec<(&str, &str)> = document["paths"]["/api/orders"]["get"]["parameters"]
        .as_array()
        .expect("the list of orders has parameters")
        .iter()
        .map(|parameter| {
            (
                parameter["name"].as_str().unwrap(),
                parameter["in"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        filters,
        [
            ("client_id", "query"),
            ("service_date", "query"),
            ("status", "query"),
            ("after", "query"),
            ("limit", "query"),
        ]
    );
    assert_eq!(
        document["paths"]["/api/orders/{id}"]["get"]["parameters"],
        json!([{
            "name": "id",
            "in": "path",
            "required": true,
            "schema": { "type": "string", "format": "uuid" },
        }])
    );

    // Nothing in the document points at a schema it does not have.
    let text = document.to_string();
    for reference in text.split("#/components/schemas/").skip(1) {
        let name = reference.split('"').next().unwrap();
        assert!(
            schemas.get(name).is_some(),
            "{name} is referred to and not there"
        );
    }

    let ids: Vec<&str> = document["paths"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|item| item.as_object().unwrap().values())
        .filter_map(|operation| operation["operationId"].as_str())
        .collect();
    let distinct: BTreeSet<&str> = ids.iter().copied().collect();
    assert_eq!(ids.len(), MATRIX.len());
    assert_eq!(distinct.len(), ids.len(), "{ids:?}");
}

/// What the API logs, as the JSON lines it writes with `LOG_FORMAT=json`.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<u8>>>);

impl io::Write for Log {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Log {
    fn lines(&self) -> Vec<Value> {
        let written = self.0.lock().unwrap();
        String::from_utf8_lossy(&written)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

/// Starts capturing what every test of this binary logs, and gives the capture.
///
/// The subscriber is the default of the whole process and not of one test's thread,
/// for the reason the solver's tests give: `tracing` remembers for every place that
/// logs whether anybody listens.
fn captured_log() -> &'static Log {
    static LOG: OnceLock<Log> = OnceLock::new();
    LOG.get_or_init(|| {
        let log = Log::default();
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("no other test sets a subscriber");
        log
    })
}

/// Verification step 6 of the plan follows one request through the logs of every
/// service. This is the API's part of it.
#[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
async fn what_a_request_logs_carries_its_id_and_who_asked(pool: PgPool) {
    let log = captured_log();
    let api = testing::api(&pool);
    let operator = api.actor(Role::Operator).await;
    api.depot().await;
    let sent = async |request_id: &str, day: u32, who: Option<&Actor>| {
        let request = testing::request("POST", "/api/plans", who)
            .insert_header((X_REQUEST_ID, request_id))
            .set_json(json!({ "service_date": date(day) }));
        api.send(request).await
    };
    // Other tests log into the same capture. These requests are told by their ids.
    let run = Uuid::now_v7();
    let (taken, refused, broken) = (
        format!("{run}.taken"),
        format!("{run}.refused"),
        format!("{run}.broken"),
    );

    assert_eq!(
        sent(&taken, 12, Some(&operator)).await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        sent(&refused, 13, None).await.status,
        StatusCode::UNAUTHORIZED
    );
    sqlx::query("ALTER TABLE jobs ADD CONSTRAINT nothing_gets_in CHECK (false) NOT VALID")
        .execute(&pool)
        .await
        .unwrap();
    let failed = sent(&broken, 13, Some(&operator)).await;
    assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);

    let lines = log.lines();
    // A line names the span it was logged in, and the span has the fields.
    let span = |line: &Value, name: &str| -> Option<Value> {
        let spans = line["spans"].as_array()?;
        spans.iter().find_map(|span| span.get(name)).cloned()
    };
    let of = |request_id: &str| -> Vec<&Value> {
        let wanted = json!(request_id);
        let lines = lines.iter();
        lines
            .filter(|line| span(line, "request_id").as_ref() == Some(&wanted))
            .collect()
    };
    let handled = |request_id: &str| -> Value {
        let lines = of(request_id);
        let line = lines
            .iter()
            .find(|line| line["fields"]["message"] == "handled the request")
            .unwrap_or_else(|| panic!("nothing logged the end of {request_id}"));
        (*line).clone()
    };

    let line = handled(&taken);
    assert_eq!(span(&line, "method"), Some(json!("POST")));
    assert_eq!(span(&line, "path"), Some(json!("/api/plans")));
    assert_eq!(span(&line, "status"), Some(json!(201)));
    assert_eq!(
        span(&line, "user_id"),
        Some(json!(operator.user.id.to_string()))
    );
    assert_eq!(line["level"], "INFO");

    let line = handled(&refused);
    assert_eq!(span(&line, "status"), Some(json!(401)));
    assert_eq!(span(&line, "user_id"), None, "nobody was logged in");

    // What went wrong on the server is in its log, with the id to find it by, and
    // was not in the response.
    assert_eq!(span(&handled(&broken), "status"), Some(json!(500)));
    let cause = of(&broken)
        .into_iter()
        .find(|line| line["level"] == "ERROR")
        .expect("the failure was logged as an error");
    assert!(cause.to_string().contains("nothing_gets_in"), "{cause}");
    assert!(!failed.text.contains("nothing_gets_in"), "{}", failed.text);
}
