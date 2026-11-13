//! One error type for every handler. It turns into an HTTP status and the JSON body
//! described in `hearth_api::ErrorBody`.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use hearth_api::{ErrorBody, ErrorCode, ErrorInfo};

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: ErrorCode,
    pub message: String,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, code: ErrorCode, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into() }
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, message)
    }
    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, ErrorCode::Unauthorized, "Sign in first.")
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, ErrorCode::Forbidden, message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, ErrorCode::Conflict, message)
    }
    pub fn rate_limited() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::RateLimited,
            "Too many attempts. Wait a few minutes and try again.",
        )
    }
    pub fn coded(status: StatusCode, code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(status, code, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody { error: ErrorInfo { code: self.code, message: self.message } };
        (self.status, Json(body)).into_response()
    }
}

/// Anything unexpected becomes a 500. The detail goes to the log, not to the client.
macro_rules! internal_from {
    ($($ty:ty),*) => {$(
        impl From<$ty> for ApiError {
            fn from(err: $ty) -> Self {
                tracing::error!(error = %err, "request failed");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ErrorCode::Internal,
                    "Something went wrong on the Hearth. Its log has the details.",
                )
            }
        }
    )*};
}
internal_from!(sqlx::Error, std::io::Error, serde_json::Error, tokio::task::JoinError);
