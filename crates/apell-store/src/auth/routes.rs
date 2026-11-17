use super::{csrf_token, now, random_secret, sessions, unauthorized, Auth, User};
use crate::{AppState, Result, StoreError};
use axum::{
  extract::{Query, State},
  http::{header, HeaderMap, HeaderValue, StatusCode},
  response::{IntoResponse, Redirect, Response},
  routing::{get, post},
  Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::Semaphore;
use url::Url;

static STORAGE_OPERATIONS: Semaphore = Semaphore::const_new(8);

pub(crate) fn routes() -> Router<AppState> {
  Router::new()
    .route("/v1/auth/me", get(me))
    .route("/v1/auth/github/login", get(login))
    .route("/v1/auth/github/callback", get(callback))
    .route("/v1/auth/logout", post(logout))
    .merge(super::cli::routes())
}

#[derive(Serialize)]
struct Me {
  login_available: bool,
  user: Option<User>,
  csrf_token: Option<String>,
}

pub(super) async fn blocking<T: Send + 'static>(
  operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
  let permit = STORAGE_OPERATIONS
    .try_acquire()
    .map_err(|_| StoreError::busy())?;
  tokio::task::spawn_blocking(move || {
    let _permit = permit;
    operation()
  })
  .await
  .map_err(|_| StoreError::internal())?
}

pub(super) fn enabled(state: &AppState) -> Result<Arc<Auth>> {
  state.auth.clone().ok_or_else(|| {
    StoreError::new(
      StatusCode::SERVICE_UNAVAILABLE,
      "login_unavailable",
      "GitHub sign-in is not configured.",
    )
  })
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<Me>> {
  let Some(auth) = state.auth else {
    return Ok(Json(Me {
      login_available: false,
      user: None,
      csrf_token: None,
    }));
  };
  blocking(move || match auth.browser_user(&headers) {
    Ok(user) => {
      let secret =
        sessions::cookie(&headers, sessions::SESSION_COOKIE)?.ok_or_else(unauthorized)?;
      Ok(Json(Me {
        login_available: true,
        user: Some(user),
        csrf_token: Some(csrf_token(&secret)),
      }))
    }
    Err(error) if error.status == StatusCode::UNAUTHORIZED => Ok(Json(Me {
      login_available: true,
      user: None,
      csrf_token: None,
    })),
    Err(error) => Err(error),
  })
  .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
  cli_code: Option<String>,
}

async fn login(State(state): State<AppState>, Query(query): Query<Login>) -> Result<Response> {
  let auth = enabled(&state)?;
  let cli_code = query
    .cli_code
    .map(|code| super::cli::user_code(&code))
    .transpose()?;
  let state_secret = random_secret()?;
  let browser = random_secret()?;
  let verifier = random_secret()?;
  let url = authorization_url(&auth, &state_secret, &verifier)?;
  let browser_for_db = browser.clone();
  blocking(move || {
    if let Some(code) = &cli_code {
      auth.store.cli_authorization(code, now())?;
    }
    auth.store.create_flow_for_cli(
      &state_secret,
      &browser_for_db,
      &verifier,
      cli_code.as_deref(),
      now(),
    )
  })
  .await?;
  let mut response = Redirect::temporary(url.as_str()).into_response();
  set_cookie(&mut response, sessions::FLOW_COOKIE, &browser, 600)?;
  Ok(response)
}

fn authorization_url(auth: &Auth, state_secret: &str, verifier: &str) -> Result<Url> {
  let mut url =
    Url::parse("https://github.com/login/oauth/authorize").map_err(|_| StoreError::internal())?;
  url.query_pairs_mut().extend_pairs([
    ("client_id", auth.config.client_id.as_str()),
    ("redirect_uri", auth.config.callback().as_str()),
    ("scope", ""),
    ("state", state_secret),
    ("code_challenge_method", "S256"),
    (
      "code_challenge",
      URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()))
        .as_str(),
    ),
  ]);
  Ok(url)
}

#[derive(Deserialize)]
struct Callback {
  code: Option<String>,
  state: Option<String>,
  error: Option<String>,
}

async fn callback(
  State(state): State<AppState>,
  headers: HeaderMap,
  Query(query): Query<Callback>,
) -> Result<Response> {
  let auth = enabled(&state)?;
  let result = complete_callback(auth, headers, query).await;
  let mut response = match result {
    Ok((secret, cli_code)) => {
      let destination = cli_code
        .map(|code| format!("/account/cli?code={code}"))
        .unwrap_or_else(|| "/account".into());
      let mut response = Redirect::to(&destination).into_response();
      set_cookie(
        &mut response,
        sessions::SESSION_COOKIE,
        &secret,
        sessions::SESSION_SECONDS,
      )?;
      response
    }
    Err(_) => Redirect::to("/account?auth_error=failed").into_response(),
  };
  set_cookie(&mut response, sessions::FLOW_COOKIE, "", 0)?;
  Ok(response)
}

async fn complete_callback(
  auth: Arc<Auth>,
  headers: HeaderMap,
  query: Callback,
) -> Result<(String, Option<String>)> {
  let browser = sessions::cookie(&headers, sessions::FLOW_COOKIE)?.ok_or_else(unauthorized)?;
  let state = query
    .state
    .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .ok_or_else(unauthorized)?;
  let flow_auth = auth.clone();
  let (verifier, cli_code) = blocking(move || {
    flow_auth
      .store
      .consume_flow_for_cli(&state, &browser, now())
  })
  .await?;
  if query.error.is_some() {
    return Err(unauthorized());
  }
  let code = query.code.ok_or_else(unauthorized)?;
  let user = auth.github.sign_in(&auth.config, &code, &verifier).await?;
  let old_secret = sessions::cookie(&headers, sessions::SESSION_COOKIE)?;
  blocking(move || {
    auth.store.upsert_user(&user)?;
    let secret = auth
      .store
      .create_session(&user, old_secret.as_deref(), now())?;
    Ok((secret, cli_code))
  })
  .await
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response> {
  let auth = enabled(&state)?;
  blocking(move || {
    auth.require_browser_mutation(&headers)?;
    let secret = sessions::cookie(&headers, sessions::SESSION_COOKIE)?.ok_or_else(unauthorized)?;
    auth.store.delete_session(&secret)
  })
  .await?;
  let mut response = StatusCode::NO_CONTENT.into_response();
  set_cookie(&mut response, sessions::SESSION_COOKIE, "", 0)?;
  Ok(response)
}

fn set_cookie(response: &mut Response, name: &str, value: &str, max_age: i64) -> Result<()> {
  let value = HeaderValue::from_str(&format!(
    "{name}={value}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={max_age}"
  ))
  .map_err(|_| StoreError::internal())?;
  response.headers_mut().append(header::SET_COOKIE, value);
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn authorization_uses_pkce_and_cookies_are_host_only() {
    let directory = tempfile::tempdir().unwrap();
    let auth = Auth::new(
      super::super::tests::config(),
      crate::Store::open(directory.path()).unwrap(),
    )
    .unwrap();
    let url = authorization_url(&auth, "fixture-state", "fixture-verifier").unwrap();
    let values: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(url.origin().ascii_serialization(), "https://github.com");
    assert_eq!(values["code_challenge_method"], "S256");
    assert_eq!(
      values["code_challenge"],
      URL_SAFE_NO_PAD.encode(Sha256::digest(b"fixture-verifier"))
    );
    assert_eq!(
      values["redirect_uri"],
      "https://store.example.invalid/api/v1/auth/github/callback"
    );
    assert_eq!(values["scope"], "");
    assert!(!url.as_str().contains("fixture-secret"));
    let mut response = StatusCode::NO_CONTENT.into_response();
    set_cookie(&mut response, sessions::SESSION_COOKIE, "fixture", 10).unwrap();
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.starts_with("__Host-"));
    assert!(cookie.contains("Path=/; Secure; HttpOnly; SameSite=Lax"));
    assert!(!cookie.contains("Domain="));
  }

  #[tokio::test]
  async fn denied_callback_consumes_browser_bound_state() {
    let directory = tempfile::tempdir().unwrap();
    let store = crate::Store::open(directory.path()).unwrap();
    let state = random_secret().unwrap();
    let browser = random_secret().unwrap();
    store
      .create_flow(&state, &browser, "fixture-verifier", now())
      .unwrap();
    let auth = Arc::new(Auth::new(super::super::tests::config(), store.clone()).unwrap());
    let mut headers = HeaderMap::new();
    headers.insert(
      header::COOKIE,
      format!("{}={browser}", sessions::FLOW_COOKIE)
        .parse()
        .unwrap(),
    );
    assert!(complete_callback(
      auth,
      headers,
      Callback {
        state: Some(state.clone()),
        code: None,
        error: Some("access_denied".into())
      }
    )
    .await
    .is_err());
    assert!(store.consume_flow(&state, &browser, now()).is_err());
  }
}
