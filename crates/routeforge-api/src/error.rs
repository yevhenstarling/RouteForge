//! How a request is refused.
//!
//! Every error leaves as `application/problem+json` (RFC 9457): the ones the handlers
//! return, and the ones the framework finds before a handler runs, such as a body that
//! is not JSON or a path that nothing serves. A client has one shape to read.
//!
//! What each status means here:
//!
//! | Status | |
//! |---|---|
//! | 400 | The request could not be read: broken JSON, a path or a query that does not parse. |
//! | 401 | No token, or one that is not valid. |
//! | 403 | A valid token whose role does not allow this. |
//! | 404 | No such thing, or none that the caller may know of. |
//! | 409 | The thing is not in a state for this. |
//! | 413, 415 | A body that is too large, or is not JSON. |
//! | 422 | The request was read, and something in it cannot be accepted. |
//! | 500 | The fault is the server's. The cause is logged and not sent. |
//! | 503 | The API cannot do its work yet: `/readyz` says so. |

use actix_web::error::{JsonPayloadError, PathError, QueryPayloadError};
use actix_web::http::StatusCode;
use actix_web::http::header;
use actix_web::{HttpResponse, ResponseError, web};
use routeforge_domain::db::DbError;
use routeforge_domain::{FieldError, Problem};

/// The media type of every error body.
pub(crate) const PROBLEM_JSON: &str = "application/problem+json";

/// The largest body the API reads. The largest it has a use for is a few hundred bytes.
const BODY_LIMIT: usize = 64 * 1024;

/// Why a request was refused. What `Display` gives is the `detail` the client reads.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(&'static str),
    #[error("your role does not allow this")]
    Forbidden,
    #[error("{0}")]
    NotFound(&'static str),
    #[error("{0}")]
    Conflict(String),
    #[error("the body is larger than {BODY_LIMIT} bytes")]
    TooLarge,
    #[error("the body has to be JSON, sent as `Content-Type: application/json`")]
    NotJson,
    #[error("{detail}")]
    Invalid {
        detail: String,
        /// The fields that were refused, if it is known which.
        errors: Vec<FieldError>,
    },
    /// The cause is for the log. The client is told only that it was not its fault.
    #[error("something went wrong on the server")]
    Internal(#[source] anyhow::Error),
    #[error("{0}")]
    Unavailable(&'static str),
}

impl ApiError {
    /// A request with fields that cannot be accepted.
    pub(crate) fn invalid(errors: Vec<FieldError>) -> Self {
        Self::Invalid {
            detail: "the request has fields that cannot be accepted".to_owned(),
            errors,
        }
    }

    /// A request with one field that cannot be accepted.
    pub(crate) fn invalid_field(field: &str, message: impl Into<String>) -> Self {
        Self::invalid(vec![FieldError {
            field: field.to_owned(),
            message: message.into(),
        }])
    }

    pub(crate) fn internal(cause: impl Into<anyhow::Error>) -> Self {
        Self::Internal(cause.into())
    }

    fn problem(&self) -> Problem {
        let status = self.status_code();
        Problem {
            kind: "about:blank".to_owned(),
            title: status.canonical_reason().unwrap_or("Error").to_owned(),
            status: status.as_u16(),
            detail: Some(self.to_string()),
            errors: match self {
                Self::Invalid { errors, .. } => errors.clone(),
                _ => Vec::new(),
            },
        }
    }
}

impl ResponseError for ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::NotJson => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::Invalid { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    fn error_response(&self) -> HttpResponse {
        let mut response = HttpResponse::build(self.status_code());
        if matches!(self, Self::Unauthorized(_)) {
            response.insert_header((header::WWW_AUTHENTICATE, "Bearer"));
        }
        // `json` leaves a content type alone that is already set.
        response
            .insert_header((header::CONTENT_TYPE, PROBLEM_JSON))
            .json(self.problem())
    }
}

impl From<DbError> for ApiError {
    fn from(error: DbError) -> Self {
        match error {
            DbError::EmailTaken | DbError::ActivePlanExists => Self::Conflict(error.to_string()),
            // What the handlers pass on has been checked, so the rest is not the
            // caller's doing: a database that is away, or one this build cannot read.
            DbError::OutOfRange(_) | DbError::Unreadable(_) | DbError::Sqlx(_) => {
                Self::internal(error)
            }
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        Self::internal(error)
    }
}

impl From<JsonPayloadError> for ApiError {
    fn from(error: JsonPayloadError) -> Self {
        match error {
            JsonPayloadError::Overflow { .. } | JsonPayloadError::OverflowKnownLength { .. } => {
                Self::TooLarge
            }
            JsonPayloadError::ContentType => Self::NotJson,
            // JSON that says something the API cannot take: a field that is missing, a
            // position off the globe, a budget without a limit. serde does not say
            // which field, only where in the text.
            JsonPayloadError::Deserialize(error) if error.is_data() => Self::Invalid {
                detail: error.to_string(),
                errors: Vec::new(),
            },
            JsonPayloadError::Deserialize(error) => {
                Self::BadRequest(format!("the body is not JSON: {error}"))
            }
            other => Self::BadRequest(format!("the body could not be read: {other}")),
        }
    }
}

/// How bodies are read: up to [`BODY_LIMIT`], and refused as a problem.
pub(crate) fn json_config() -> web::JsonConfig {
    web::JsonConfig::default()
        .limit(BODY_LIMIT)
        .error_handler(|error, _| ApiError::from(error).into())
}

/// A path that does not parse, such as an id that is no UUID, is refused as a problem.
pub(crate) fn path_config() -> web::PathConfig {
    web::PathConfig::default()
        .error_handler(|error: PathError, _| ApiError::BadRequest(error.to_string()).into())
}

/// A query that does not parse is refused as a problem.
pub(crate) fn query_config() -> web::QueryConfig {
    web::QueryConfig::default()
        .error_handler(|error: QueryPayloadError, _| ApiError::BadRequest(error.to_string()).into())
}

/// What a path that nothing serves is answered with.
pub(crate) const NOTHING_HERE: ApiError = ApiError::NotFound("nothing is served at this path");

/// The answer to whatever matches no route.
pub(crate) async fn not_found() -> Result<HttpResponse, ApiError> {
    Err(NOTHING_HERE)
}

#[cfg(test)]
mod tests {
    use actix_web::body::to_bytes;

    use super::*;

    async fn sent(error: ApiError) -> (StatusCode, Option<header::HeaderValue>, Problem) {
        let response = error.error_response();
        let status = response.status();
        let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
        let body = to_bytes(response.into_body()).await.unwrap();
        (status, content_type, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn every_error_is_a_problem_with_its_own_status() {
        let errors = [
            (
                ApiError::BadRequest("broken".to_owned()),
                400,
                "Bad Request",
            ),
            (ApiError::Unauthorized("no token"), 401, "Unauthorized"),
            (ApiError::Forbidden, 403, "Forbidden"),
            (
                ApiError::NotFound("there is no such order"),
                404,
                "Not Found",
            ),
            (ApiError::Conflict("too late".to_owned()), 409, "Conflict"),
            (ApiError::TooLarge, 413, "Payload Too Large"),
            (ApiError::NotJson, 415, "Unsupported Media Type"),
            (
                ApiError::invalid_field("demand", "must be at least 1"),
                422,
                "Unprocessable Entity",
            ),
            (
                ApiError::internal(anyhow::anyhow!("the disk is full")),
                500,
                "Internal Server Error",
            ),
            (
                ApiError::Unavailable("no schema"),
                503,
                "Service Unavailable",
            ),
        ];
        for (error, status, title) in errors {
            let detail = error.to_string();
            let (sent_status, content_type, problem) = sent(error).await;
            assert_eq!(sent_status.as_u16(), status);
            assert_eq!(content_type.unwrap(), PROBLEM_JSON, "{status}");
            assert_eq!(
                (
                    problem.kind.as_str(),
                    problem.title.as_str(),
                    problem.status
                ),
                ("about:blank", title, status)
            );
            assert_eq!(problem.detail.as_deref(), Some(detail.as_str()));
        }
    }

    #[tokio::test]
    async fn the_cause_of_a_server_error_is_kept_from_the_client() {
        let error = ApiError::internal(anyhow::anyhow!("password authentication failed"));
        // It is there for the log.
        assert!(format!("{error:?}").contains("password authentication failed"));

        let (_, _, problem) = sent(error).await;
        let body = serde_json::to_string(&problem).unwrap();
        assert!(!body.contains("password"), "{body}");
    }

    #[tokio::test]
    async fn a_refused_field_is_named() {
        let (_, _, problem) = sent(ApiError::invalid_field("demand", "must be at least 1")).await;
        assert_eq!(
            problem.errors,
            [FieldError {
                field: "demand".to_owned(),
                message: "must be at least 1".to_owned(),
            }]
        );
    }

    #[tokio::test]
    async fn a_missing_token_says_how_to_authenticate() {
        let response = ApiError::Unauthorized("no token").error_response();
        assert_eq!(
            response.headers().get(header::WWW_AUTHENTICATE).unwrap(),
            "Bearer"
        );
        let response = ApiError::Forbidden.error_response();
        assert!(response.headers().get(header::WWW_AUTHENTICATE).is_none());
    }

    #[test]
    fn the_databases_refusals_become_conflicts_and_its_troubles_stay_inside() {
        assert!(matches!(
            ApiError::from(DbError::EmailTaken),
            ApiError::Conflict(detail) if detail.contains("already registered")
        ));
        assert!(matches!(
            ApiError::from(DbError::ActivePlanExists),
            ApiError::Conflict(detail) if detail.contains("plan in progress")
        ));
        for trouble in [
            DbError::Unreadable("a status of `teleported`".to_owned()),
            DbError::OutOfRange("the route's distance"),
            DbError::Sqlx(sqlx::Error::PoolTimedOut),
        ] {
            assert!(matches!(ApiError::from(trouble), ApiError::Internal(_)));
        }
    }
}
