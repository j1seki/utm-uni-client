use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TLS error: {0}")]
    Tls(#[from] rustls::Error),

    #[error("invalid configuration: {0}")]
    Config(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("codable error: {0}")]
    Codable(String),

    #[error("authentication failed: {0}")]
    Auth(String),

    #[error("fingerprint mismatch (expected {expected}, got {actual})")]
    FingerprintMismatch { expected: String, actual: String },

    #[error("server is not trusted; connection fingerprint is {fingerprint}")]
    Untrusted { fingerprint: String },

    #[error("password required")]
    PasswordRequired,

    #[error("invalid password")]
    InvalidPassword,

    #[error("connection closed")]
    Closed,

    #[error("request timed out")]
    Timeout,

    #[error("server returned an error: {0}")]
    Remote(String),

    #[error("SPICE error: {0}")]
    Spice(String),

    #[error("unsupported: {0}")]
    Unsupported(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid request: {0}")]
    BadRequest(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Config(format!("JSON error: {e}"))
    }
}
