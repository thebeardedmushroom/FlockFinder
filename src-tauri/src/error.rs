//! Application error type. Serialized to the frontend as `{ kind, message }`.

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("network unavailable: {0}")]
    Offline(String),
    #[error("HTTP {status} from {endpoint}: {body}")]
    Http {
        status: u16,
        endpoint: String,
        body: String,
    },
    #[error("{0} is rate limiting or overloaded; retries exhausted")]
    RateLimited(String),
    #[error("request cancelled")]
    Cancelled,
    #[error("parse error: {0}")]
    Parse(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("OSM sign-in required")]
    AuthRequired,
    #[error("not configured: {0}")]
    NotConfigured(String),
    #[error("{0}")]
    Other(String),
}

impl AppError {
    pub fn kind(&self) -> &'static str {
        match self {
            AppError::Db(_) => "db",
            AppError::Offline(_) => "offline",
            AppError::Http { .. } => "http",
            AppError::RateLimited(_) => "rate_limited",
            AppError::Cancelled => "cancelled",
            AppError::Parse(_) => "parse",
            AppError::Invalid(_) => "invalid",
            AppError::AuthRequired => "auth_required",
            AppError::NotConfigured(_) => "not_configured",
            AppError::Other(_) => "other",
        }
    }

    pub fn is_offline(&self) -> bool {
        matches!(self, AppError::Offline(_))
    }
}

impl Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("AppError", 2)?;
        s.serialize_field("kind", self.kind())?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::Parse(e.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Other(format!("I/O error: {e}"))
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_connect() || e.is_timeout() || e.is_request() {
            AppError::Offline(e.to_string())
        } else {
            AppError::Other(e.to_string())
        }
    }
}

pub type AppResult<T> = Result<T, AppError>;
