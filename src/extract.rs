//! Extractors whose rejections go through [`AppError`] (400 instead of axum's 422).

use axum::{
    body::Bytes,
    extract::{FromRequest, FromRequestParts, Request},
};

use crate::error::AppError;

#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(AppError))]
pub struct Path<T>(pub T);

#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(AppError))]
pub struct Query<T>(pub T);

#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(AppError))]
pub struct JsonBody<T>(pub T);

/// Raw body, still capped by `DefaultBodyLimit` (→ 413). Parsed inside the handler.
pub struct Body(pub Bytes);

impl<S: Send + Sync> FromRequest<S> for Body {
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(Bytes::from_request(req, state).await?))
    }
}
