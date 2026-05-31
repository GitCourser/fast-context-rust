use std::fmt;

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FastContextErrorKind {
    Timeout,
    PayloadTooLarge,
    RateLimited,
    AuthError,
    ServerError,
    NetworkError,
    InvalidResponse,
    MissingApiKey,
    NotImplemented,
}

impl fmt::Display for FastContextErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = match self {
            Self::Timeout => "TIMEOUT",
            Self::PayloadTooLarge => "PAYLOAD_TOO_LARGE",
            Self::RateLimited => "RATE_LIMITED",
            Self::AuthError => "AUTH_ERROR",
            Self::ServerError => "SERVER_ERROR",
            Self::NetworkError => "NETWORK_ERROR",
            Self::InvalidResponse => "INVALID_RESPONSE",
            Self::MissingApiKey => "MISSING_API_KEY",
            Self::NotImplemented => "NOT_IMPLEMENTED",
        };
        formatter.write_str(code)
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{kind}: {message}")]
pub struct FastContextError {
    pub kind: FastContextErrorKind,
    pub message: String,
    pub status: Option<u16>,
}

impl FastContextError {
    pub fn new(kind: FastContextErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            status: None,
        }
    }

    pub fn with_status(
        kind: FastContextErrorKind,
        status: u16,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            status: Some(status),
        }
    }

    pub fn user_hint(&self) -> &'static str {
        match self.kind {
            FastContextErrorKind::Timeout => {
                "Windsurf request timed out. Retry, narrow the query, or reduce request size."
            }
            FastContextErrorKind::PayloadTooLarge => {
                "The request payload is too large. Reduce tree/context size or exclusions."
            }
            FastContextErrorKind::RateLimited => {
                "Windsurf rate limit was reached. Wait before retrying."
            }
            FastContextErrorKind::AuthError => {
                "Authentication failed. Set a valid WINDSURF_API_KEY or re-run key extraction."
            }
            FastContextErrorKind::ServerError => {
                "Windsurf server returned an error. Retry later or check service status."
            }
            FastContextErrorKind::NetworkError => {
                "Network request failed. Check connectivity, proxy, and TLS settings."
            }
            FastContextErrorKind::InvalidResponse => {
                "Windsurf returned an invalid response. Capture debug output and retry."
            }
            FastContextErrorKind::MissingApiKey => {
                "WINDSURF_API_KEY is not set. Export it or use the Windsurf key extractor."
            }
            FastContextErrorKind::NotImplemented => {
                "This Rust search path is not fully implemented yet. Use the legacy Node baseline or a newer Rust release for real search."
            }
        }
    }

    pub fn user_message(&self) -> String {
        match self.status {
            Some(status) => format!(
                "{} (HTTP {status}): {}\n{}",
                self.kind,
                self.message,
                self.user_hint()
            ),
            None => format!("{}: {}\n{}", self.kind, self.message, self.user_hint()),
        }
    }
}

pub fn classify_http_status(status: u16, message: impl Into<String>) -> FastContextError {
    let kind = match status {
        413 => FastContextErrorKind::PayloadTooLarge,
        429 => FastContextErrorKind::RateLimited,
        401 | 403 => FastContextErrorKind::AuthError,
        408 | 504 => FastContextErrorKind::Timeout,
        400..=599 => FastContextErrorKind::ServerError,
        _ => FastContextErrorKind::NetworkError,
    };
    FastContextError::with_status(kind, status, message)
}
