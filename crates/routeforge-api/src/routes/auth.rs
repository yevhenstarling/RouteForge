//! Opening an account, logging in, and the bot's way to a user's token.

use actix_web::{HttpResponse, post, web};
use routeforge_domain::db::users;
use routeforge_domain::{Login, NewUser, Role, Session, TelegramLogin, User};
use tracing_actix_web::RootSpan;
use utoipa_actix_web::service_config::ServiceConfig;

use crate::app::{AppState, NoKey, NotAcceptable};
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
#[utoipa::path(
    tag = "auth",
    responses(
        (status = 200, description = "The token, and who it speaks for", body = Session),
        (status = 401, description = "The address or the password is wrong",
            body = routeforge_domain::Problem, content_type = "application/problem+json"),
    )
)]
#[post("/api/auth/login")]
async fn login(
    state: web::Data<AppState>,
    body: web::Json<Login>,
) -> Result<web::Json<Session>, ApiError> {
    let sent = body.into_inner();
    let (user, hash) = match users::find_by_email(&state.pool, &sent.email).await? {
        Some(account) => (Some(account.user), Some(account.password_hash)),
        None => (None, None),
    };
    // Checked even when there is no such account, so that the time it takes does not
    // say whether there is.
    let right = state.passwords.verify(sent.password, hash).await?;
    match user {
        Some(user) if right => Ok(web::Json(state.tokens.issue(user)?)),
        // Which of the two was wrong is not said either.
        _ => Err(ApiError::Unauthorized(
            "the address or the password is wrong",
        )),
    }
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

    use crate::error::PROBLEM_JSON;
    use crate::testing::{self, Api, BOT_KEY, BOT_TOKEN_TTL, Reply, date};

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
