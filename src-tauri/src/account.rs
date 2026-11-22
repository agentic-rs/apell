mod oidc;

use crate::{commands::blocking, host::Host};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use oidc::{Config, Provider, TokenError, Tokens};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Url};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::Mutex;
use zeroize::{Zeroize, Zeroizing};

const EVENT: &str = "apell-account-changed";
const SESSION_SLOT: &str = "apell-account-session-v1";
const SIGN_IN_TTL: Duration = Duration::from_secs(10 * 60);
const SIGN_IN_EXPIRED: &str = "Apell Account sign-in expired. Try again.";
const CLIENT_UNAVAILABLE: &str = "Apell Account is unavailable in this build.";
const SIGN_IN_REQUIRED: &str = "Sign in to Apell Account to sync settings.";

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum SignInState {
  Unavailable,
  SignedOut,
  SigningIn,
  SignedIn,
}

#[derive(Serialize)]
struct Status {
  state: SignInState,
  email: Option<String>,
  error: Option<String>,
}

#[derive(Clone)]
struct Attempt {
  state: String,
  nonce: String,
  verifier: Zeroizing<String>,
  expires: Instant,
  provider: Option<Provider>,
  completing: bool,
}

impl Attempt {
  fn new() -> Result<Self, String> {
    fn random() -> Result<String, String> {
      let mut bytes = Zeroizing::new([0u8; 32]);
      getrandom::fill(bytes.as_mut()).map_err(|_| "Could not start Apell Account sign-in.")?;
      Ok(URL_SAFE_NO_PAD.encode(bytes.as_ref()))
    }
    Ok(Self {
      state: random()?,
      nonce: random()?,
      verifier: Zeroizing::new(random()?),
      expires: Instant::now() + SIGN_IN_TTL,
      provider: None,
      completing: false,
    })
  }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredSession {
  issuer: String,
  client_id: String,
  resource: String,
  subject: String,
  email: Option<String>,
  refresh_token: String,
}

impl StoredSession {
  fn matches(&self, config: &Config) -> bool {
    self.issuer == config.issuer.as_str()
      && self.client_id == config.client_id
      && self.resource == config.resource
      && oidc::visible_ascii(&self.subject, 255)
      && oidc::valid_token(&self.refresh_token)
  }

  fn account_key(&self) -> String {
    let mut digest = Sha256::new();
    digest.update(self.issuer.as_bytes());
    digest.update([0]);
    digest.update(self.subject.as_bytes());
    format!("{:x}", digest.finalize())
  }
}

impl Drop for StoredSession {
  fn drop(&mut self) {
    self.refresh_token.zeroize();
  }
}

struct AccessToken {
  value: Zeroizing<String>,
  expires: Instant,
}

impl AccessToken {
  fn new(value: String, expires_in: u64) -> Self {
    Self {
      value: Zeroizing::new(value),
      expires: Instant::now() + Duration::from_secs(expires_in.min(86_400)),
    }
  }
}

struct Session {
  stored: StoredSession,
  pending_vault_write: bool,
  access_token: Option<AccessToken>,
}

#[derive(Default)]
struct State {
  loaded: bool,
  session: Option<Session>,
  attempt: Option<Attempt>,
  error: Option<String>,
}

impl State {
  fn status(&self, enabled: bool) -> Status {
    let state = if !enabled {
      SignInState::Unavailable
    } else if self.session.is_some() {
      SignInState::SignedIn
    } else if self.attempt.is_some() {
      SignInState::SigningIn
    } else {
      SignInState::SignedOut
    };
    Status {
      state,
      email: self
        .session
        .as_ref()
        .and_then(|session| session.stored.email.clone()),
      error: self.error.clone(),
    }
  }

  async fn load(&mut self, app: &AppHandle, config: &Config) -> Result<(), String> {
    if self.loaded {
      return Ok(());
    }
    let app = app.clone();
    let payload =
      blocking(move || tauri_plugin_apell_credentials::read(&app, SESSION_SLOT)).await?;
    if let Some(payload) = payload.map(Zeroizing::new) {
      if let Ok(stored) = serde_json::from_str::<StoredSession>(&payload) {
        if stored.matches(config) {
          self.session = Some(Session {
            stored,
            pending_vault_write: false,
            access_token: None,
          });
        }
      }
    }
    self.loaded = true;
    Ok(())
  }
}

pub(crate) struct Access {
  pub account_key: String,
  pub sync_url: Url,
  pub token: Zeroizing<String>,
}

pub(crate) struct Account {
  config: Option<Config>,
  client: Client,
  state: Mutex<State>,
}

impl Account {
  pub(crate) fn new() -> Result<Self, String> {
    let client = Client::builder()
      .https_only(true)
      .redirect(reqwest::redirect::Policy::none())
      .connect_timeout(Duration::from_secs(8))
      .timeout(Duration::from_secs(20))
      .build()
      .map_err(|_| "Could not initialize Apell Account networking.")?;
    Ok(Self {
      config: Config::compiled(),
      client,
      state: Mutex::default(),
    })
  }

  async fn status(&self, app: &AppHandle) -> Result<Status, String> {
    let mut state = self.state.lock().await;
    if let Some(config) = &self.config {
      state.load(app, config).await?;
    }
    Ok(state.status(self.config.is_some()))
  }

  pub async fn account_key(&self, app: &AppHandle) -> Result<Option<String>, String> {
    let Some(config) = &self.config else {
      return Ok(None);
    };
    let mut state = self.state.lock().await;
    state.load(app, config).await?;
    Ok(
      state
        .session
        .as_ref()
        .map(|session| session.stored.account_key()),
    )
  }

  pub async fn access(&self, app: &AppHandle) -> Result<Access, String> {
    let config = self.config.as_ref().ok_or(CLIENT_UNAVAILABLE)?;
    // One refresh at a time. Logto can rotate the refresh token on each exchange.
    let mut state = self.state.lock().await;
    state.load(app, config).await?;
    let session = state.session.as_mut().ok_or(SIGN_IN_REQUIRED)?;
    if session.pending_vault_write {
      persist(app, Some(&session.stored)).await?;
      session.pending_vault_write = false;
    }
    if let Some(access_token) = &session.access_token {
      if access_token.expires > Instant::now() + Duration::from_secs(30) {
        return Ok(access(config, session, access_token));
      }
    }
    let provider = oidc::discover(&self.client, config).await?;
    let tokens = match oidc::refresh(
      &self.client,
      config,
      &provider,
      &session.stored.refresh_token,
    )
    .await
    {
      Ok(tokens) => tokens,
      Err(TokenError::Expired) => {
        persist(app, None).await?;
        state.session = None;
        state.error = Some(TokenError::Expired.message());
        changed(app);
        return Err(TokenError::Expired.message());
      }
      Err(error) => return Err(error.message()),
    };
    let token = AccessToken::new(tokens.access_token, tokens.expires_in);
    if let Some(refresh_token) = tokens.refresh_token {
      session.stored.refresh_token.zeroize();
      session.stored.refresh_token = refresh_token;
      session.pending_vault_write = true;
      persist(app, Some(&session.stored)).await?;
      session.pending_vault_write = false;
    }
    let result = access(config, session, &token);
    session.access_token = Some(token);
    Ok(result)
  }

  async fn sign_in(&self, app: &AppHandle) -> Result<Status, String> {
    let config = self.config.as_ref().ok_or(CLIENT_UNAVAILABLE)?;
    let attempt = {
      let mut state = self.state.lock().await;
      state.load(app, config).await?;
      if state.attempt.is_some() || state.session.is_some() {
        return Ok(state.status(true));
      }
      let attempt = Attempt::new()?;
      state.error = None;
      state.attempt = Some(attempt.clone());
      attempt
    };
    changed(app);
    let provider = oidc::discover(&self.client, config).await;
    let mut state = self.state.lock().await;
    let Some(pending) = state
      .attempt
      .as_mut()
      .filter(|pending| pending.state == attempt.state)
    else {
      return Ok(state.status(true));
    };
    match provider {
      Ok(provider) => {
        let url = oidc::authorize(
          config,
          &provider,
          &attempt.state,
          &attempt.nonce,
          &attempt.verifier,
        );
        pending.provider = Some(provider);
        if app.opener().open_url(url.as_str(), None::<&str>).is_err() {
          state.attempt = None;
          state.error = Some("Could not open the browser for Apell Account sign-in.".into());
        } else {
          let app = app.clone();
          tauri::async_runtime::spawn(async move {
            tokio::time::sleep_until(attempt.expires.into()).await;
            app
              .state::<Host>()
              .account
              .expire(&app, &attempt.state)
              .await;
          });
        }
      }
      Err(error) => {
        state.attempt = None;
        state.error = Some(error);
      }
    }
    changed(app);
    Ok(state.status(true))
  }

  async fn expire(&self, app: &AppHandle, attempt_id: &str) {
    let mut state = self.state.lock().await;
    if state
      .attempt
      .as_ref()
      .is_some_and(|attempt| attempt.state == attempt_id)
    {
      state.attempt = None;
      state.error = Some(SIGN_IN_EXPIRED.into());
      changed(app);
    }
  }

  async fn cancel(&self, app: &AppHandle) -> Result<Status, String> {
    let mut state = self.state.lock().await;
    if state.attempt.take().is_some() {
      state.error = None;
      changed(app);
    }
    Ok(state.status(self.config.is_some()))
  }

  async fn sign_out(&self, app: &AppHandle) -> Result<Status, String> {
    let Some(config) = &self.config else {
      return self.status(app).await;
    };
    let mut state = self.state.lock().await;
    state.load(app, config).await?;
    if state.session.is_some() {
      persist(app, None).await?;
    }
    state.attempt = None;
    state.error = None;
    if let Some(session) = state.session.take() {
      let app = app.clone();
      tauri::async_runtime::spawn(async move {
        let host = app.state::<Host>();
        let account = &host.account;
        let Some(config) = &account.config else {
          return;
        };
        if let Ok(provider) = oidc::discover(&account.client, config).await {
          oidc::revoke(
            &account.client,
            config,
            &provider,
            &session.stored.refresh_token,
          )
          .await;
        }
      });
    }
    changed(app);
    Ok(state.status(true))
  }

  #[cfg(any(target_os = "ios", target_os = "android", target_os = "macos"))]
  async fn complete(&self, app: &AppHandle, url: Url) {
    let Some(config) = &self.config else { return };
    let Some(callback) = oidc::callback(&url) else {
      return;
    };
    if callback.issuer != config.issuer.as_str() {
      return;
    }
    let (attempt, provider, code) = {
      let mut state = self.state.lock().await;
      let Some(attempt) = state.attempt.as_mut() else {
        return;
      };
      if attempt.state != callback.state || attempt.completing || attempt.expires <= Instant::now()
      {
        return;
      }
      let Some(provider) = attempt.provider.clone() else {
        return;
      };
      let Some(code) = callback.code else {
        state.attempt = None;
        state.error = Some("Apell Account sign-in was declined. Try again.".into());
        changed(app);
        return;
      };
      attempt.completing = true;
      (attempt.clone(), provider, code)
    };
    let session = self.exchange(config, &provider, &attempt, &code).await;
    let mut state = self.state.lock().await;
    if !state
      .attempt
      .as_ref()
      .is_some_and(|current| current.state == attempt.state)
    {
      return;
    }
    state.attempt = None;
    state.error = match session {
      Ok(session) => match persist(app, Some(&session.stored)).await {
        Ok(()) => {
          state.session = Some(session);
          None
        }
        Err(error) => Some(error),
      },
      Err(error) => Some(error),
    };
    changed(app);
  }

  #[cfg(any(target_os = "ios", target_os = "android", target_os = "macos"))]
  async fn exchange(
    &self,
    config: &Config,
    provider: &Provider,
    attempt: &Attempt,
    code: &str,
  ) -> Result<Session, String> {
    let tokens = oidc::exchange(&self.client, config, provider, code, &attempt.verifier)
      .await
      .map_err(TokenError::message)?;
    let identity = oidc::verify(&self.client, config, provider, &tokens, &attempt.nonce).await?;
    let Tokens {
      refresh_token,
      access_token,
      expires_in,
      ..
    } = tokens;
    let refresh_token = refresh_token.ok_or("Apell Account did not grant a persistent session.")?;
    let email = identity.email.filter(|email| {
      identity.email_verified == Some(true)
        && email.len() <= 320
        && !email.chars().any(char::is_control)
    });
    Ok(Session {
      stored: StoredSession {
        issuer: config.issuer.to_string(),
        client_id: config.client_id.into(),
        resource: config.resource.into(),
        subject: identity.sub,
        email,
        refresh_token,
      },
      pending_vault_write: false,
      access_token: Some(AccessToken::new(access_token, expires_in)),
    })
  }
}

fn access(config: &Config, session: &Session, token: &AccessToken) -> Access {
  Access {
    account_key: session.stored.account_key(),
    sync_url: config.sync_url.clone(),
    token: token.value.clone(),
  }
}

async fn persist(app: &AppHandle, session: Option<&StoredSession>) -> Result<(), String> {
  let payload = session
    .map(serde_json::to_string)
    .transpose()
    .map_err(|_| "Could not save the Apell Account session.")?
    .map(Zeroizing::new);
  let app = app.clone();
  blocking(move || {
    tauri_plugin_apell_credentials::write(
      &app,
      SESSION_SLOT,
      payload.as_deref().map(String::as_str),
    )
  })
  .await
}

fn changed(app: &AppHandle) {
  let _ = app.emit_to("main", EVENT, ());
}

#[cfg(any(target_os = "ios", target_os = "android", target_os = "macos"))]
pub(crate) fn register_callbacks(app: &tauri::App) {
  use tauri_plugin_deep_link::DeepLinkExt;
  let handle = app.handle().clone();
  app.deep_link().on_open_url(move |event| {
    for url in event.urls() {
      let app = handle.clone();
      tauri::async_runtime::spawn(async move {
        app.state::<Host>().account.complete(&app, url).await;
      });
    }
  });
}

pub(crate) async fn dispatch(
  app: AppHandle,
  account: &Account,
  method: &str,
) -> Result<Value, String> {
  let status = match method {
    "account.status" => account.status(&app).await?,
    "account.sign_in" => account.sign_in(&app).await?,
    "account.cancel" => account.cancel(&app).await?,
    "account.sign_out" => account.sign_out(&app).await?,
    _ => return Err("Unknown Apell Account operation.".into()),
  };
  serde_json::to_value(status).map_err(|_| "Could not read Apell Account status.".into())
}
