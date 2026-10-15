use axum::{
    Json,
    extract::rejection::{BytesRejection, JsonRejection, PathRejection, QueryRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug)]
pub enum AppError {
    BadRequest(String),
    Unauthorized,
    NotFound,
    PayloadTooLarge(String),
    UnsupportedMediaType,
    TooManyRequests,
    Internal(String),
}

/// Error response body.
#[derive(Serialize, ToSchema)]
pub struct ErrorBody {
    pub error: String,
}

impl AppError {
    pub fn bad(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".into()),
            Self::NotFound => (StatusCode::NOT_FOUND, "not found".into()),
            Self::PayloadTooLarge(m) => (StatusCode::PAYLOAD_TOO_LARGE, m),
            Self::UnsupportedMediaType => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "expected Content-Type: application/json".into(),
            ),
            Self::TooManyRequests => (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded".into()),
            Self::Internal(m) => {
                tracing::error!(error = %m, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
        };
        (status, Json(ErrorBody { error })).into_response()
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

/// axum rejections: keep 413/415, everything else (incl. axum's default 422) becomes 400.
fn from_rejection(status: StatusCode, text: String) -> AppError {
    match status {
        StatusCode::PAYLOAD_TOO_LARGE => AppError::PayloadTooLarge(text),
        StatusCode::UNSUPPORTED_MEDIA_TYPE => AppError::UnsupportedMediaType,
        _ => AppError::BadRequest(text),
    }
}

macro_rules! from_rejections {
    ($($t:ty),*) => {$(
        impl From<$t> for AppError {
            fn from(r: $t) -> Self {
                from_rejection(r.status(), r.body_text())
            }
        }
    )*};
}

from_rejections!(BytesRejection, JsonRejection, PathRejection, QueryRejection);
