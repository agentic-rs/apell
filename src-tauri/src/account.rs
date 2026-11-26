#[cfg(all(feature = "local-dev", not(all(target_os = "macos", debug_assertions))))]
compile_error!("The local-dev feature requires a macOS debug build.");

mod oidc;

use crate::{commands::blocking, host::Host};
use oidc::{Config, DeviceCode, TokenError};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
  future::Future,
  sync::{Arc, Weak},
  time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, Url};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::{watch, Mutex, MutexGuard};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const EVENT: &str = "apell-account-changed";
const SESSION_SLOT: &str = "apell-account-session-v1";
const CLIENT_UNAVAILABLE: &str = "Apell Account is unavailable in this build.";
const SIGN_IN_REQUIRED: &str = "Sign in to Apell Account to sync settings.";
const SESSION_CHANGED: &str = "The Apell Account session changed. Try again.";

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
  account_session_id: Option<Uuid>,
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
  #[serde(default)]
  key_exchange_resource: Option<String>,
  subject: String,
  email: Option<String>,
  refresh_token: String,
}

impl StoredSession {
  fn matches(&self, config: &Config) -> bool {
    self.issuer == config.issuer.as_str()
      && self.client_id == config.client_id
      && self.resource == config.resource
      && self.key_exchange_resource.as_deref() == config.key_exchange.as_ref().map(Url::as_str)
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
  id: Uuid,
  stored: StoredSession,
  refresh_gate: Arc<Mutex<()>>,
  pending_vault_write: bool,
  access_token: Option<AccessToken>,
  key_exchange_token: Option<AccessToken>,
}

impl Session {
  fn new(stored: StoredSession, access_token: Option<AccessToken>) -> Self {
    Self {
      id: Uuid::new_v4(),
      stored,
      refresh_gate: Arc::new(Mutex::new(())),
      pending_vault_write: false,
      access_token,
      key_exchange_token: None,
    }
  }

  fn token(&self, key_exchange: bool) -> Option<&AccessToken> {
    let token = if key_exchange {
      &self.key_exchange_token
    } else {
      &self.access_token
    };
    token
      .as_ref()
      .filter(|token| token.expires > Instant::now() + Duration::from_secs(30))
  }
}

enum RefreshEvent {
  Expired,
  StaleToken(Zeroizing<String>),
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
    let session = enabled.then_some(self.session.as_ref()).flatten();
    let state = if !enabled {
      SignInState::Unavailable
    } else if session.is_some() {
      SignInState::SignedIn
    } else if self.attempt.is_some() {
      SignInState::SigningIn
    } else {
      SignInState::SignedOut
    };
    Status {
      state,
      account_session_id: session.map(|session| session.id),
      email: session.and_then(|session| session.stored.email.clone()),
      error: self.error.clone(),
      device_code: self
        .attempt
        .as_ref()
        .and_then(|attempt| attempt.presentation.as_ref())
        .filter(|presentation| presentation.deadline > tokio::time::Instant::now())
        .map(|presentation| presentation.display.clone()),
    }
  }

  fn require_session(&self, id: Uuid) -> Result<&Session, String> {
    self
      .session
      .as_ref()
      .filter(|session| session.id == id)
      .ok_or_else(|| SESSION_CHANGED.into())
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
          self.session = Some(Session::new(stored, None));
        }
      }
    }
    self.loaded = true;
    Ok(())
  }
}

pub(crate) struct Access {
  pub account_key: String,
  pub session_id: Uuid,
  pub sync_url: Url,
  pub token: Zeroizing<String>,
  refresh_gate: Arc<Mutex<()>>,
}

impl Access {
  pub(crate) fn session_marker(&self) -> Weak<Mutex<()>> {
    Arc::downgrade(&self.refresh_gate)
  }
}

pub(crate) struct Account {
  config: Option<Config>,
  client: Client,
  state: Mutex<State>,
}

impl Account {
  pub(crate) fn new() -> Result<Self, String> {
    let config = Config::compiled();
    #[cfg(all(feature = "local-dev", target_os = "macos", debug_assertions))]
    let client = oidc::local_http_builder(config.as_ref())?
      .connect_timeout(Duration::from_secs(10))
      .read_timeout(Duration::from_secs(30))
      .timeout(Duration::from_secs(30))
      .build()
      .map_err(|_| "Could not initialize local Account networking.")?;
    #[cfg(not(all(feature = "local-dev", target_os = "macos", debug_assertions)))]
    let client = apell_core::providers::http_client()
      .map_err(|_| "Could not initialize Apell Account networking.")?;
    Ok(Self {
      config,
      client,
      state: Mutex::default(),
    })
  }

  pub(crate) fn http_client(&self) -> &Client {
    &self.client
  }

  #[cfg(all(feature = "local-dev", target_os = "macos", debug_assertions))]
  pub(crate) fn local_http_builder(&self) -> Result<reqwest::ClientBuilder, String> {
    oidc::local_http_builder(self.config.as_ref())
  }

  async fn status(&self, app: &AppHandle) -> Result<Status, String> {
    let mut state = self.state.lock().await;
    if let Some(config) = &self.config {
      state.load(app, config).await?;
    }
    Ok(state.status(self.config.is_some()))
  }

  pub async fn account_key(&self, app: &AppHandle) -> Result<Option<String>, String> {
    Ok(self.session(app, None).await?.map(|(key, _)| key))
  }

  pub(crate) async fn session(
    &self,
    app: &AppHandle,
    expected: Option<Uuid>,
  ) -> Result<Option<(String, Uuid)>, String> {
    let mut state = self.state.lock().await;
    if let Some(config) = &self.config {
      state.load(app, config).await?;
    }
    let session = match expected {
      Some(id) => Some(state.require_session(id)?),
      None => state.session.as_ref(),
    };
    Ok(session.map(|session| (session.stored.account_key(), session.id)))
  }

  pub(crate) async fn with_current_session<T>(
    &self,
    app: &AppHandle,
    expected: Uuid,
    effect: impl FnOnce() -> T,
  ) -> Result<T, String> {
    let mut state = self.state.lock().await;
    if let Some(config) = &self.config {
      state.load(app, config).await?;
    }
    state.require_session(expected)?;
    Ok(effect())
  }

  pub(crate) async fn require_current_access(&self, access: &Access) -> Result<(), String> {
    let state = self.state.lock().await;
    if state
      .session
      .as_ref()
      .is_some_and(|session| Arc::ptr_eq(&session.refresh_gate, &access.refresh_gate))
    {
      Ok(())
    } else {
      Err(SESSION_CHANGED.into())
    }
  }

  pub fn key_exchange_available(&self) -> bool {
    self
      .config
      .as_ref()
      .is_some_and(|config| config.key_exchange.is_some())
  }

  pub(crate) async fn start_background_access(
    &self,
    app: &AppHandle,
    key_exchange: bool,
    same_session_as: Option<&Access>,
    expected: Option<Uuid>,
  ) -> Result<tauri::async_runtime::JoinHandle<Result<Access, String>>, String> {
    let refresh_gate = {
      let state = self.state.lock().await;
      let session = match expected {
        Some(id) => state.require_session(id)?,
        None => state.session.as_ref().ok_or(SIGN_IN_REQUIRED)?,
      };
      if same_session_as
        .is_some_and(|access| !Arc::ptr_eq(&access.refresh_gate, &session.refresh_gate))
      {
        return Err(SESSION_CHANGED.into());
      }
      session.refresh_gate.clone()
    };
    let app = app.clone();
    // Sign-out can stop waiting, but an already-issued refresh must finish. It
    // can rotate a token even if saving sign-out later fails on this device.
    Ok(tauri::async_runtime::spawn(async move {
      app
        .state::<Host>()
        .account
        .access_for(&app, key_exchange, Some(&refresh_gate))
        .await
    }))
  }

  async fn access_for(
    &self,
    app: &AppHandle,
    key_exchange: bool,
    required_gate: Option<&Arc<Mutex<()>>>,
  ) -> Result<Access, String> {
    let config = self.config.as_ref().ok_or(CLIENT_UNAVAILABLE)?;
    let (resource, origin) = if key_exchange {
      let resource = config
        .key_exchange
        .as_ref()
        .ok_or("Encrypted API key sync is unavailable.")?;
      (
        resource.as_str(),
        config.issuer.join("/").map_err(|_| CLIENT_UNAVAILABLE)?,
      )
    } else {
      (config.resource, config.sync_url.clone())
    };
    let mut state = self.state.lock().await;
    state.load(app, config).await?;
    if let Some(gate) = required_gate {
      if state
        .session
        .as_ref()
        .is_none_or(|session| !Arc::ptr_eq(&session.refresh_gate, gate))
      {
        return Err(SESSION_CHANGED.into());
      }
    }
    self
      .refresh_access(
        state,
        origin,
        key_exchange,
        |refresh_token| async move {
          let provider = oidc::discover(&self.client, config)
            .await
            .map_err(TokenError::Failed)?;
          oidc::refresh(&self.client, config, &provider, &refresh_token, resource).await
        },
        |payload| persist_payload(app, payload),
        |event| match event {
          RefreshEvent::Expired => changed(app),
          RefreshEvent::StaleToken(token) => revoke_in_background(app, token),
        },
      )
      .await
  }

  async fn refresh_access<Exchange, Save>(
    &self,
    state: MutexGuard<'_, State>,
    origin: Url,
    key_exchange: bool,
    exchange: impl FnOnce(Zeroizing<String>) -> Exchange,
    save: impl Fn(Option<Zeroizing<String>>) -> Save,
    notify: impl FnOnce(RefreshEvent),
  ) -> Result<Access, String>
  where
    Exchange: Future<Output = Result<oidc::Tokens, TokenError>> + Send,
    Save: Future<Output = Result<(), String>> + Send,
  {
    let session = state.session.as_ref().ok_or(SIGN_IN_REQUIRED)?;
    if !session.pending_vault_write {
      if let Some(token) = session.token(key_exchange) {
        return Ok(access(origin, session, token));
      }
    }
    // Both resources share a refresh token. The gate also identifies the exact
    // session, including across sign-out and sign-in to the same account.
    let refresh_gate = session.refresh_gate.clone();
    drop(state);
    let _refresh = refresh_gate.lock().await;
    let refresh_token = {
      let mut state = self.state.lock().await;
      let session = state
        .session
        .as_mut()
        .filter(|session| Arc::ptr_eq(&session.refresh_gate, &refresh_gate))
        .ok_or(SESSION_CHANGED)?;
      if session.pending_vault_write {
        save(Some(encode_session(&session.stored)?)).await?;
        session.pending_vault_write = false;
      }
      if let Some(token) = session.token(key_exchange) {
        return Ok(access(origin, session, token));
      }
      Zeroizing::new(session.stored.refresh_token.clone())
    };
    let result = exchange(refresh_token.clone()).await;

    let mut state = self.state.lock().await;
    let Some(session) = state
      .session
      .as_mut()
      .filter(|session| Arc::ptr_eq(&session.refresh_gate, &refresh_gate))
    else {
      let current_token = state
        .session
        .as_ref()
        .map(|session| session.stored.refresh_token.as_str());
      if let Ok(mut tokens) = result {
        tokens.access_token.zeroize();
        tokens.id_token.zeroize();
        if let Some(token) = tokens.refresh_token.map(Zeroizing::new) {
          if token.as_str() != refresh_token.as_str() && Some(token.as_str()) != current_token {
            drop(state);
            notify(RefreshEvent::StaleToken(token));
          }
        }
      }
      return Err(SESSION_CHANGED.into());
    };
    let tokens = match result {
      Ok(tokens) => tokens,
      Err(TokenError::Expired) => {
        save(None).await?;
        state.session = None;
        state.error = Some(TokenError::Expired.message());
        drop(state);
        notify(RefreshEvent::Expired);
        return Err(TokenError::Expired.message());
      }
      Err(error) => return Err(error.message()),
    };
    let token = AccessToken::new(tokens.access_token, tokens.expires_in);
    if let Some(refresh_token) = tokens.refresh_token {
      session.stored.refresh_token.zeroize();
      session.stored.refresh_token = refresh_token;
      session.pending_vault_write = true;
      save(Some(encode_session(&session.stored)?)).await?;
      session.pending_vault_write = false;
    }
    let result = access(origin, session, &token);
    if key_exchange {
      session.key_exchange_token = Some(token);
    } else {
      session.access_token = Some(token);
    }
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

  pub(crate) async fn sign_out(&self, app: &AppHandle, expected: Uuid) -> Result<Value, String> {
    let mut state = self.state.lock().await;
    state.require_session(expected)?;
    persist(app, None).await?;
    state.attempt = None;
    state.error = None;
    if let Some(mut session) = state.session.take() {
      let refresh_token = Zeroizing::new(std::mem::take(&mut session.stored.refresh_token));
      revoke_in_background(app, refresh_token);
    }
    changed(app);
    output_status(state.status(true))
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
    Ok(Session::new(
      StoredSession {
        issuer: config.issuer.to_string(),
        client_id: config.client_id.into(),
        resource: config.resource.into(),
        key_exchange_resource: config.key_exchange.as_ref().map(Url::to_string),
        subject: identity.sub,
        email,
        refresh_token,
      },
      Some(AccessToken::new(tokens.access_token, tokens.expires_in)),
    ))
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
    drop(state);
    crate::key_sync::kick(app);
    crate::config_sync::kick(app);
  }
}

fn revoke_in_background(app: &AppHandle, token: Zeroizing<String>) {
  let app = app.clone();
  tauri::async_runtime::spawn(async move {
    let host = app.state::<Host>();
    let account = &host.account;
    let Some(config) = &account.config else {
      return;
    };
    if let Ok(provider) = oidc::discover(&account.client, config).await {
      oidc::revoke(&account.client, config, &provider, &token).await;
    }
  });
}

fn access(sync_url: Url, session: &Session, token: &AccessToken) -> Access {
  Access {
    account_key: session.stored.account_key(),
    session_id: session.id,
    sync_url,
    token: token.value.clone(),
    refresh_gate: session.refresh_gate.clone(),
  }
}

async fn persist(app: &AppHandle, session: Option<&StoredSession>) -> Result<(), String> {
  persist_payload(app, session.map(encode_session).transpose()?).await
}

fn encode_session(session: &StoredSession) -> Result<Zeroizing<String>, String> {
  serde_json::to_string(session)
    .map(Zeroizing::new)
    .map_err(|_| "Could not save the Apell Account session.".into())
}

async fn persist_payload(
  app: &AppHandle,
  payload: Option<Zeroizing<String>>,
) -> Result<(), String> {
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

pub(crate) fn take_session_id(params: &mut Value) -> Result<Uuid, String> {
  params
    .as_object_mut()
    .and_then(|params| params.remove("account_session_id"))
    .and_then(|value| value.as_str().and_then(|id| Uuid::parse_str(id).ok()))
    .ok_or_else(|| SESSION_CHANGED.into())
}

fn output_status(status: Status) -> Result<Value, String> {
  serde_json::to_value(status).map_err(|_| "Could not read Apell Account status.".into())
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
    _ => return Err("Unknown Apell Account operation.".into()),
  };
  output_status(status)
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;
  use tokio::sync::oneshot;

  fn session(token: &str) -> Session {
    Session::new(
      StoredSession {
        issuer: "https://issuer.example.invalid/oidc".into(),
        client_id: "fixture-client".into(),
        resource: "https://store.example.invalid".into(),
        key_exchange_resource: None,
        subject: "fixture-subject".into(),
        email: None,
        refresh_token: token.into(),
      },
      None,
    )
  }

  fn account() -> Arc<Account> {
    Arc::new(Account {
      config: None,
      client: Client::new(),
      state: Mutex::new(State {
        loaded: true,
        session: Some(session("initial-token")),
        ..State::default()
      }),
    })
  }

  fn origin() -> Url {
    Url::parse("https://store.example.invalid").unwrap()
  }

  fn tokens(rotated: Option<&str>) -> oidc::Tokens {
    serde_json::from_value(json!({
      "access_token": "fixture-access",
      "refresh_token": rotated,
      "expires_in": 100,
      "token_type": "Bearer",
    }))
    .unwrap()
  }

  fn assert_saved_rotation(payload: Option<Zeroizing<String>>) {
    let stored: StoredSession = serde_json::from_str(&payload.unwrap()).unwrap();
    assert_eq!(stored.refresh_token, "rotated-token");
  }

  async fn unexpected_save(_: Option<Zeroizing<String>>) -> Result<(), String> {
    panic!("Unexpected change to a saved session")
  }

  fn unexpected_event(_: RefreshEvent) {
    panic!("Unexpected Account event")
  }

  #[test]
  fn shell_identity_distinguishes_sign_ins_without_persisting_the_session_id() {
    let mut state = State::default();
    assert!(output_status(state.status(true)).unwrap()["account_session_id"].is_null());
    let first = session("first-token");
    let original_id = first.id;
    let original_account = first.stored.account_key();
    let stored = encode_session(&first.stored).unwrap();
    assert!(serde_json::from_str::<Value>(&stored)
      .unwrap()
      .get("account_session_id")
      .is_none());
    state.session = Some(first);
    let status = output_status(state.status(true)).unwrap();
    assert_eq!(status["account_session_id"], json!(original_id));
    assert!(status["email"].is_null());
    assert!(state.require_session(original_id).is_ok());

    let restored = Session::new(serde_json::from_str(&stored).unwrap(), None);
    assert_eq!(restored.stored.account_key(), original_account);
    assert_ne!(restored.id, original_id);
    state.session = Some(restored);
    assert!(state.require_session(original_id).is_err());
    assert!(output_status(state.status(false)).unwrap()["account_session_id"].is_null());

    let mut params = json!({"account_session_id": original_id, "enabled": true});
    assert_eq!(take_session_id(&mut params).unwrap(), original_id);
    assert_eq!(params, json!({"enabled": true}));
    assert_eq!(take_session_id(&mut params).unwrap_err(), SESSION_CHANGED);
  }

  #[tokio::test]
  async fn slow_refresh_leaves_account_state_readable_while_both_resources_serialize() {
    let account = account();
    let (started, ready) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let first_account = account.clone();
    let first = tokio::spawn(async move {
      first_account
        .refresh_access(
          first_account.state.lock().await,
          origin(),
          true,
          |token| async move {
            assert_eq!(token.as_str(), "initial-token");
            started.send(()).unwrap();
            released.await.unwrap();
            Ok(tokens(Some("rotated-token")))
          },
          |payload| async move {
            assert_saved_rotation(payload);
            Ok(())
          },
          unexpected_event,
        )
        .await
    });
    ready.await.unwrap();

    let second_account = account.clone();
    let (started_waiting, waiting) = oneshot::channel();
    let second = tokio::spawn(async move {
      let state = second_account.state.lock().await;
      started_waiting.send(()).unwrap();
      second_account
        .refresh_access(
          state,
          origin(),
          false,
          |token| async move {
            assert_eq!(token.as_str(), "rotated-token");
            Ok(tokens(None))
          },
          unexpected_save,
          unexpected_event,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(1), waiting)
      .await
      .expect("Local session reads must continue during the remote refresh")
      .unwrap();
    let state = tokio::time::timeout(Duration::from_secs(1), account.state.lock())
      .await
      .expect("Waiting for a token must release the session state");
    assert!(matches!(state.status(true).state, SignInState::SignedIn));
    assert!(!second.is_finished());
    drop(state);
    release.send(()).unwrap();
    let first = first.await.unwrap().unwrap();
    let second = second.await.unwrap().unwrap();
    assert_eq!(first.account_key, second.account_key);
    assert_eq!(first.session_id, second.session_id);
  }

  #[tokio::test]
  async fn late_refresh_expires_or_rotates_only_the_original_session() {
    for result in [Ok(tokens(Some("late-token"))), Err(TokenError::Expired)] {
      let expect_revocation = result.is_ok();
      let account = account();
      let (started, ready) = oneshot::channel();
      let (release, released) = oneshot::channel();
      let (revoked, revocation) = oneshot::channel();
      let refreshing = account.clone();
      let request = tokio::spawn(async move {
        refreshing
          .refresh_access(
            refreshing.state.lock().await,
            origin(),
            false,
            |_| async move {
              started.send(()).unwrap();
              released.await.unwrap();
              result
            },
            unexpected_save,
            move |event| match event {
              RefreshEvent::StaleToken(token) => revoked.send(token).unwrap(),
              RefreshEvent::Expired => panic!("Late expiration must not clear a new session"),
            },
          )
          .await
      });
      ready.await.unwrap();

      // The same subject signs in again during the network request.
      let mut state = account.state.lock().await;
      let replacement = session("replacement-token");
      assert_eq!(
        state.session.as_ref().unwrap().stored.account_key(),
        replacement.stored.account_key()
      );
      state.session = Some(replacement);
      drop(state);
      release.send(()).unwrap();
      assert_eq!(
        request.await.unwrap().err().as_deref(),
        Some(SESSION_CHANGED)
      );
      let state = account.state.lock().await;
      assert_eq!(
        state.session.as_ref().unwrap().stored.refresh_token,
        "replacement-token"
      );
      match (expect_revocation, revocation.await) {
        (true, Ok(token)) => assert_eq!(token.as_str(), "late-token"),
        (false, Err(_)) => {}
        _ => panic!("Only stale token rotations should request revocation"),
      }
    }
  }

  #[tokio::test]
  async fn a_failed_rotation_write_is_retried_before_another_exchange() {
    let account = account();
    let first = account
      .refresh_access(
        account.state.lock().await,
        origin(),
        true,
        |_| async { Ok(tokens(Some("rotated-token"))) },
        |_| async { Err("Fixture vault failure".into()) },
        unexpected_event,
      )
      .await;
    assert_eq!(first.err().as_deref(), Some("Fixture vault failure"));
    let saved = std::sync::atomic::AtomicBool::new(false);
    let saved = &saved;
    account
      .refresh_access(
        account.state.lock().await,
        origin(),
        false,
        |token| async move {
          assert!(saved.load(std::sync::atomic::Ordering::SeqCst));
          assert_eq!(token.as_str(), "rotated-token");
          Ok(tokens(None))
        },
        |payload| async move {
          assert_saved_rotation(payload);
          saved.store(true, std::sync::atomic::Ordering::SeqCst);
          Ok(())
        },
        unexpected_event,
      )
      .await
      .unwrap();
  }

  #[tokio::test]
  async fn queued_exchange_cannot_use_the_replacement_sessions_refresh_token() {
    let account = account();
    let gate = account
      .state
      .lock()
      .await
      .session
      .as_ref()
      .unwrap()
      .refresh_gate
      .clone();
    let held = gate.lock().await;
    let (started, ready) = oneshot::channel();
    let waiting = account.clone();
    let request = tokio::spawn(async move {
      let state = waiting.state.lock().await;
      started.send(()).unwrap();
      waiting
        .refresh_access(
          state,
          origin(),
          true,
          |_| async { panic!("An old request must never start a new session exchange") },
          unexpected_save,
          unexpected_event,
        )
        .await
    });
    ready.await.unwrap();
    account.state.lock().await.session = Some(session("replacement-token"));
    drop(held);
    assert_eq!(
      request.await.unwrap().err().as_deref(),
      Some(SESSION_CHANGED)
    );
  }
}
