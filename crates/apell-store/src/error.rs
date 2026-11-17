use axum::{
  http::{header, HeaderValue, StatusCode},
  response::{IntoResponse, Response},
  Json,
};
use serde::Serialize;

#[derive(Debug)]
pub struct StoreError {
  pub status: StatusCode,
  pub code: &'static str,
  pub message: String,
}

impl StoreError {
  pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
    Self {
      status,
      code,
      message: message.into(),
    }
  }

  pub fn invalid(message: impl Into<String>) -> Self {
    Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
  }

  pub fn missing() -> Self {
    Self::new(
      StatusCode::NOT_FOUND,
      "not_found",
      "The requested applet or release was not found.",
    )
  }

  pub fn conflict(message: impl Into<String>) -> Self {
    Self::new(StatusCode::CONFLICT, "version_conflict", message)
  }

  pub fn internal() -> Self {
    Self::new(
      StatusCode::INTERNAL_SERVER_ERROR,
      "storage_error",
      "The store could not complete this request.",
    )
  }

  pub fn busy() -> Self {
    Self::new(
      StatusCode::TOO_MANY_REQUESTS,
      "store_busy",
      "The store is busy. Try again shortly.",
    )
  }
}

impl std::fmt::Display for StoreError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&self.message)
  }
}
impl std::error::Error for StoreError {}

#[derive(Serialize)]
struct ErrorBody<'a> {
  error: ErrorDetail<'a>,
}
#[derive(Serialize)]
struct ErrorDetail<'a> {
  code: &'a str,
  message: &'a str,
}

impl IntoResponse for StoreError {
  fn into_response(self) -> Response {
    let mut response = (
      self.status,
      Json(ErrorBody {
        error: ErrorDetail {
          code: self.code,
          message: &self.message,
        },
      }),
    )
      .into_response();
    if self.status == StatusCode::UNAUTHORIZED {
      response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    if self.status == StatusCode::TOO_MANY_REQUESTS {
      response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("3"));
    }
    response
  }
}

pub type Result<T> = std::result::Result<T, StoreError>;
