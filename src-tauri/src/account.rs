mod oidc;

use crate::{commands::blocking, host::Host};
use oidc::{Config, DeviceCode, TokenError};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Url};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::{watch, Mutex};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const EVENT: &str = "apell-account-changed";
const SESSION_SLOT: &str = "apell-account-session-v1";
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
  device_code: Option<DeviceCode>,
}

struct Attempt {
  id: Uuid,
  _cancellation: watch::Sender<()>,
  presentation: Option<Presentation>,
}

struct Presentation {
  display: DeviceCode,
  browser_url: Url,
  deadline: tokio::time::Instant,
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
      device_code: self
        .attempt
        .as_ref()
        .and_then(|attempt| attempt.presentation.as_ref())
        .filter(|presentation| presentation.deadline > tokio::time::Instant::now())
        .map(|presentation| presentation.display.clone()),
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
    let mut state = self.state.lock().await;
    state.load(app, config).await?;
    if state.attempt.is_some() || state.session.is_some() {
      return Ok(state.status(true));
    }
    let id = Uuid::new_v4();
    let (cancellation, mut cancelled) = watch::channel(());
    state.error = None;
    state.attempt = Some(Attempt {
      id,
      _cancellation: cancellation,
      presentation: None,
    });
    changed(app);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
      let host = app.state::<Host>();
      let account = &host.account;
      let Some(config) = &account.config else {
        return;
      };
      let result = tokio::select! {
        _ = cancelled.changed() => return,
        result = account.sign_in_with_device_code(&app, config, id) => result,
      };
      account.finish_sign_in(&app, id, result).await;
    });
    Ok(state.status(true))
  }

  async fn open_sign_in(&self, app: &AppHandle) -> Result<(), String> {
    let state = self.state.lock().await;
    let presentation = state
      .attempt
      .as_ref()
      .and_then(|attempt| attempt.presentation.as_ref())
      .filter(|presentation| presentation.deadline > tokio::time::Instant::now())
      .ok_or("Start a new sign-in to get a device code.")?;
    app
      .opener()
      .open_url(presentation.browser_url.as_str(), None::<&str>)
      .map_err(|_| "Could not open the browser for Apell Account sign-in.".into())
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

  async fn sign_in_with_device_code(
    &self,
    app: &AppHandle,
    config: &Config,
    id: Uuid,
  ) -> Result<Session, String> {
    let provider = oidc::discover(&self.client, config).await?;
    let challenge = oidc::start_device_sign_in(&self.client, config, &provider).await?;
    {
      let mut state = self.state.lock().await;
      let attempt = state
        .attempt
        .as_mut()
        .filter(|attempt| attempt.id == id)
        .ok_or("Apell Account sign-in was cancelled.")?;
      attempt.presentation = Some(Presentation {
        display: challenge.display.clone(),
        browser_url: challenge.browser_url.clone(),
        deadline: challenge.deadline,
      });
      changed(app);
    }

    let tokens = oidc::await_device_approval(&self.client, config, &provider, &challenge).await?;
    let identity = oidc::verify(
      &self.client,
      config,
      &provider,
      &tokens,
      challenge.requested_at,
    )
    .await?;
    let refresh_token = tokens
      .refresh_token
      .ok_or("Apell Account did not grant a persistent session.")?;
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
      access_token: Some(AccessToken::new(tokens.access_token, tokens.expires_in)),
    })
  }

  async fn finish_sign_in(&self, app: &AppHandle, id: Uuid, result: Result<Session, String>) {
    let mut state = self.state.lock().await;
    if state
      .attempt
      .as_ref()
      .is_none_or(|attempt| attempt.id != id)
    {
      return;
    }
    state.attempt = None;
    state.error = match result {
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

pub(crate) async fn dispatch(
  app: AppHandle,
  account: &Account,
  method: &str,
) -> Result<Value, String> {
  if method == "account.open_sign_in" {
    account.open_sign_in(&app).await?;
    return Ok(Value::Null);
  }
  let status = match method {
    "account.status" => account.status(&app).await?,
    "account.sign_in" => account.sign_in(&app).await?,
    "account.cancel" => account.cancel(&app).await?,
    "account.sign_out" => account.sign_out(&app).await?,
    _ => return Err("Unknown Apell Account operation.".into()),
  };
  serde_json::to_value(status).map_err(|_| "Could not read Apell Account status.".into())
}
