//! Who is asking, and whether they may.
//!
//! A password is stored as an Argon2id hash and checked on a blocking thread, because
//! hashing is meant to be slow. A login gives a token, signed with a key only the API
//! has, that names the user and their role and expires. Every later request carries
//! it, and nothing is looked up to check it.
//!
//! The role is in the token. So a user whose role an admin has changed keeps the old
//! one until their token expires, an hour at most by default. That is the price of
//! checking a request without the database.
//!
//! A handler says what it needs by the type of an argument: [`AuthUser`] for anyone
//! who is logged in, [`Staff`] for an operator or an admin, [`Admin`] for an admin.
//!
//! One caller is no user: the Telegram bot, [`Bot`]. It shows a key instead of a token,
//! and all the key is good for is a short-lived token of the Telegram user the bot is
//! talking to. Whatever the bot then does, it does as that user (D9, D60).

use std::future::{Ready, ready};
use std::sync::Arc;
use std::time::Duration;

use actix_web::dev::Payload;
use actix_web::http::header;
use actix_web::{FromRequest, HttpMessage, HttpRequest, web};
use anyhow::Context;
use argon2::Argon2;
use argon2::password_hash::{self, PasswordHasher, PasswordVerifier};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use routeforge_domain::{Role, Session, User, UserId};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tracing_actix_web::RootSpan;

use crate::app::AppState;
use crate::error::ApiError;

/// Hashes passwords and checks them, off the threads that serve requests.
#[derive(Clone)]
pub(crate) struct Passwords {
    argon: Argon2<'static>,
    /// The hash of a password nobody has. A login for an address without an account
    /// is checked against it, so that it takes as long as any other and the time
    /// tells nothing about which addresses are registered.
    decoy: Arc<str>,
}

impl Passwords {
    /// Argon2id with the parameters the crate recommends: 19 MiB and two passes.
    pub(crate) fn new() -> anyhow::Result<Self> {
        Self::with(Argon2::default())
    }

    /// The same algorithm with the smallest parameters there are. A debug build takes
    /// the better part of a second over a real hash, and the tests make many.
    #[cfg(test)]
    pub(crate) fn cheap() -> Self {
        use argon2::{Params, Version};

        let params = Params::new(
            Params::MIN_M_COST,
            Params::MIN_T_COST,
            Params::MIN_P_COST,
            None,
        )
        .unwrap();
        Self::with(Argon2::new(
            argon2::Algorithm::Argon2id,
            Version::V0x13,
            params,
        ))
        .unwrap()
    }

    fn with(argon: Argon2<'static>) -> anyhow::Result<Self> {
        let decoy = argon
            .hash_password(b"the password of nobody")
            .context("could not hash a password")?
            .to_string();
        Ok(Self {
            argon,
            decoy: decoy.into(),
        })
    }

    /// The hash to store for a password.
    pub(crate) async fn hash(&self, password: String) -> Result<String, ApiError> {
        let argon = self.argon.clone();
        let hashing = move || {
            argon
                .hash_password(password.as_bytes())
                .map(|hash| hash.to_string())
        };
        web::block(hashing)
            .await
            .map_err(ApiError::internal)?
            .map_err(ApiError::internal)
    }

    /// Whether `password` is the one `stored` is the hash of.
    ///
    /// `None` stands for an account that does not exist. The answer is `false`, after
    /// the same work as for one that does.
    pub(crate) async fn verify(
        &self,
        password: String,
        stored: Option<String>,
    ) -> Result<bool, ApiError> {
        let argon = self.argon.clone();
        let decoy = Arc::clone(&self.decoy);
        let checking = move || {
            let hash = stored.as_deref().unwrap_or(&decoy);
            // The parameters are those the hash was made with, not this hasher's.
            match argon.verify_password(password.as_bytes(), hash) {
                Ok(()) => Ok(stored.is_some()),
                Err(password_hash::Error::PasswordInvalid) => Ok(false),
                Err(error) => Err(error),
            }
        };
        web::block(checking)
            .await
            .map_err(ApiError::internal)?
            .map_err(ApiError::internal)
    }
}

/// What a token says.
#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    /// The user.
    sub: UserId,
    role: Role,
    /// When the token was issued and when it stops being accepted, in seconds since
    /// 1970.
    iat: u64,
    exp: u64,
}

/// Signs the tokens a login gives, and reads them back.
pub(crate) struct Tokens {
    signing: EncodingKey,
    checking: DecodingKey,
    validation: Validation,
    ttl: Duration,
}

impl Tokens {
    /// Tokens signed with `secret` (HS256) that are good for `ttl`.
    pub(crate) fn new(secret: &[u8], ttl: Duration) -> Self {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_required_spec_claims(&["exp", "sub"]);
        // The same process issues a token and checks it, by the same clock.
        validation.leeway = 0;
        Self {
            signing: EncodingKey::from_secret(secret),
            checking: DecodingKey::from_secret(secret),
            validation,
            ttl,
        }
    }

    /// A session for `user`, from now, for as long as a login is good for.
    pub(crate) fn issue(&self, user: User) -> Result<Session, ApiError> {
        self.issue_for(user, self.ttl)
    }

    /// A session for `user`, from now, that is good for `ttl`. The bot's are much
    /// shorter than a login's: it asks for one each time it needs one (D60).
    pub(crate) fn issue_for(&self, user: User, ttl: Duration) -> Result<Session, ApiError> {
        let now = jsonwebtoken::get_current_timestamp();
        let token = self.sign(user.id, user.role, now, ttl)?;
        Ok(Session {
            token,
            expires_in: ttl.as_secs(),
            user,
        })
    }

    fn sign(
        &self,
        user: UserId,
        role: Role,
        issued_at: u64,
        ttl: Duration,
    ) -> Result<String, ApiError> {
        let claims = Claims {
            sub: user,
            role,
            iat: issued_at,
            exp: issued_at.saturating_add(ttl.as_secs()),
        };
        jsonwebtoken::encode(&Header::default(), &claims, &self.signing).map_err(ApiError::internal)
    }

    /// Who a token speaks for. `None` if it was not signed with this key, has expired
    /// or cannot be read.
    pub(crate) fn read(&self, token: &str) -> Option<AuthUser> {
        let token = jsonwebtoken::decode::<Claims>(token, &self.checking, &self.validation).ok()?;
        Some(AuthUser {
            id: token.claims.sub,
            role: token.claims.role,
            expires_at: token.claims.exp,
        })
    }

    /// A token for `user` as it would have been issued at `issued_at`, so that a test
    /// can hold one that is about to expire.
    #[cfg(test)]
    pub(crate) fn issued_at(&self, user: &User, issued_at: u64) -> String {
        self.sign(user.id, user.role, issued_at, self.ttl).unwrap()
    }
}

/// The key the bot shows to be given a user's token (D60). The bot and the API have
/// it, and nobody else.
///
/// It has no `Debug`, so that it cannot reach a log.
pub(crate) struct ServiceKey(Box<[u8]>);

impl ServiceKey {
    pub(crate) fn new(key: &str) -> Self {
        Self(key.as_bytes().into())
    }

    /// Whether `shown` is the key.
    ///
    /// Every byte is compared, however early the first one differs. So the time it
    /// takes says nothing about how much of a guess was right. It does say whether
    /// the guess is as long as the key, which is no help in guessing it.
    fn is(&self, shown: &str) -> bool {
        self.0.ct_eq(shown.as_bytes()).into()
    }
}

/// Whoever sent the request, as their token says. Any role will do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AuthUser {
    pub(crate) id: UserId,
    pub(crate) role: Role,
    /// When the token stops being accepted, in seconds since 1970. A request is
    /// checked once, as it arrives. What answers for longer than that, an event
    /// stream, ends itself at this moment (D50).
    pub(crate) expires_at: u64,
}

impl AuthUser {
    /// Whether this is an operator or an admin: someone who sees everybody's orders.
    pub(crate) fn is_staff(&self) -> bool {
        matches!(self.role, Role::Operator | Role::Admin)
    }
}

/// An operator or an admin.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Staff(pub(crate) AuthUser);

/// An admin.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Admin(pub(crate) AuthUser);

/// The Telegram bot, which has shown its key (D60).
///
/// It is no user, and no token of a user's stands for it: an admin's opens what this
/// guards as little as nobody's does.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Bot;

fn state_of(request: &HttpRequest) -> Result<&web::Data<AppState>, ApiError> {
    request
        .app_data::<web::Data<AppState>>()
        .ok_or_else(|| ApiError::internal(anyhow::anyhow!("the application has no state")))
}

fn the_bot(request: &HttpRequest) -> Result<Bot, ApiError> {
    let Some(key) = &state_of(request)?.bot_key else {
        return Err(ApiError::Unauthorized(
            "this API takes no bot: it was started without BOT_SERVICE_KEY",
        ));
    };
    match bearer_token(request) {
        Some(shown) if key.is(shown) => Ok(Bot),
        _ => Err(ApiError::Unauthorized(
            "this is for the bot, which shows its key as `Authorization: Bearer <key>`",
        )),
    }
}

fn authenticate(request: &HttpRequest) -> Result<AuthUser, ApiError> {
    let state = state_of(request)?;
    let token = bearer_token(request).ok_or(ApiError::Unauthorized(
        "log in, and send the token as `Authorization: Bearer <token>`",
    ))?;
    let user = state.tokens.read(token).ok_or(ApiError::Unauthorized(
        "the token is not valid, or is no longer: log in again",
    ))?;
    // So that what the request logs says who asked.
    if let Some(span) = request.extensions().get::<RootSpan>() {
        span.record("user_id", tracing::field::display(user.id));
    }
    Ok(user)
}

/// The token of an `Authorization: Bearer <token>` header.
fn bearer_token(request: &HttpRequest) -> Option<&str> {
    let value = request.headers().get(header::AUTHORIZATION)?;
    let (scheme, token) = value.to_str().ok()?.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then_some(token.trim())
}

impl FromRequest for AuthUser {
    type Error = ApiError;
    type Future = Ready<Result<Self, ApiError>>;

    fn from_request(request: &HttpRequest, _: &mut Payload) -> Self::Future {
        ready(authenticate(request))
    }
}

impl FromRequest for Staff {
    type Error = ApiError;
    type Future = Ready<Result<Self, ApiError>>;

    fn from_request(request: &HttpRequest, _: &mut Payload) -> Self::Future {
        ready(authenticate(request).and_then(|user| {
            if user.is_staff() {
                Ok(Self(user))
            } else {
                Err(ApiError::Forbidden)
            }
        }))
    }
}

impl FromRequest for Admin {
    type Error = ApiError;
    type Future = Ready<Result<Self, ApiError>>;

    fn from_request(request: &HttpRequest, _: &mut Payload) -> Self::Future {
        ready(authenticate(request).and_then(|user| {
            if user.role == Role::Admin {
                Ok(Self(user))
            } else {
                Err(ApiError::Forbidden)
            }
        }))
    }
}

impl FromRequest for Bot {
    type Error = ApiError;
    type Future = Ready<Result<Self, ApiError>>;

    fn from_request(request: &HttpRequest, _: &mut Payload) -> Self::Future {
        ready(the_bot(request))
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
    const HOUR: Duration = Duration::from_secs(3_600);

    fn user(role: Role) -> User {
        User {
            id: UserId::generate(),
            email: Some("ada@example.com".to_owned()),
            telegram_id: None,
            role,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn a_token_says_who_and_what_role_and_for_how_long() {
        let tokens = Tokens::new(SECRET, HOUR);
        for &role in Role::ALL {
            let user = user(role);
            let session = tokens.issue(user.clone()).unwrap();
            assert_eq!(session.expires_in, 3_600);
            assert_eq!(session.user, user);
            let read = tokens.read(&session.token).unwrap();
            assert_eq!((read.id, read.role), (user.id, role));
            // And until when: an hour from now, less the moment this test took.
            let now = jsonwebtoken::get_current_timestamp();
            assert!(
                (now + 3_590..=now + 3_600).contains(&read.expires_at),
                "{} from {now}",
                read.expires_at
            );
        }
    }

    #[test]
    fn a_token_is_good_until_the_second_it_expires() {
        let tokens = Tokens::new(SECRET, HOUR);
        let user = user(Role::Operator);
        let now = jsonwebtoken::get_current_timestamp();

        // Issued an hour ago, less a minute: a minute left.
        let nearly = tokens.issued_at(&user, now - 3_540);
        assert!(tokens.read(&nearly).is_some());
        // Issued an hour and a second ago. There is no leeway.
        let expired = tokens.issued_at(&user, now - 3_601);
        assert_eq!(tokens.read(&expired), None);
    }

    /// The bot's tokens are of the same key as a login's and read by the same check.
    /// They differ in how long they are good for, and in nothing else.
    #[test]
    fn a_token_can_be_issued_for_less_than_a_login_lasts() {
        let tokens = Tokens::new(SECRET, HOUR);
        let user = user(Role::Operator);
        let minute = Duration::from_secs(60);

        let session = tokens.issue_for(user.clone(), minute).unwrap();
        assert_eq!(session.expires_in, 60);
        assert_eq!(session.user, user);
        let read = tokens.read(&session.token).unwrap();
        assert_eq!((read.id, read.role), (user.id, Role::Operator));
        let now = jsonwebtoken::get_current_timestamp();
        assert!(
            (now + 50..=now + 60).contains(&read.expires_at),
            "{} from {now}",
            read.expires_at
        );

        // Issued two minutes ago for one minute: over, though a login would still hold.
        let over = tokens.sign(user.id, user.role, now - 120, minute).unwrap();
        assert_eq!(tokens.read(&over), None);
        assert!(tokens.read(&tokens.issued_at(&user, now - 120)).is_some());
    }

    #[test]
    fn the_bots_key_is_the_whole_key_and_nothing_but() {
        let key = ServiceKey::new("the key the bot alone is to have");
        assert!(key.is("the key the bot alone is to have"));
        for wrong in [
            "",
            "the key the bot alone is to hav",
            "the key the bot alone is to have.",
            "The key the bot alone is to have",
            "the key the bot alone is to havE",
        ] {
            assert!(!key.is(wrong), "{wrong:?}");
        }
    }

    #[test]
    fn a_token_signed_with_another_key_is_refused() {
        let ours = Tokens::new(SECRET, HOUR);
        let theirs = Tokens::new(b"another key of thirty-two bytes!", HOUR);
        let forged = theirs.issue(user(Role::Admin)).unwrap().token;
        assert_eq!(ours.read(&forged), None);
        assert!(theirs.read(&forged).is_some());
    }

    #[test]
    fn a_token_that_was_altered_is_refused() {
        let tokens = Tokens::new(SECRET, HOUR);
        let user = user(Role::Client);
        let token = tokens.issue(user.clone()).unwrap().token;
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);

        // The claims of an admin, under the signature of a client's token.
        let admin = Tokens::new(b"another key of thirty-two bytes!", HOUR)
            .sign(
                user.id,
                Role::Admin,
                jsonwebtoken::get_current_timestamp(),
                HOUR,
            )
            .unwrap();
        let admin_claims = admin.split('.').nth(1).unwrap();
        let promoted = format!("{}.{admin_claims}.{}", parts[0], parts[2]);
        assert_eq!(tokens.read(&promoted), None);

        // Without a signature, and with the algorithm that needs none.
        let unsigned = format!("{}.{}.", parts[0], parts[1]);
        assert_eq!(tokens.read(&unsigned), None);
        // {"alg":"none","typ":"JWT"}
        let none = format!("eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.{}.", parts[1]);
        assert_eq!(tokens.read(&none), None);

        for garbage in ["", "not a token", "a.b.c", &token[..token.len() - 2]] {
            assert_eq!(tokens.read(garbage), None, "{garbage:?}");
        }
    }

    #[tokio::test]
    async fn a_password_is_checked_against_its_hash() {
        let passwords = Passwords::cheap();
        let hash = passwords.hash("correct horse".to_owned()).await.unwrap();
        assert!(hash.starts_with("$argon2id$"), "{hash}");
        assert!(!hash.contains("correct horse"));

        let check = async |password: &str, stored: Option<&str>| {
            passwords
                .verify(password.to_owned(), stored.map(str::to_owned))
                .await
        };
        assert!(check("correct horse", Some(&hash)).await.unwrap());
        assert!(!check("wrong horse", Some(&hash)).await.unwrap());
        assert!(!check("", Some(&hash)).await.unwrap());

        // The same password twice gives two hashes: each has a salt of its own.
        let again = passwords.hash("correct horse".to_owned()).await.unwrap();
        assert_ne!(again, hash);
        assert!(check("correct horse", Some(&again)).await.unwrap());
    }

    #[tokio::test]
    async fn nobody_can_log_in_to_an_account_that_does_not_exist() {
        let passwords = Passwords::cheap();
        assert!(!passwords.verify("anything".to_owned(), None).await.unwrap());
        // Not even with the password the decoy was made from.
        assert!(
            !passwords
                .verify("the password of nobody".to_owned(), None)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn a_stored_hash_that_is_no_hash_is_a_fault_of_the_server() {
        let passwords = Passwords::cheap();
        let error = passwords
            .verify(
                "anything".to_owned(),
                Some("$argon2id$not-a-real-hash".to_owned()),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ApiError::Internal(_)), "{error:?}");
    }

    /// A hash made with the real parameters is checked by a hasher with others: the
    /// parameters travel in the hash.
    #[tokio::test]
    async fn a_hash_carries_the_parameters_it_was_made_with() {
        let real = Passwords::new().unwrap();
        let hash = real.hash("correct horse".to_owned()).await.unwrap();
        assert!(hash.contains("m=19456,t=2,p=1"), "{hash}");

        let cheap = Passwords::cheap();
        assert!(
            cheap
                .verify("correct horse".to_owned(), Some(hash))
                .await
                .unwrap()
        );
    }
}
