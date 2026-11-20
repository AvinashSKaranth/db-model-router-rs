use thiserror::Error;

pub type Result<T> = std::result::Result<T, AppError>;

/// Strongly-typed error contract carrying HTTP-compatible semantics.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Not found: {0}")]
    NotFound(String),

    /// Optimistic-lock failure: the row changed since the client read it.
    #[error("Conflict: {0}")]
    Conflict(String),

    /// Optimistic locking is on and the write did not carry the version.
    #[error("Precondition required: {0}")]
    PreconditionRequired(String),

    #[error("Unprocessable entity: {0}")]
    UnprocessableEntity(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Database error: {0}")]
    Database(String),

    #[error("Encryption error: {0}")]
    Encryption(String),

    #[error("Validation error: {0}")]
    Validation(String),
}

impl AppError {
    #[must_use]
    pub fn status_code(&self) -> u16 {
        match self {
            Self::BadRequest(_) => 400,
            Self::Unauthorized(_) => 401,
            Self::NotFound(_) => 404,
            Self::Conflict(_) => 409,
            Self::PreconditionRequired(_) => 428,
            Self::UnprocessableEntity(_) | Self::Validation(_) => 422,
            Self::Internal(_) | Self::Database(_) | Self::Encryption(_) => 500,
        }
    }

    /// The bare message without the `Display` prefix (what HTTP bodies carry).
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::BadRequest(m)
            | Self::Unauthorized(m)
            | Self::NotFound(m)
            | Self::Conflict(m)
            | Self::PreconditionRequired(m)
            | Self::UnprocessableEntity(m)
            | Self::Internal(m)
            | Self::Database(m)
            | Self::Encryption(m)
            | Self::Validation(m) => m,
        }
    }
}
