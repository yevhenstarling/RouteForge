//! The id a request is followed by, from the API's log into the log of every solver
//! that works on what the request asked for.
//!
//! A client may send its own as `X-Request-Id`. One that is well formed is kept, and
//! otherwise the API makes one. Either way it is echoed on the response, is the
//! `request_id` of the span everything about the request is logged in, and, when the
//! request asks for a plan, is stored on the plan's jobs. A solver puts it into the
//! span of each of those jobs under the same name (D33).
//!
//! `tracing-actix-web` gives every request an id of its own making and has no way to
//! take one from a header. So the span is built here, by [`RequestSpan`], and the
//! middleware [`assign`] decides the id before that span is made.

use std::future::{Ready, ready};
use std::sync::Arc;

use actix_web::body::MessageBody;
use actix_web::dev::{Payload, ServiceRequest, ServiceResponse};
use actix_web::http::header::{HeaderName, HeaderValue};
use actix_web::middleware::Next;
use actix_web::{Error, FromRequest, HttpMessage, HttpRequest};
use tracing::field::Empty;
use tracing::{Span, info, info_span};
use tracing_actix_web::RootSpanBuilder;
use uuid::Uuid;

use crate::error::ApiError;

/// The header the id arrives in and leaves in.
pub(crate) const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// The longest id a client may send. A UUID takes 36 characters.
const LONGEST: usize = 64;

/// The id of the request being handled. A handler takes it as an argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RequestId(Arc<str>);

impl RequestId {
    /// The id the request came with, if it is one to keep, or a new one.
    fn of(request: &ServiceRequest) -> Self {
        let sent = request
            .headers()
            .get(X_REQUEST_ID)
            .and_then(|value| value.to_str().ok())
            .filter(|text| is_well_formed(text));
        match sent {
            Some(text) => Self(text.into()),
            // UUIDv7, like every other id of the system (D11).
            None => Self(Uuid::now_v7().to_string().into()),
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether an id from outside may be kept: short, and of characters that mean nothing
/// to a log, a header or a query.
fn is_well_formed(text: &str) -> bool {
    (1..=LONGEST).contains(&text.len())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// The middleware: gives the request its id and puts it on the response.
///
/// It is wrapped round the tracing middleware, so the id is there when the span is made.
pub(crate) async fn assign(
    request: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, Error> {
    let id = RequestId::of(&request);
    request.extensions_mut().insert(id.clone());

    let mut response = next.call(request).await?;
    // An id is of characters a header can hold, so this does not fail.
    if let Ok(value) = HeaderValue::from_str(id.as_str()) {
        response.headers_mut().insert(X_REQUEST_ID, value);
    }
    Ok(response)
}

impl FromRequest for RequestId {
    type Error = ApiError;
    type Future = Ready<Result<Self, ApiError>>;

    fn from_request(request: &HttpRequest, _: &mut Payload) -> Self::Future {
        let id = request.extensions().get::<Self>().cloned();
        ready(id.ok_or_else(|| {
            ApiError::internal(anyhow::anyhow!("the request was given no id on its way in"))
        }))
    }
}

/// Makes the span a request is handled in, and logs how the request ended.
pub(crate) struct RequestSpan;

impl RootSpanBuilder for RequestSpan {
    fn on_request_start(request: &ServiceRequest) -> Span {
        let id = request.extensions().get::<RequestId>().cloned();
        info_span!(
            "request",
            request_id = id.as_ref().map(RequestId::as_str),
            method = %request.method(),
            path = request.path(),
            // Known once the token has been read, and the response made.
            user_id = Empty,
            status = Empty,
        )
    }

    fn on_request_end<B: MessageBody>(span: Span, outcome: &Result<ServiceResponse<B>, Error>) {
        let status = match outcome {
            Ok(response) => response.status(),
            Err(error) => error.as_response_error().status_code(),
        };
        span.record("status", status.as_u16());
        // Called inside the span, so the line carries the id.
        info!("handled the request");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_from_outside_is_kept_only_if_it_is_harmless() {
        for kept in [
            "a",
            "req-42",
            "0199d3c2-7b1e-7f6a-9c0d-2f4b8a1e6d35",
            "trace_1.span_2",
            &"x".repeat(64),
        ] {
            assert!(is_well_formed(kept), "{kept}");
        }
        for refused in [
            "",
            &"x".repeat(65),
            "two words",
            "line\nbreak",
            "quote\"",
            "semi;colon",
            "naïve",
            "a/b",
        ] {
            assert!(!is_well_formed(refused), "{refused:?}");
        }
    }
}
