//! Errors as the API reports them: a kind that sets the status, and an
//! RFC 9457 problem body.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// What went wrong, which sets the HTTP status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// The request is malformed.
    Invalid,
    /// No credential, or one that is not valid.
    Unauthorized,
    /// A valid credential that may not do this.
    Forbidden,
    NotFound,
    MethodNotAllowed,
    /// Not allowed now, given the current state.
    Conflict,
    PayloadTooLarge,
    UnsupportedMediaType,
    /// Well formed, but refers to something that cannot be: an unknown
    /// thread, a clock out of order, a used-up room.
    Unprocessable,
    /// This daemon failed.
    Internal,
    /// Another daemon failed or did not answer.
    Upstream,
    /// Refused for now; retrying later may pass.
    Unavailable,
}

impl ErrorKind {
    pub fn status(self) -> u16 {
        match self {
            ErrorKind::Invalid => 400,
            ErrorKind::Unauthorized => 401,
            ErrorKind::Forbidden => 403,
            ErrorKind::NotFound => 404,
            ErrorKind::MethodNotAllowed => 405,
            ErrorKind::Conflict => 409,
            ErrorKind::PayloadTooLarge => 413,
            ErrorKind::UnsupportedMediaType => 415,
            ErrorKind::Unprocessable => 422,
            ErrorKind::Internal => 500,
            ErrorKind::Upstream => 502,
            ErrorKind::Unavailable => 503,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            ErrorKind::Invalid => "Bad Request",
            ErrorKind::Unauthorized => "Unauthorized",
            ErrorKind::Forbidden => "Forbidden",
            ErrorKind::NotFound => "Not Found",
            ErrorKind::MethodNotAllowed => "Method Not Allowed",
            ErrorKind::Conflict => "Conflict",
            ErrorKind::PayloadTooLarge => "Content Too Large",
            ErrorKind::UnsupportedMediaType => "Unsupported Media Type",
            ErrorKind::Unprocessable => "Unprocessable Content",
            ErrorKind::Internal => "Internal Server Error",
            ErrorKind::Upstream => "Bad Gateway",
            ErrorKind::Unavailable => "Service Unavailable",
        }
    }

    /// The kind an HTTP status reports, for a client reading a problem.
    pub fn from_status(status: u16) -> Option<Self> {
        Some(match status {
            400 => ErrorKind::Invalid,
            401 => ErrorKind::Unauthorized,
            403 => ErrorKind::Forbidden,
            404 => ErrorKind::NotFound,
            405 => ErrorKind::MethodNotAllowed,
            409 => ErrorKind::Conflict,
            413 => ErrorKind::PayloadTooLarge,
            415 => ErrorKind::UnsupportedMediaType,
            422 => ErrorKind::Unprocessable,
            500 => ErrorKind::Internal,
            502 => ErrorKind::Upstream,
            503 => ErrorKind::Unavailable,
            _ => return None,
        })
    }
}

/// An error the caller (or another daemon) caused, with a message the API
/// may show. Internal failures carry no detail outward.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct Error {
    pub kind: ErrorKind,
    pub detail: String,
    /// For [`ErrorKind::Unavailable`]: when to retry, in seconds.
    pub retry_after: Option<u32>,
}

impl Error {
    pub fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            retry_after: None,
        }
    }

    #[must_use]
    pub fn retry_after(mut self, seconds: u32) -> Self {
        self.retry_after = Some(seconds);
        self
    }

    /// The body this error is answered with. A 5xx from this daemon says no
    /// more than its status.
    pub fn problem(&self) -> Problem {
        let detail = (self.kind != ErrorKind::Internal && !self.detail.is_empty())
            .then(|| self.detail.clone());
        Problem::new(self.kind, detail)
    }
}

/// RFC 9457 problem details: every operation's only error response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Problem {
    /// A URI reference naming the problem type; `about:blank` when absent.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub problem_type: Option<String>,
    pub title: String,
    #[schema(minimum = 100, maximum = 599)]
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
}

impl Problem {
    pub fn new(kind: ErrorKind, detail: Option<String>) -> Self {
        Self {
            problem_type: None,
            title: kind.title().to_string(),
            status: kind.status(),
            detail,
            instance: None,
        }
    }
}
