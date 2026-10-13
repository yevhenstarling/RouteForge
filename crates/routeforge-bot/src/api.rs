//! What the bot asks of the API, and how it asks.
//!
//! The bot has no authority of its own (D9). The one thing it does in its own name is
//! [`Api::telegram_login`]: it shows its key and is given a token of the Telegram user
//! it is talking to (D60). Everything else it asks with that token, as that user, and
//! is answered as that user would be.
//!
//! [`Api`] is what the dialogue is written against, so that a test can put something
//! else in its place. Its methods are named after the operations of the API's OpenAPI
//! document. [`HttpApi`] is the real thing, and the types it sends and reads are the
//! ones the API reads and sends: both take them from `routeforge-domain` (D43).

use std::error::Error;
use std::future::Future;
use std::time::Duration;

use anyhow::Context;
use chrono::NaiveDate;
use reqwest::{Client, RequestBuilder};
use routeforge_domain::{
    Depot, IdempotencyKey, Order, OrderFilter, OrderId, OrderRequest, Plan, PlanFilter,
    PlanRequest, Problem, Session, TelegramLogin,
};
use serde::de::DeserializeOwned;

/// The header an id travels in, from an update of Telegram's into the log of the API
/// and, for a plan, of every solver that works on it (D42).
pub(crate) const X_REQUEST_ID: &str = "x-request-id";

/// How long the API is given to take a connection, and to answer. The person is
/// waiting in a chat. Something that takes longer is told to them as the API being
/// away, and they can try again.
const CONNECT_WITHIN: Duration = Duration::from_secs(5);
const ANSWER_WITHIN: Duration = Duration::from_secs(10);

/// Why the API did not do what it was asked.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Failure {
    /// The API refused. `detail` is its own word for why, written for a person (D39).
    #[error("the API refused with {status}: {detail}")]
    Refused { status: u16, detail: String },
    /// The API did not answer, or answered what the bot cannot read.
    #[error("the API does not answer: {0}")]
    Away(String),
}

/// On whose behalf a request is made, and which update of Telegram's it comes of.
#[derive(Clone, Copy)]
pub(crate) struct Caller<'a> {
    /// The token the API gave for the Telegram user. Never the bot's key.
    pub(crate) token: &'a str,
    pub(crate) request_id: &'a str,
}

/// The API, as far as the bot uses it.
pub(crate) trait Api: Send + Sync + 'static {
    /// A session of the Telegram user's account, which is made if this is the first
    /// that is heard of them. This is the one request the bot makes with its key.
    fn telegram_login(
        &self,
        telegram_id: i64,
        request_id: &str,
    ) -> impl Future<Output = Result<Session, Failure>> + Send;

    /// Places an order, once for each key (D76): asked again with the key of an
    /// order that was placed, the API answers with that order and places nothing.
    /// So an order whose answer was lost can be asked for again.
    fn create_order(
        &self,
        who: &Caller<'_>,
        order: &OrderRequest,
        key: &IdempotencyKey,
    ) -> impl Future<Output = Result<Order, Failure>> + Send;

    /// One page of the orders that pass the filter, oldest first (D44).
    fn list_orders(
        &self,
        who: &Caller<'_>,
        filter: &OrderFilter,
    ) -> impl Future<Output = Result<Vec<Order>, Failure>> + Send;

    fn get_order(
        &self,
        who: &Caller<'_>,
        id: OrderId,
    ) -> impl Future<Output = Result<Order, Failure>> + Send;

    /// Withdraws an order that is still pending.
    fn cancel_order(
        &self,
        who: &Caller<'_>,
        id: OrderId,
    ) -> impl Future<Output = Result<Order, Failure>> + Send;

    fn create_plan(
        &self,
        who: &Caller<'_>,
        plan: &PlanRequest,
    ) -> impl Future<Output = Result<Plan, Failure>> + Send;

    /// The plans of a day, newest first (D53).
    fn list_plans(
        &self,
        who: &Caller<'_>,
        day: NaiveDate,
    ) -> impl Future<Output = Result<Vec<Plan>, Failure>> + Send;

    /// The depot as it is now, which anyone who is logged in may read. `None` if none
    /// has been set yet.
    fn get_depot(
        &self,
        who: &Caller<'_>,
    ) -> impl Future<Output = Result<Option<Depot>, Failure>> + Send;
}

/// The API over HTTP.
///
/// It has no `Debug`, so that the key it holds cannot reach a log.
pub(crate) struct HttpApi {
    client: Client,
    /// Where the API is, without a slash at its end.
    base: String,
    /// What the bot shows to be given a user's token (D60).
    service_key: String,
}

impl HttpApi {
    pub(crate) fn new(base: &str, service_key: &str) -> anyhow::Result<Self> {
        Self::within(base, service_key, CONNECT_WITHIN, ANSWER_WITHIN)
    }

    /// The API, given so long to take a connection and so long to answer.
    pub(crate) fn within(
        base: &str,
        service_key: &str,
        connect: Duration,
        answer: Duration,
    ) -> anyhow::Result<Self> {
        let client = Client::builder()
            .connect_timeout(connect)
            .timeout(answer)
            .build()
            .context("could not make the client the API is asked with")?;
        Ok(Self {
            client,
            base: base.trim_end_matches('/').to_owned(),
            service_key: service_key.to_owned(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Sends a request with what says who asks and which update it comes of, and
    /// reads the answer: what was asked for, or why not.
    async fn answer<T: DeserializeOwned>(
        &self,
        asked: RequestBuilder,
        bearer: &str,
        request_id: &str,
    ) -> Result<T, Failure> {
        let response = asked
            .bearer_auth(bearer)
            .header(X_REQUEST_ID, request_id)
            .send()
            .await
            .map_err(|error| Failure::Away(causes(&error)))?;
        let status = response.status();
        if status.is_success() {
            return response.json().await.map_err(|error| {
                Failure::Away(format!(
                    "it answered {status} with what cannot be read: {}",
                    causes(&error)
                ))
            });
        }
        // A refusal of the API's is a problem (D39). One from something that stands
        // between the two is not, and has its status to go by.
        let detail = match response.json::<Problem>().await {
            Ok(problem) => words_of(problem),
            Err(_) => status
                .canonical_reason()
                .unwrap_or("no reason was given")
                .to_lowercase(),
        };
        Err(Failure::Refused {
            status: status.as_u16(),
            detail,
        })
    }
}

/// What a problem says to a person: its detail, and the fields it names.
fn words_of(problem: Problem) -> String {
    let mut words = problem.detail.unwrap_or(problem.title);
    for refused in problem.errors {
        words.push_str(&format!("; {} {}", refused.field, refused.message));
    }
    words
}

/// An error with what caused it, which is where reqwest says that a connection was
/// refused or that an answer took too long.
fn causes(error: &dyn Error) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(deeper) = cause {
        text.push_str(&format!(": {deeper}"));
        cause = deeper.source();
    }
    text
}

impl Api for HttpApi {
    async fn telegram_login(&self, telegram_id: i64, request_id: &str) -> Result<Session, Failure> {
        let asked = self
            .client
            .post(self.url("/api/auth/telegram"))
            .json(&TelegramLogin { telegram_id });
        self.answer(asked, &self.service_key, request_id).await
    }

    async fn create_order(
        &self,
        who: &Caller<'_>,
        order: &OrderRequest,
        key: &IdempotencyKey,
    ) -> Result<Order, Failure> {
        let asked = self
            .client
            .post(self.url("/api/orders"))
            .header(IdempotencyKey::HEADER, key.as_str())
            .json(order);
        self.answer(asked, who.token, who.request_id).await
    }

    async fn list_orders(
        &self,
        who: &Caller<'_>,
        filter: &OrderFilter,
    ) -> Result<Vec<Order>, Failure> {
        let asked = self.client.get(self.url("/api/orders")).query(filter);
        self.answer(asked, who.token, who.request_id).await
    }

    async fn get_order(&self, who: &Caller<'_>, id: OrderId) -> Result<Order, Failure> {
        let asked = self.client.get(self.url(&format!("/api/orders/{id}")));
        self.answer(asked, who.token, who.request_id).await
    }

    async fn cancel_order(&self, who: &Caller<'_>, id: OrderId) -> Result<Order, Failure> {
        let asked = self
            .client
            .post(self.url(&format!("/api/orders/{id}/cancel")));
        self.answer(asked, who.token, who.request_id).await
    }

    async fn create_plan(&self, who: &Caller<'_>, plan: &PlanRequest) -> Result<Plan, Failure> {
        let asked = self.client.post(self.url("/api/plans")).json(plan);
        self.answer(asked, who.token, who.request_id).await
    }

    async fn list_plans(&self, who: &Caller<'_>, day: NaiveDate) -> Result<Vec<Plan>, Failure> {
        let filter = PlanFilter { service_date: day };
        let asked = self.client.get(self.url("/api/plans")).query(&filter);
        self.answer(asked, who.token, who.request_id).await
    }

    async fn get_depot(&self, who: &Caller<'_>) -> Result<Option<Depot>, Failure> {
        let asked = self.client.get(self.url("/api/depot"));
        match self.answer(asked, who.token, who.request_id).await {
            Ok(depot) => Ok(Some(depot)),
            // The API's word for a depot that has not been set yet.
            Err(Failure::Refused { status: 404, .. }) => Ok(None),
            Err(failure) => Err(failure),
        }
    }
}

#[cfg(test)]
mod tests;
