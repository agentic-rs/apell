use super::{bearer, user_code, Polled, INTERVAL};
use crate::{
  auth::{
    now,
    routes::{blocking, enabled},
  },
  AppState, Result, StoreError,
};
use axum::{
  body::to_bytes,
  extract::{Path, Request, State},
  http::{header, HeaderMap, StatusCode},
  response::{IntoResponse, Response},
  routing::{delete, get, post},
  Json, Router,
};
use serde::{de::DeserializeOwned, Deserialize};
use std::time::Duration;

pub(in crate::auth) fn routes() -> Router<AppState> {
  Router::new()
    .route("/v1/auth/cli/start", post(start))
    .route("/v1/auth/cli/poll", post(poll))
    .route("/v1/auth/cli/me", get(me))
    .route("/v1/auth/cli/logout", post(logout))
    .route("/v1/account/cli/requests/{code}", get(details))
    .route("/v1/account/cli/requests/{code}/approve", post(approve))
    .route("/v1/account/cli/requests/{code}/deny", post(deny))
    .route("/v1/account/cli/credentials", get(credentials))
    .route("/v1/account/cli/credentials/{id}", delete(revoke))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Poll {
  device_code: String,
}

async fn input<T: DeserializeOwned>(request: Request) -> Result<T> {
  if !request
    .headers()
    .get(header::CONTENT_TYPE)
    .and_then(|value| value.to_str().ok())
    .is_some_and(|value| {
      value
        .split(';')
        .next()
        .is_some_and(|mime| mime.trim() == "application/json")
    })
  {
    return Err(StoreError::invalid(
      "Use application/json for this request.",
    ));
  }
  let body = tokio::time::timeout(Duration::from_secs(10), to_bytes(request.into_body(), 4096))
    .await
    .map_err(|_| StoreError::invalid("The request timed out."))?
    .map_err(|_| StoreError::invalid("The request exceeds its size limit."))?;
  serde_json::from_slice(&body).map_err(|_| StoreError::invalid("Invalid request fields."))
}

async fn start(State(state): State<AppState>, request: Request) -> Result<Response> {
  let auth = enabled(&state)?;
  let _: Empty = input(request).await?;
  let origin = auth.config.origin.clone();
  let started = blocking(move || auth.store.start_cli_authorization(now())).await?;
  let verification_uri = format!("{origin}/account/cli");
  let verification_uri_complete = format!("{verification_uri}?code={}", started.user_code);
  Ok(
    Json(serde_json::json!({
      "device_code":started.device_code,"user_code":started.user_code,
      "verification_uri":verification_uri,"verification_uri_complete":verification_uri_complete,
      "expires_at":started.expires_at,"interval":started.interval,
    }))
    .into_response(),
  )
}

async fn poll(State(state): State<AppState>, request: Request) -> Result<Response> {
  let auth = enabled(&state)?;
  let input: Poll = input(request).await?;
  match blocking(move || auth.store.poll_cli_authorization(&input.device_code, now())).await? {
    Polled::Pending => Ok(
      (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({"status":"pending","interval":INTERVAL})),
      )
        .into_response(),
    ),
    Polled::Authorized(credential) => Ok(Json(credential).into_response()),
  }
}

async fn details(
  State(state): State<AppState>,
  Path(code): Path<String>,
  headers: HeaderMap,
) -> Result<Response> {
  let auth = enabled(&state)?;
  blocking(move || {
    auth.browser_user(&headers)?;
    auth
      .store
      .cli_authorization(&code, now())
      .map(|details| Json(details).into_response())
  })
  .await
}

async fn decide(
  state: AppState,
  code: String,
  request: Request,
  approve: bool,
) -> Result<Response> {
  let auth = enabled(&state)?;
  let headers = request.headers().clone();
  let checked = auth.clone();
  let user = blocking(move || checked.require_browser_mutation(&headers)).await?;
  let _: Empty = input(request).await?;
  let code = user_code(&code)?;
  blocking(move || {
    auth
      .store
      .decide_cli_authorization(&code, user.github_id, approve, now())
  })
  .await?;
  Ok(Json(serde_json::json!({"ok":true})).into_response())
}

async fn approve(
  State(state): State<AppState>,
  Path(code): Path<String>,
  request: Request,
) -> Result<Response> {
  decide(state, code, request, true).await
}

async fn deny(
  State(state): State<AppState>,
  Path(code): Path<String>,
  request: Request,
) -> Result<Response> {
  decide(state, code, request, false).await
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> Result<Response> {
  let auth = enabled(&state)?;
  let token = bearer(&headers)?;
  blocking(move || {
    auth
      .store
      .cli_identity(&token, now())
      .map(|identity| Json(identity).into_response())
  })
  .await
}

async fn logout(State(state): State<AppState>, request: Request) -> Result<Response> {
  let auth = enabled(&state)?;
  let token = bearer(request.headers())?;
  let checked = auth.clone();
  let checked_token = token.clone();
  blocking(move || checked.store.cli_identity(&checked_token, now())).await?;
  let _: Empty = input(request).await?;
  blocking(move || auth.store.logout_cli(&token)).await?;
  Ok(Json(serde_json::json!({"ok":true})).into_response())
}

async fn credentials(State(state): State<AppState>, headers: HeaderMap) -> Result<Response> {
  let auth = enabled(&state)?;
  blocking(move || {
    let user = auth.browser_user(&headers)?;
    let credentials = auth.store.cli_credentials(user.github_id, now())?;
    Ok(Json(serde_json::json!({"credentials":credentials})).into_response())
  })
  .await
}

async fn revoke(
  State(state): State<AppState>,
  Path(id): Path<String>,
  request: Request,
) -> Result<Response> {
  let auth = enabled(&state)?;
  let checked = auth.clone();
  let headers = request.headers().clone();
  let user = blocking(move || checked.require_browser_mutation(&headers)).await?;
  let _: Empty = input(request).await?;
  blocking(move || auth.store.revoke_cli_credential(user.github_id, &id)).await?;
  Ok(Json(serde_json::json!({"ok":true})).into_response())
}
