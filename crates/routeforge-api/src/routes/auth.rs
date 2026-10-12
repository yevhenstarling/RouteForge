//! Opening an account, logging in, and the bot's way to a user's token.

use std::time::Instant;

use actix_web::{HttpResponse, post, web};
use routeforge_domain::db::users;
use routeforge_domain::{Login, NewUser, Role, Session, TelegramLogin, User};
use tracing_actix_web::RootSpan;
use utoipa_actix_web::service_config::ServiceConfig;

use crate::app::{AppState, Busy, NoKey, NotAcceptable, TooManyLogins};
use crate::auth::Bot;
use crate::error::ApiError;
use crate::validate;

pub(crate) fn configure(config: &mut ServiceConfig<'_>) {
    config
        .service(register)
        .service(login)
        .service(telegram_login);
}

/// Open an account
///
/// Anyone can. The account is a client's: it places orders and sees its own. An admin
/// can make it an operator's or an admin's.
#[utoipa::path(
    tag = "auth",
    responses(
        (status = 201, description = "The account", body = User),
        (status = 409, description = "The address already has an account",
            body = routeforge_domain::Problem, content_type = "application/problem+json"),
        NotAcceptable,
        Busy,
    )
)]
#[post("/api/auth/register")]
async fn register(
    state: web::Data<AppState>,
    body: web::Json<Login>,
) -> Result<HttpResponse, ApiError> {
    let asked = body.into_inner();
    validate::registration(&asked)?;
    let new = NewUser {
        email: asked.email,
        password_hash: state.passwords.hash(asked.password).await?,
        role: Role::Client,
    };
    let user = users::create(&state.pool, &new).await?;
    Ok(HttpResponse::Created().json(user))
}

/// Log in
///
/// The token of the answer goes with every later request, as
/// `Authorization: Bearer <token>`. It says who the user is and what their role was at
/// this moment, and is good for `expires_in` seconds.
///
/// Only so many logins are taken for one address within a minute. After that the next
/// is refused, right password or wrong, until the minute is over. A login that
/// succeeds clears the count.
#[utoipa::path(
    tag = "auth",
    responses(
        (status = 200, description = "The token, and who it speaks for", body = Session),
        (status = 401, description = "The address or the password is wrong",
            body = routeforge_domain::Problem, content_type = "application/problem+json"),
        TooManyLogins,
        Busy,
    )
)]
#[post("/api/auth/login")]
async fn login(
    state: web::Data<AppState>,
    body: web::Json<Login>,
) -> Result<web::Json<Session>, ApiError> {
    let sent = body.into_inner();
    // Counted before anything is looked up or hashed, and for an address nobody has
    // as for one somebody has: the refusal says nothing about which is which (D68).
    state
        .logins
        .begin(&sent.email, Instant::now())
        .map_err(|retry_after| ApiError::TooManyLogins { retry_after })?;

    let checked = check(&state, &sent).await;
    match checked {
        Ok(Some(user)) => {
            state.logins.succeeded(&sent.email);
            Ok(web::Json(state.tokens.issue(user)?))
        }
        // Which of the two was wrong is not said either.
        Ok(None) => Err(ApiError::Unauthorized(
            "the address or the password is wrong",
        )),
        // The password was not checked at all: the database is away, or the API has
        // too many to check. That is not a try of whoever sent it.
        Err(error) => {
            state.logins.take_back(&sent.email);
            Err(error)
        }
    }
}

/// The account a login is for, if the password is its password.
async fn check(state: &AppState, sent: &Login) -> Result<Option<User>, ApiError> {
    let (user, hash) = match users::find_by_email(&state.pool, &sent.email).await? {
        Some(account) => (Some(account.user), Some(account.password_hash)),
        None => (None, None),
    };
    // Checked even when there is no such account, so that the time it takes does not
    // say whether there is.
    let right = state.passwords.verify(sent.password.clone(), hash).await?;
    Ok(user.filter(|_| right))
}

/// Log in a Telegram user, for the bot
///
/// The Telegram bot is the one caller of this. It shows its key, as
/// `Authorization: Bearer <key>`, says which Telegram user it is talking to, and is
/// given a token for that user's account. With that token it asks what the user asks
/// of it, so the bot can do no more than the user may (D9).
///
/// A Telegram user the API has not heard of gets an account here and now: a client's,
/// with no address and no password. An admin can make it an operator's.
///
/// The token is good for `expires_in` seconds, which is far less than a login's: the
/// bot asks for one whenever it needs one.
#[utoipa::path(
    tag = "auth",
    security(("service_key" = [])),
    responses(
        (status = 200, description = "The token, and whose account it speaks for", body = Session),
        NoKey,
        NotAcceptable,
    )
)]
#[post("/api/auth/telegram")]
async fn telegram_login(
    _: Bot,
    state: web::Data<AppState>,
    span: RootSpan,
    body: web::Json<TelegramLogin>,
) -> Result<web::Json<Session>, ApiError> {
    let asked = body.into_inner();
    validate::telegram(&asked)?;
    let user = users::find_or_create_by_telegram(&state.pool, asked.telegram_id).await?;
    // So that what the request logs says for whom the bot asked.
    span.record("user_id", tracing::field::display(user.id));
    Ok(web::Json(
        state.tokens.issue_for(user, state.bot_token_ttl)?,
    ))
}

#[cfg(test)]
mod tests {
    use actix_web::http::{StatusCode, header};
    use routeforge_domain::Role;
    use routeforge_domain::db::users;
    use serde_json::{Value, json};
    use sqlx::PgPool;

    use std::time::Duration;

    use crate::auth::Passwords;
    use crate::error::PROBLEM_JSON;
    use crate::logins::{Logins, WINDOW};
    use crate::testing::{self, Api, BOT_KEY, BOT_TOKEN_TTL, LOGIN_ATTEMPTS, Reply, date};

    const LOGIN: &str = "/api/auth/login";

    /// Opens an account through the API, and gives what logs in to it.
    async fn account(api: &Api, email: &str) -> Value {
        let login = json!({ "email": email, "password": "correct horse" });
        let made = api.post(None, "/api/auth/register", login.clone()).await;
        assert_eq!(made.status, StatusCode::CREATED, "{}", made.text);
        login
    }

    fn wrong_for(email: &str) -> Value {
        json!({ "email": email, "password": "wrong horse" })
    }

    /// How many seconds a refusal says to wait.
    fn retry_after(refused: &Reply) -> u64 {
        let seconds = refused.headers.get(header::RETRY_AFTER).expect("a wait");
        seconds.to_str().unwrap().parse().unwrap()
    }

    /// Whoever knows an address has a handful of guesses at its password within a
    /// minute, and no more (D68).
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn so_many_logins_are_taken_for_an_address_within_a_minute(pool: PgPool) {
        let api = testing::api(&pool);
        let right = account(&api, "ada@example.com").await;
        let wrong = wrong_for("ada@example.com");

        for attempt in 1..=LOGIN_ATTEMPTS {
            let refused = api.post(None, LOGIN, wrong.clone()).await;
            assert_eq!(refused.status, StatusCode::UNAUTHORIZED, "try {attempt}");
        }
        // The next is not looked at, and the right password is refused like a wrong
        // one: whoever guesses is not told when they have guessed.
        for body in [wrong.clone(), right.clone(), wrong_for("ADA@Example.com")] {
            let refused = api.post(None, LOGIN, body).await;
            assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(refused.content_type.as_deref(), Some(PROBLEM_JSON));
            assert!((1..=60).contains(&retry_after(&refused)));
            let detail = refused.body["detail"].as_str().unwrap();
            assert!(detail.contains("too many logins"), "{detail}");
        }

        // Somebody else logs in as ever.
        let bob = account(&api, "bob@example.com").await;
        assert_eq!(api.post(None, LOGIN, bob).await.status, StatusCode::OK);
    }

    /// The count is by the address that was sent, account or none. So how it is
    /// refused says nothing about which addresses are registered (D40).
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn an_address_nobody_has_is_limited_like_one_somebody_has(pool: PgPool) {
        let api = testing::api(&pool);
        account(&api, "ada@example.com").await;

        let mut answers = Vec::new();
        for email in ["ada@example.com", "nobody@example.com"] {
            let mut statuses = Vec::new();
            for _ in 0..LOGIN_ATTEMPTS + 2 {
                let refused = api.post(None, LOGIN, wrong_for(email)).await;
                statuses.push((refused.status, refused.body["title"].clone()));
            }
            answers.push(statuses);
        }
        assert_eq!(answers[0], answers[1]);
        assert_eq!(answers[0][0].0, StatusCode::UNAUTHORIZED);
        assert_eq!(answers[0].last().unwrap().0, StatusCode::TOO_MANY_REQUESTS);
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_login_that_succeeds_clears_the_count(pool: PgPool) {
        let api = testing::api(&pool);
        let right = account(&api, "ada@example.com").await;
        let wrong = wrong_for("ada@example.com");

        // One short of too many, and then the right one, which is the last that may.
        for _ in 1..LOGIN_ATTEMPTS {
            assert_eq!(
                api.post(None, LOGIN, wrong.clone()).await.status,
                StatusCode::UNAUTHORIZED
            );
        }
        assert_eq!(api.post(None, LOGIN, right).await.status, StatusCode::OK);

        // The count begins again: as many wrong ones are looked at as before.
        for attempt in 1..=LOGIN_ATTEMPTS {
            let refused = api.post(None, LOGIN, wrong.clone()).await;
            assert_eq!(refused.status, StatusCode::UNAUTHORIZED, "try {attempt}");
        }
        assert_eq!(
            api.post(None, LOGIN, wrong).await.status,
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn the_limit_is_lifted_when_its_time_is_over(pool: PgPool) {
        let window = Duration::from_millis(400);
        let api = testing::api_with_logins(&pool, Logins::new(2, window), Passwords::cheap());
        let right = account(&api, "ada@example.com").await;
        let wrong = wrong_for("ada@example.com");

        for _ in 0..2 {
            assert_eq!(
                api.post(None, LOGIN, wrong.clone()).await.status,
                StatusCode::UNAUTHORIZED
            );
        }
        let refused = api.post(None, LOGIN, right.clone()).await;
        assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
        // A fraction of a second is said as a whole one: never too short a wait.
        assert_eq!(retry_after(&refused), 1);

        tokio::time::sleep(window + Duration::from_millis(100)).await;
        assert_eq!(api.post(None, LOGIN, right).await.status, StatusCode::OK);
    }

    /// A login that is over the limit is refused before anything is looked up or hashed
    /// (D68), so refusing it costs the API nothing. Here no password can be looked at
    /// at all, and the refusal comes at once all the same.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_login_over_the_limit_is_refused_without_its_password_being_looked_at(pool: PgPool) {
        // One password at a time, and one that waits for its turn waits five seconds.
        let passwords = Passwords::cheap_for(1, Duration::from_secs(5));
        let logins = Logins::new(2, WINDOW);
        let api = testing::api_with_logins(&pool, logins, passwords.clone());
        let right = account(&api, "ada@example.com").await;
        for _ in 0..2 {
            let refused = api.post(None, LOGIN, wrong_for("ada@example.com")).await;
            assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
        }

        // The one turn there is, taken by somebody else's password.
        let _held = passwords.turn().await.unwrap();
        let asked = std::time::Instant::now();
        let refused = api.post(None, LOGIN, right).await;
        assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
        let took = asked.elapsed();
        assert!(
            took < Duration::from_secs(2),
            "it waited for a turn: {took:?}"
        );
    }

    /// Passwords are hashed a few at a time (D68). A login that finds no turn free is
    /// told to come back, and is no try at the password: it was never looked at.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_login_the_server_is_too_busy_for_is_told_so_and_not_counted(pool: PgPool) {
        let passwords = Passwords::cheap_for(1, Duration::from_millis(50));
        let logins = Logins::new(2, WINDOW);
        let api = testing::api_with_logins(&pool, logins, passwords.clone());
        let right = account(&api, "ada@example.com").await;
        let wrong = wrong_for("ada@example.com");

        // The one turn there is, taken by somebody else's password.
        let held = passwords.turn().await.unwrap();
        for _ in 0..3 {
            let busy = api.post(None, LOGIN, wrong.clone()).await;
            assert_eq!(busy.status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(busy.content_type.as_deref(), Some(PROBLEM_JSON));
            assert_eq!(retry_after(&busy), 1);
        }
        // An account cannot be opened meanwhile either: that takes a hash as well.
        let eve = json!({ "email": "eve@example.com", "password": "correct horse" });
        let busy = api.post(None, "/api/auth/register", eve).await;
        assert_eq!(busy.status, StatusCode::SERVICE_UNAVAILABLE);
        drop(held);

        // Both tries are still there.
        for _ in 0..2 {
            assert_eq!(
                api.post(None, LOGIN, wrong.clone()).await.status,
                StatusCode::UNAUTHORIZED
            );
        }
        assert_eq!(
            api.post(None, LOGIN, right).await.status,
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    /// A request with `token` for a bearer, whoever's it is.
    async fn with_token(api: &Api, method: &str, path: &str, token: &str, body: Value) -> Reply {
        let request = testing::request(method, path, None)
            .insert_header((header::AUTHORIZATION, format!("Bearer {token}")));
        if body.is_null() {
            api.send(request).await
        } else {
            api.send(request.set_json(body)).await
        }
    }

    fn token_of(session: &Reply) -> String {
        session.body["token"].as_str().expect("a token").to_owned()
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn the_bot_is_given_a_token_of_the_telegram_user_it_names(pool: PgPool) {
        let api = testing::api(&pool);

        let session = api
            .telegram_login(Some(BOT_KEY), json!({ "telegram_id": 4_242 }))
            .await;
        assert_eq!(session.status, StatusCode::OK, "{}", session.text);
        // Good for a minute and not for the hour a login is.
        assert_eq!(session.body["expires_in"], BOT_TOKEN_TTL.as_secs());
        // Somebody the API had not heard of: a client now, whom no address leads to.
        let user = &session.body["user"];
        assert_eq!(
            (&user["role"], &user["telegram_id"], &user["email"]),
            (&json!("client"), &json!(4_242), &Value::Null)
        );
        let stored = users::list(&pool).await.unwrap();
        assert_eq!(json!(stored), json!([user]));

        // What the bot does with the token it does as that client, and no more.
        let token = token_of(&session);
        let order = json!({
            "kind": "delivery",
            "service_date": date(13),
            "location": { "lat": 50.45, "lon": 30.52 },
            "demand": 2,
        });
        let placed = with_token(&api, "POST", "/api/orders", &token, order).await;
        assert_eq!(placed.status, StatusCode::CREATED, "{}", placed.text);
        assert_eq!(placed.body["client_id"], user["id"]);
        let plans = with_token(
            &api,
            "GET",
            "/api/plans?service_date=2026-10-13",
            &token,
            Value::Null,
        );
        assert_eq!(plans.await.status, StatusCode::FORBIDDEN);

        // Whoever comes back is who they were. Somebody else is somebody else.
        let again = api
            .telegram_login(Some(BOT_KEY), json!({ "telegram_id": 4_242 }))
            .await;
        assert_eq!(again.body["user"], *user);
        let other = api
            .telegram_login(Some(BOT_KEY), json!({ "telegram_id": 7 }))
            .await;
        assert_ne!(other.body["user"]["id"], user["id"]);
        assert_eq!(users::list(&pool).await.unwrap().len(), 2);
        // The other one's token does not open the first one's order.
        let order_path = format!("/api/orders/{}", placed.body["id"].as_str().unwrap());
        let theirs = with_token(&api, "GET", &order_path, &token_of(&other), Value::Null).await;
        assert_eq!(theirs.status, StatusCode::NOT_FOUND);
    }

    /// The role is in the token (D40), and the bot asks for a new one each time. So
    /// what an admin makes of an account applies to the next thing its owner asks.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn an_account_an_admin_has_promoted_gets_an_operators_token_next(pool: PgPool) {
        let api = testing::api(&pool);
        let admin = api.actor(Role::Admin).await;
        let login = || api.telegram_login(Some(BOT_KEY), json!({ "telegram_id": 4_242 }));
        let couriers = async |token: &str| {
            with_token(&api, "GET", "/api/couriers", token, Value::Null)
                .await
                .status
        };

        let before = login().await;
        assert_eq!(couriers(&token_of(&before)).await, StatusCode::FORBIDDEN);

        let role = format!(
            "/api/users/{}/role",
            before.body["user"]["id"].as_str().unwrap()
        );
        let promoted = api
            .put(Some(&admin), &role, json!({ "role": "operator" }))
            .await;
        assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.text);
        // Still the account of that Telegram user, and of nobody's address.
        assert_eq!(promoted.body["telegram_id"], 4_242);

        let after = login().await;
        assert_eq!(after.body["user"]["role"], "operator");
        assert_eq!(after.body["user"]["id"], before.body["user"]["id"]);
        assert_eq!(couriers(&token_of(&after)).await, StatusCode::OK);
        // The token from before says what the account was, for the minute it lasts.
        assert_eq!(couriers(&token_of(&before)).await, StatusCode::FORBIDDEN);
    }

    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn nothing_but_the_key_of_the_bot_opens_its_way_in(pool: PgPool) {
        let api = testing::api(&pool);
        let admin = api.actor(Role::Admin).await;
        let who = json!({ "telegram_id": 4_242 });

        let lower = BOT_KEY.to_lowercase().replace('t', "T");
        let longer = format!("{BOT_KEY}.");
        for wrong in [
            None,
            Some(""),
            Some("a key somebody thought of"),
            Some(&BOT_KEY[1..]),
            Some(longer.as_str()),
            Some(lower.as_str()),
            // An admin may do everything a user may. This is not for users.
            Some(admin.token.as_str()),
        ] {
            let refused = api.telegram_login(wrong, who.clone()).await;
            assert_eq!(refused.status, StatusCode::UNAUTHORIZED, "{wrong:?}");
            assert_eq!(refused.content_type.as_deref(), Some(PROBLEM_JSON));
            assert_eq!(
                refused.headers.get(header::WWW_AUTHENTICATE).unwrap(),
                "Bearer"
            );
        }
        // The key is a bearer's, and no other scheme's.
        let basic = testing::request("POST", "/api/auth/telegram", None)
            .insert_header((header::AUTHORIZATION, format!("Basic {BOT_KEY}")))
            .set_json(who.clone());
        assert_eq!(api.send(basic).await.status, StatusCode::UNAUTHORIZED);
        // Nobody who was kept out left an account behind.
        assert_eq!(
            users::list(&pool).await.unwrap(),
            std::slice::from_ref(&admin.user)
        );

        // The gate comes before the body: without the key nobody is told what is
        // wrong with what they sent.
        let unread = api
            .telegram_login(None, json!({ "telegram_id": "ada" }))
            .await;
        assert_eq!(unread.status, StatusCode::UNAUTHORIZED);

        // With the key, the body is read, and held to what a Telegram user is.
        for (body, field) in [
            (json!({ "telegram_id": 0 }), Some("telegram_id")),
            (json!({ "telegram_id": -4_242 }), Some("telegram_id")),
            (json!({ "telegram_id": "ada" }), None),
            (json!({ "telegram_id": 1.5 }), None),
            (json!({}), None),
        ] {
            let refused = api.telegram_login(Some(BOT_KEY), body.clone()).await;
            assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
            if let Some(field) = field {
                assert_eq!(refused.body["errors"][0]["field"], field, "{body}");
            }
        }
        assert_eq!(users::list(&pool).await.unwrap().len(), 1);
    }

    /// The API has no default for the key (D60). Started without one it takes no bot,
    /// whatever anybody shows it.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn an_api_that_was_given_no_key_takes_no_bot(pool: PgPool) {
        let api = testing::api_without_a_bot(&pool);
        let who = json!({ "telegram_id": 4_242 });

        for shown in [Some(BOT_KEY), Some(""), None] {
            let refused = api.telegram_login(shown, who.clone()).await;
            assert_eq!(refused.status, StatusCode::UNAUTHORIZED, "{shown:?}");
            let detail = refused.body["detail"].as_str().unwrap();
            assert!(detail.contains("BOT_SERVICE_KEY"), "{detail}");
        }
        assert!(users::list(&pool).await.unwrap().is_empty());

        // Everything else is as it is with a key.
        let client = api.actor(Role::Client).await;
        assert_eq!(
            api.get(Some(&client), "/api/orders").await.status,
            StatusCode::OK
        );
    }

    /// Two first messages of one person can reach the API at the same moment.
    #[sqlx::test(migrator = "routeforge_domain::db::MIGRATOR")]
    async fn a_newcomer_who_is_asked_for_twice_at_once_is_one_account(pool: PgPool) {
        let api = testing::api(&pool);
        let who = json!({ "telegram_id": 4_242 });
        let (first, second, third) = tokio::join!(
            api.telegram_login(Some(BOT_KEY), who.clone()),
            api.telegram_login(Some(BOT_KEY), who.clone()),
            api.telegram_login(Some(BOT_KEY), who.clone()),
        );
        for session in [&first, &second, &third] {
            assert_eq!(session.status, StatusCode::OK, "{}", session.text);
            assert_eq!(session.body["user"], first.body["user"]);
        }
        assert_eq!(users::list(&pool).await.unwrap().len(), 1);
    }
}
