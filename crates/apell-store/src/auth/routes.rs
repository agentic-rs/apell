use super::{
  csrf_token, now, random_secret, sessions, unauthorized, Auth, LoginProvider, OAuthConfig, SignIn,
  User,
};
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
    .route("/v1/auth/github/login", get(github_login))
    .route("/v1/auth/github/callback", get(github_callback))
    .route("/v1/auth/account/login", get(account_login))
    .route("/v1/auth/account/callback", get(account_callback))
    .route("/v1/auth/account/security", get(account_security))
    .route("/v1/auth/logout", post(logout))
    .merge(super::cli::routes())
}

#[derive(Serialize)]
struct Me {
  login_available: bool,
  login_provider: Option<LoginProvider>,
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
  state.auth.clone().ok_or_else(login_unavailable)
}

fn login_unavailable() -> StoreError {
  StoreError::new(
    StatusCode::SERVICE_UNAVAILABLE,
    "login_unavailable",
    "This Store sign-in option is unavailable.",
  )
}

fn for_provider(state: &AppState, provider: LoginProvider) -> Result<Arc<Auth>> {
  let auth = enabled(state)?;
  if auth.login_provider() != provider {
    return Err(login_unavailable());
  }
  Ok(auth)
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<Me>> {
  let Some(auth) = state.auth else {
    return Ok(Json(Me {
      login_available: false,
      login_provider: None,
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
        login_provider: Some(auth.login_provider()),
        user: Some(user),
        csrf_token: Some(csrf_token(&secret)),
      }))
    }
    Err(error) if error.status == StatusCode::UNAUTHORIZED => Ok(Json(Me {
      login_available: true,
      login_provider: Some(auth.login_provider()),
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

async fn github_login(
  State(state): State<AppState>,
  Query(query): Query<Login>,
) -> Result<Response> {
  login(&state, query, LoginProvider::Github).await
}

async fn account_login(
  State(state): State<AppState>,
  Query(query): Query<Login>,
) -> Result<Response> {
  login(&state, query, LoginProvider::Account).await
}

async fn account_security(State(state): State<AppState>) -> Result<Redirect> {
  let auth = for_provider(&state, LoginProvider::Account)?;
  let SignIn::Account(account) = &auth.sign_in else {
    return Err(login_unavailable());
  };
  Ok(Redirect::to(account.security_url().as_str()))
}

async fn login(state: &AppState, query: Login, provider: LoginProvider) -> Result<Response> {
  let auth = for_provider(state, provider)?;
  let cli_code = query
    .cli_code
    .map(|code| super::cli::user_code(&code))
    .transpose()?;
  let state_secret = random_secret()?;
  let browser = random_secret()?;
  let verifier = random_secret()?;
  let (url, nonce) = match &auth.sign_in {
    SignIn::Github(config) => (authorization_url(config, &state_secret, &verifier)?, None),
    SignIn::Account(account) => {
      let nonce = random_secret()?;
      let url = account
        .authorization_url(&state_secret, &verifier, &nonce)
        .await?;
      (url, Some(nonce))
    }
  };
  let browser_for_db = browser.clone();
  blocking(move || {
    if let Some(code) = &cli_code {
      auth.store.cli_authorization(code, now())?;
    }
    auth.store.create_auth_flow(
      &state_secret,
      &browser_for_db,
      &verifier,
      nonce.as_deref(),
      cli_code.as_deref(),
      now(),
    )
  })
  .await?;
  let mut response = Redirect::temporary(url.as_str()).into_response();
  set_cookie(&mut response, sessions::FLOW_COOKIE, &browser, 600)?;
  Ok(response)
}

fn authorization_url(config: &OAuthConfig, state_secret: &str, verifier: &str) -> Result<Url> {
  let mut url =
    Url::parse("https://github.com/login/oauth/authorize").map_err(|_| StoreError::internal())?;
  url.query_pairs_mut().extend_pairs([
    ("client_id", config.client_id.as_str()),
    ("redirect_uri", config.callback().as_str()),
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

async fn github_callback(
  State(state): State<AppState>,
  headers: HeaderMap,
  Query(query): Query<Callback>,
) -> Result<Response> {
  callback(&state, headers, query, LoginProvider::Github).await
}

async fn account_callback(
  State(state): State<AppState>,
  headers: HeaderMap,
  Query(query): Query<Callback>,
) -> Result<Response> {
  callback(&state, headers, query, LoginProvider::Account).await
}

async fn callback(
  state: &AppState,
  headers: HeaderMap,
  query: Callback,
  provider: LoginProvider,
) -> Result<Response> {
  let auth = for_provider(state, provider)?;
  let (result, cli_code) = match take_callback_flow(&auth, &headers, &query, provider).await {
    Ok(flow) => {
      let result = complete_callback(auth, &headers, query, &flow).await;
      (result, flow.cli_code)
    }
    Err(error) => (Err(error), None),
  };
  let mut response = match result {
    Ok(secret) => {
      let destination = callback_destination(cli_code.as_deref(), None);
      let mut response = Redirect::to(&destination).into_response();
      set_cookie(
        &mut response,
        sessions::SESSION_COOKIE,
        &secret,
        sessions::SESSION_SECONDS,
      )?;
      response
    }
    Err(error) => {
      let reason = match error.code {
        "github_link_required" => "github_link_required",
        _ => "failed",
      };
      let resume_code = if provider == LoginProvider::Account {
        cli_code.as_deref()
      } else {
        None
      };
      Redirect::to(&callback_destination(resume_code, Some(reason))).into_response()
    }
  };
  set_cookie(&mut response, sessions::FLOW_COOKIE, "", 0)?;
  Ok(response)
}

fn callback_destination(cli_code: Option<&str>, error: Option<&str>) -> String {
  let mut destination = match cli_code {
    Some(code) => format!("/account/cli?code={code}"),
    None => "/account".into(),
  };
  if let Some(error) = error {
    destination.push(if cli_code.is_some() { '&' } else { '?' });
    destination.push_str("auth_error=");
    destination.push_str(error);
  }
  destination
}

async fn take_callback_flow(
  auth: &Auth,
  headers: &HeaderMap,
  query: &Callback,
  provider: LoginProvider,
) -> Result<sessions::AuthFlow> {
  let browser = sessions::cookie(headers, sessions::FLOW_COOKIE)?.ok_or_else(unauthorized)?;
  let state = query
    .state
    .as_ref()
    .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .ok_or_else(unauthorized)?
    .clone();
  let store = auth.store.clone();
  blocking(move || {
    store.consume_auth_flow(&state, &browser, provider == LoginProvider::Account, now())
  })
  .await
}

async fn complete_callback(
  auth: Arc<Auth>,
  headers: &HeaderMap,
  query: Callback,
  flow: &sessions::AuthFlow,
) -> Result<String> {
  if query.error.is_some() {
    return Err(unauthorized());
  }
  let code = query.code.ok_or_else(unauthorized)?;
  let (user, account_identity) = match &auth.sign_in {
    SignIn::Github(config) => (
      auth.github.sign_in(config, &code, &flow.verifier).await?,
      None,
    ),
    SignIn::Account(account) => {
      let nonce = flow.nonce.as_deref().ok_or_else(unauthorized)?;
      let identity = account.sign_in(&code, &flow.verifier, nonce).await?;
      let github = identity.github.ok_or_else(|| {
        StoreError::new(
          StatusCode::FORBIDDEN,
          "github_link_required",
          "Link GitHub in Apell Account to sign in to the Store.",
        )
      })?;
      let user = match github.login {
        Some(login) => User {
          github_id: github.id,
          github_login: login,
        },
        None => {
          let store = auth.store.clone();
          let id = github.id;
          match blocking(move || store.user_by_id(id)).await? {
            Some(user) => user,
            None => auth.github.lookup_user_id(id).await?,
          }
        }
      };
      (user, Some((identity.issuer, identity.subject)))
    }
  };
  let old_secret = sessions::cookie(headers, sessions::SESSION_COOKIE)?;
  blocking(move || {
    if let Some((issuer, subject)) = account_identity {
      auth.store.bind_account(&issuer, &subject, &user)?;
    } else {
      auth.store.upsert_user(&user)?;
    }
    auth
      .store
      .create_session(&user, old_secret.as_deref(), now())
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
    let url = authorization_url(
      &super::super::tests::config(),
      "fixture-state",
      "fixture-verifier",
    )
    .unwrap();
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
    let query = Callback {
      state: Some(state.clone()),
      code: None,
      error: Some("access_denied".into()),
    };
    let flow = take_callback_flow(&auth, &headers, &query, LoginProvider::Github)
      .await
      .unwrap();
    assert!(complete_callback(auth, &headers, query, &flow)
      .await
      .is_err());
    assert!(store.consume_flow(&state, &browser, now()).is_err());
  }
}
