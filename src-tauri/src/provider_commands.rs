use crate::{
  host::{string, Host},
  settings::{Connection, CredentialFormat, KeyAction, SaveConnection, Settings, DEMO_ID},
};
use apell_core::{
  llm::{validate_config, LlmConfig, LlmRequest, LlmTarget},
  providers::{self, AccountConfig, DeviceCodeHandle},
};
use serde_json::{json, Value};
use std::{
  collections::HashMap,
  future::Future,
  sync::Mutex,
  time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

const MAX_VAULT_BYTES: usize = 128 * 1024;

mod drafts;

struct Login {
  connection_id: String,
  provider_id: String,
  handle: DeviceCodeHandle,
  expires_at: Instant,
  polling: bool,
  cancel: tokio::sync::watch::Sender<bool>,
}

struct PendingLogin {
  connection_id: String,
  cancel: tokio::sync::watch::Sender<bool>,
}

struct LoginReservation<'a> {
  jobs: &'a ProviderJobs,
  login_id: String,
}

impl Drop for LoginReservation<'_> {
  fn drop(&mut self) {
    if let Ok(mut pending) = self.jobs.pending.lock() {
      pending.remove(&self.login_id);
    }
  }
}

#[derive(Default)]
pub struct ProviderJobs {
  drafts: Mutex<HashMap<String, drafts::Draft>>,
  // When both maps are needed, lock pending before logins. The reservation ID
  // is also the final login ID, so late completion/drop cannot affect a newer job.
  pending: Mutex<HashMap<String, PendingLogin>>,
  logins: Mutex<HashMap<String, Login>>,
}

impl ProviderJobs {
  fn reserve(
    &self,
    connection_id: &str,
  ) -> Result<(LoginReservation<'_>, tokio::sync::watch::Receiver<bool>), String> {
    let mut pending = self
      .pending
      .lock()
      .map_err(|_| "Sign-in state is unavailable.")?;
    let mut logins = self
      .logins
      .lock()
      .map_err(|_| "Sign-in state is unavailable.")?;
    pending.retain(|_, login| {
      if login.connection_id == connection_id {
        login.cancel.send_replace(true);
        false
      } else {
        true
      }
    });
    logins.retain(|_, login| {
      let keep = login.connection_id != connection_id && login.expires_at > Instant::now();
      if !keep {
        login.cancel.send_replace(true);
      }
      keep
    });
    if pending.len() + logins.len() >= 4 {
      return Err("Too many sign-in attempts are active. Cancel one first.".into());
    }
    let login_id = uuid::Uuid::new_v4().to_string();
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    pending.insert(
      login_id.clone(),
      PendingLogin {
        connection_id: connection_id.into(),
        cancel,
      },
    );
    Ok((
      LoginReservation {
        jobs: self,
        login_id,
      },
      receiver,
    ))
  }

  fn publish(
    &self,
    reservation: &LoginReservation<'_>,
    provider_id: &str,
    handle: DeviceCodeHandle,
  ) -> Result<(), String> {
    let mut pending = self
      .pending
      .lock()
      .map_err(|_| "Sign-in state is unavailable.")?;
    let mut logins = self
      .logins
      .lock()
      .map_err(|_| "Sign-in state is unavailable.")?;
    let pending = pending
      .remove(&reservation.login_id)
      .filter(|login| !*login.cancel.borrow())
      .ok_or("This sign-in attempt was cancelled or replaced.")?;
    logins.insert(
      reservation.login_id.clone(),
      Login {
        connection_id: pending.connection_id,
        provider_id: provider_id.into(),
        expires_at: Instant::now() + Duration::from_secs(handle.expires_in),
        handle,
        polling: false,
        cancel: pending.cancel,
      },
    );
    Ok(())
  }

  fn cancel_login(&self, id: &str) -> Result<(), String> {
    let mut pending = self
      .pending
      .lock()
      .map_err(|_| "Sign-in state is unavailable.")?;
    let mut logins = self
      .logins
      .lock()
      .map_err(|_| "Sign-in state is unavailable.")?;
    if let Some(login) = pending.remove(id) {
      login.cancel.send_replace(true);
    }
    if let Some(login) = logins.remove(id) {
      login.cancel.send_replace(true);
    }
    Ok(())
  }

  fn cancel_connection(&self, id: &str) {
    // Settings mutations hold the settings lock, as does initial reservation.
    // Invalidate requests still obtaining a device code as well as active polls.
    let Ok(mut pending) = self.pending.lock() else {
      return;
    };
    pending.retain(|_, login| {
      if login.connection_id == id {
        login.cancel.send_replace(true);
        false
      } else {
        true
      }
    });
    if let Ok(mut logins) = self.logins.lock() {
      logins.retain(|_, login| {
        if login.connection_id == id {
          login.cancel.send_replace(true);
          false
        } else {
          true
        }
      });
    }
  }
}

fn read_account(app: &AppHandle, connection: &Connection) -> Result<Option<AccountConfig>, String> {
  if connection.is_demo() {
    return Ok(None);
  }
  let saved = tauri_plugin_apell_credentials::read(app, &connection.credential.account)?;
  let Some(saved) = saved else {
    return Ok(None);
  };
  if saved.len() > MAX_VAULT_BYTES {
    return Err("The saved credential exceeds its size limit.".into());
  }
  let account = match connection.credential.format {
    CredentialFormat::ApiKey => providers::static_account(
      &connection.provider_id,
      &connection.connection_id,
      Some(&saved),
    )?,
    CredentialFormat::Account => serde_json::from_str::<AccountConfig>(&saved)
      .map_err(|_| "The saved credential is invalid. Enter it again.".to_owned())?,
  };
  let expected_provider = if connection.provider_id == "custom" {
    "openai"
  } else {
    &connection.provider_id
  };
  if account.id != connection.connection_id || account.provider != expected_provider {
    return Err("The saved credential belongs to another connection.".into());
  }
  Ok(Some(account))
}

fn encode_account(account: &AccountConfig) -> Result<String, String> {
  let value =
    serde_json::to_string(account).map_err(|_| "Could not encode provider credentials.")?;
  if value.len() > MAX_VAULT_BYTES {
    return Err("The provider credential exceeds its size limit.".into());
  }
  Ok(value)
}

fn credential_status(app: &AppHandle, connection: &Connection) -> &'static str {
  match read_account(app, connection) {
    Ok(Some(_)) => "present",
    Ok(None) => "absent",
    Err(_) => "unavailable",
  }
}

async fn blocking<T: Send + 'static>(
  operation: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
  tauri::async_runtime::spawn_blocking(operation)
    .await
    .map_err(|_| "The provider settings operation was interrupted.".to_owned())?
}

pub enum ModelRoute<'a> {
  Active,
  Saved(&'a str),
  Selected(&'a LlmTarget),
}

impl<'a> ModelRoute<'a> {
  pub fn applet(target: Option<&'a LlmTarget>) -> Self {
    target.map_or(Self::Active, Self::Selected)
  }

  pub fn resolve(&self, settings: &Settings) -> Result<Connection, String> {
    match self {
      Self::Active => Ok(settings.active().clone()),
      Self::Saved(id) => settings.connection(id).cloned(),
      Self::Selected(target) => {
        let mut selected = settings.connection(&target.connection_id)?.clone();
        selected.model_id.clone_from(&target.model_id);
        target.validate_for(&selected.config())?;
        Ok(selected)
      }
    }
  }
}

pub struct ModelSnapshot {
  pub config: LlmConfig,
  pub account: Result<Option<AccountConfig>, String>,
}

// Dropping the waiter's JoinHandle detaches the task. The same task owns the
// mutation gate across the provider exchange and the final blocking vault write.
async fn finish_snapshot<T: Send + 'static>(
  work: impl Future<Output = Result<T, String>> + Send + 'static,
) -> Result<T, String> {
  tauri::async_runtime::spawn(work)
    .await
    .map_err(|_| "The provider connection snapshot was interrupted.".to_owned())?
}

/// Resolve the destination and read its current credential under the same
/// mutation gate. An unavailable selection fails before the secure store is read.
pub async fn snapshot(
  app: &AppHandle,
  _host: &Host,
  route: ModelRoute<'_>,
) -> Result<ModelSnapshot, String> {
  enum Destination {
    Active,
    Saved(String),
    Selected(LlmTarget),
  }
  let destination = match route {
    ModelRoute::Active => Destination::Active,
    ModelRoute::Saved(id) => Destination::Saved(id.to_owned()),
    ModelRoute::Selected(target) => Destination::Selected(target.clone()),
  };
  let app = app.clone();
  finish_snapshot(async move {
    let host = app.state::<Host>();
    let _gate = host.credential_gate.lock().await;
    let route = match &destination {
      Destination::Active => ModelRoute::Active,
      Destination::Saved(id) => ModelRoute::Saved(id),
      Destination::Selected(target) => ModelRoute::Selected(target),
    };
    let connection = route.resolve(&*host.settings.lock().await)?;
    let account = current_account(&app, &host, &connection).await;
    Ok(ModelSnapshot {
      config: connection.config(),
      account,
    })
  })
  .await
}

async fn current_account(
  app: &AppHandle,
  host: &Host,
  connection: &Connection,
) -> Result<Option<AccountConfig>, String> {
  let app_read = app.clone();
  let read_connection = connection.clone();
  let mut account = blocking(move || read_account(&app_read, &read_connection)).await?;
  if let Some(current) = account.as_mut() {
    if providers::refresh_account(&host.provider_http, current).await? {
      let saved = encode_account(current)?;
      let app_write = app.clone();
      let slot = connection.credential.account.clone();
      blocking(move || tauri_plugin_apell_credentials::write(&app_write, &slot, Some(&saved)))
        .await?;
    }
  } else if !connection.is_demo() {
    account = Some(providers::static_account(
      &connection.provider_id,
      &connection.connection_id,
      None,
    )?);
  }
  Ok(account)
}

fn commit_settings(
  app: &AppHandle,
  root: &std::path::Path,
  settings: &Settings,
  updates: Vec<(String, Option<String>)>,
) -> Result<(), String> {
  commit_with_vault(
    |slot| tauri_plugin_apell_credentials::read(app, slot),
    |slot, value| tauri_plugin_apell_credentials::write(app, slot, value),
    || settings.persist(root),
    updates,
  )
}

fn commit_with_vault(
  read: impl Fn(&str) -> Result<Option<String>, String>,
  write: impl Fn(&str, Option<&str>) -> Result<(), String>,
  save: impl FnOnce() -> Result<(), String>,
  updates: Vec<(String, Option<String>)>,
) -> Result<(), String> {
  let mut previous = Vec::new();
  for (slot, _) in &updates {
    previous.push((slot.clone(), read(slot)?));
  }
  let mut changed = 0;
  let result = (|| {
    for (slot, value) in &updates {
      // A failed platform write can itself have partially completed.
      changed += 1;
      write(slot, value.as_deref())?;
    }
    save()
  })();
  if let Err(error) = result {
    let mut restored = true;
    for (slot, value) in previous.into_iter().take(changed).rev() {
      restored &= write(&slot, value.as_deref()).is_ok();
    }
    if !restored {
      return Err("Saving failed and credentials could not be fully restored. Re-enter the affected credentials.".into());
    }
    return Err(error);
  }
  Ok(())
}

pub async fn dispatch(
  app: AppHandle,
  host: &Host,
  method: &str,
  params: Value,
) -> Result<Value, String> {
  match method {
    "llm.settings" => {
      let saved = host.settings.lock().await.clone();
      blocking(move || Ok(json!({
        "active_connection_id": saved.active_connection_id,
        "connections": saved.connections.iter().map(|connection| connection.public(credential_status(&app, connection))).collect::<Vec<_>>(),
        "providers": providers::presets(),
      }))).await
    }
    "llm.create_draft" => {
      let input = serde_json::from_value(params).map_err(|_| "Invalid connection draft.")?;
      drafts::create(app, host, input).await
    }
    "llm.discard_draft" => {
      drafts::discard(&host.provider_jobs, string(&params, "draft_id")?).await?;
      Ok(Value::Null)
    }
    "llm.save_connection" => {
      let input = serde_json::from_value(params).map_err(|_| "Invalid connection settings.")?;
      drafts::save(app, host, input).await
    }
    "llm.discover_draft_models" => {
      let input = serde_json::from_value(params).map_err(|_| "Invalid model discovery request.")?;
      drafts::discover(app, host, input).await
    }
    "llm.activate_connection" => {
      let id = string(&params, "connection_id")?;
      let _gate = host.credential_gate.lock().await;
      let mut current = host.settings.lock().await;
      let connection = current.connection(id)?.clone();
      validate_config(&connection.config())?;
      let app_read = app.clone();
      let checked = connection.clone();
      blocking(move || {
        if !checked.is_demo()
          && !matches!(checked.provider_id.as_str(), "custom" | "llama-cpp")
          && read_account(&app_read, &checked)?.is_none()
        {
          return Err("Add credentials before using this connection.".into());
        }
        Ok(())
      })
      .await?;
      let mut next = current.clone();
      next.active_connection_id = id.to_owned();
      let root = host.root.clone();
      let persist = next.clone();
      blocking(move || persist.persist(&root)).await?;
      *current = next;
      Ok(Value::Null)
    }
    "llm.delete_connection" => {
      let id = string(&params, "connection_id")?;
      let _gate = host.credential_gate.lock().await;
      let mut current = host.settings.lock().await;
      let connection = current.connection(id)?.clone();
      if connection.is_demo() {
        return Err("The local demo connection cannot be removed.".into());
      }
      let mut next = current.clone();
      next
        .connections
        .retain(|connection| connection.connection_id != id);
      if next.active_connection_id == id {
        next.active_connection_id = DEMO_ID.into();
      }
      let persist = next.clone();
      let root = host.root.clone();
      blocking(move || {
        commit_settings(
          &app,
          &root,
          &persist,
          vec![(connection.credential.account, None)],
        )
      })
      .await?;
      *current = next;
      drafts::invalidate_source(&host.provider_jobs, id, None);
      Ok(Value::Null)
    }
    "llm.test_connection" => {
      let id = string(&params, "connection_id")?;
      let request_id = string(&params, "request_id")?.to_owned();
      let ModelSnapshot { config, account } = snapshot(&app, host, ModelRoute::Saved(id)).await?;
      let request = LlmRequest {
        request_id,
        messages: vec![json!({"role":"user","content":"Reply with OK."})],
        max_output_tokens: (config.provider_id != "codex").then_some(128),
        ..Default::default()
      };
      serde_json::to_value(
        host
          .llm
          .generate("shell-settings", request, config, account?, |_| {})
          .await,
      )
      .map_err(|_| "Could not encode the connection test result.".into())
    }
    "llm.cancel_test" => {
      host
        .llm
        .cancel("shell-settings", string(&params, "request_id")?)?;
      Ok(Value::Null)
    }
    "llm.begin_login" => drafts::begin_login(host, string(&params, "draft_id")?).await,
    "llm.poll_login" => drafts::poll_login(host, string(&params, "login_id")?).await,
    "llm.cancel_login" => {
      drafts::cancel_login(host, string(&params, "login_id")?).await?;
      Ok(Value::Null)
    }
    "llm.open_login" => {
      let uri = {
        let logins = host
          .provider_jobs
          .logins
          .lock()
          .map_err(|_| "Sign-in state is unavailable.")?;
        let login = logins
          .get(string(&params, "login_id")?)
          .filter(|login| login.expires_at > Instant::now())
          .ok_or("This sign-in attempt has expired. Start again.")?;
        login.handle.verification_uri.clone()
      };
      app
        .opener()
        .open_url(uri, None::<&str>)
        .map_err(|_| "Could not open the provider sign-in page.")?;
      Ok(Value::Null)
    }
    _ => Err("Unknown provider settings operation.".into()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::cell::{Cell, RefCell};

  #[tokio::test]
  async fn cancelling_snapshot_waiter_cannot_let_rotated_credentials_overwrite_a_newer_save() {
    use std::sync::Arc;
    use std::task::{Context, Waker};
    use tokio::sync::{oneshot, Mutex as AsyncMutex};

    let gate = Arc::new(AsyncMutex::new(()));
    let writes = Arc::new(Mutex::new(Vec::new()));
    let (writing, write_started) = oneshot::channel();
    let (release_write, released) = oneshot::channel();
    let snapshot_gate = gate.clone();
    let snapshot_writes = writes.clone();
    let waiter = tokio::spawn(finish_snapshot(async move {
      let _gate = snapshot_gate.lock().await;
      // Model a provider that has rotated the token, with its vault commit
      // blocked in the same kind of native worker used by current_account.
      blocking(move || {
        let _ = writing.send(());
        released
          .blocking_recv()
          .map_err(|_| "Fixture write cancelled.")?;
        snapshot_writes.lock().unwrap().push("rotated");
        Ok(())
      })
      .await
    }));
    write_started.await.unwrap();
    // Poll the second writer ourselves so no scheduling race can obscure
    // whether the mutation became runnable immediately after cancellation.
    let mut save = Box::pin(gate.lock());
    let mut context = Context::from_waker(Waker::noop());
    assert!(save.as_mut().poll(&mut context).is_pending());
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert!(
      save.as_mut().poll(&mut context).is_pending(),
      "the rotation must retain its gate"
    );
    assert!(writes.lock().unwrap().is_empty());
    release_write.send(()).unwrap();
    let _gate = save.await;
    writes.lock().unwrap().push("replacement");
    assert_eq!(*writes.lock().unwrap(), ["rotated", "replacement"]);
  }

  #[test]
  fn applet_route_overrides_only_the_selected_model_and_never_falls_back() {
    let mut settings = Settings::default();
    let saved = Connection {
      connection_id: "chosen-connection".into(),
      name: "Private connection alias".into(),
      provider_id: "custom".into(),
      base_url: "https://example.invalid/v1".into(),
      model_id: "saved-default".into(),
      credential: crate::settings::credential_ref("chosen-connection"),
    };
    settings.replace(None, saved.clone());
    let target = LlmTarget {
      connection_id: saved.connection_id.clone(),
      model_id: "chosen-model".into(),
    };
    let selected = ModelRoute::applet(Some(&target))
      .resolve(&settings)
      .unwrap();
    assert_eq!(selected.model_id, "chosen-model");
    assert_eq!(selected.credential, saved.credential);
    assert_eq!(settings.connection(&target.connection_id).unwrap(), &saved);
    assert!(ModelRoute::applet(None)
      .resolve(&settings)
      .unwrap()
      .is_demo());
    let public = crate::host::public_config(&selected.config());
    assert_eq!(public["provider_name"], "OpenAI compatible");
    assert!(!public.to_string().contains(&saved.name));
    assert!(!public.to_string().contains(&saved.base_url));
    assert!(ModelRoute::Selected(&LlmTarget {
      connection_id: DEMO_ID.into(),
      ..target.clone()
    })
    .resolve(&settings)
    .is_err());
    assert!(ModelRoute::Selected(&LlmTarget {
      model_id: " ".into(),
      ..target.clone()
    })
    .resolve(&settings)
    .is_err());
    settings
      .connections
      .retain(|entry| entry.connection_id != target.connection_id);
    assert!(ModelRoute::Selected(&target).resolve(&settings).is_err());
  }

  #[test]
  fn config_failure_restores_both_retired_and_replaced_credentials() {
    let vault = RefCell::new(HashMap::from([
      ("old-slot".to_owned(), "old-credential".to_owned()),
      ("new-slot".to_owned(), "previous-value".to_owned()),
    ]));
    let before = vault.borrow().clone();
    let result = commit_with_vault(
      |slot| Ok(vault.borrow().get(slot).cloned()),
      |slot, value| {
        match value {
          Some(value) => {
            vault.borrow_mut().insert(slot.to_owned(), value.to_owned());
          }
          None => {
            vault.borrow_mut().remove(slot);
          }
        }
        Ok(())
      },
      || Err("disk fixture failure".into()),
      vec![
        ("new-slot".into(), Some("new-credential".into())),
        ("old-slot".into(), None),
      ],
    );
    assert_eq!(result.unwrap_err(), "disk fixture failure");
    assert_eq!(*vault.borrow(), before);
  }

  #[test]
  fn partial_platform_write_is_rolled_back_before_any_config_save() {
    let vault = RefCell::new(HashMap::from([("slot".to_owned(), "old".to_owned())]));
    let calls = Cell::new(0);
    let saved = Cell::new(false);
    let result = commit_with_vault(
      |slot| Ok(vault.borrow().get(slot).cloned()),
      |slot, value| {
        calls.set(calls.get() + 1);
        vault
          .borrow_mut()
          .insert(slot.to_owned(), value.unwrap_or_default().to_owned());
        if calls.get() == 1 {
          Err("platform fixture failure".into())
        } else {
          Ok(())
        }
      },
      || {
        saved.set(true);
        Ok(())
      },
      vec![("slot".into(), Some("new".into()))],
    );
    assert!(result.is_err());
    assert_eq!(vault.borrow().get("slot").unwrap(), "old");
    assert!(!saved.get());
  }

  #[test]
  fn cancel_connection_invalidates_its_poll_and_leaves_other_logins() {
    let jobs = ProviderJobs::default();
    let mut receivers = Vec::new();
    for id in ["one", "two"] {
      let cancel = tokio::sync::watch::channel(false).0;
      receivers.push(cancel.subscribe());
      jobs.logins.lock().unwrap().insert(
        id.to_owned(),
        Login {
          connection_id: id.into(),
          provider_id: "codex".into(),
          polling: true,
          expires_at: Instant::now() + Duration::from_secs(30),
          cancel,
          handle: DeviceCodeHandle {
            device_code: "fixture-device-code".into(),
            user_code: "TEST-CODE".into(),
            verification_uri: "https://auth.openai.com/codex/device".into(),
            expires_in: 30,
            interval: 5,
          },
        },
      );
    }
    jobs.cancel_connection("one");
    assert!(*receivers[0].borrow());
    assert!(!*receivers[1].borrow());
    assert_eq!(jobs.logins.lock().unwrap().len(), 1);
  }

  fn login_handle() -> DeviceCodeHandle {
    DeviceCodeHandle {
      device_code: "fixture-device-code".into(),
      user_code: "TEST-CODE".into(),
      verification_uri: "https://auth.openai.com/codex/device".into(),
      expires_in: 30,
      interval: 5,
    }
  }

  #[tokio::test]
  async fn late_device_code_completion_cannot_replace_a_newer_poll() {
    let jobs = std::sync::Arc::new(ProviderJobs::default());
    let (started, ready) = tokio::sync::oneshot::channel();
    let (complete, completion) = tokio::sync::oneshot::channel();
    let older_jobs = jobs.clone();
    let older = tokio::spawn(async move {
      let (reservation, cancelled) = older_jobs.reserve("connection").unwrap();
      started.send(()).unwrap();
      // Model a device-code response that arrives after a newer attempt.
      completion.await.unwrap();
      assert!(*cancelled.borrow());
      assert!(older_jobs
        .publish(&reservation, "codex", login_handle())
        .is_err());
      // Dropping the old reservation must not release the newer attempt.
    });
    ready.await.unwrap();
    let (newer, _) = jobs.reserve("connection").unwrap();
    jobs.publish(&newer, "codex", login_handle()).unwrap();
    let cancelled = {
      let mut logins = jobs.logins.lock().unwrap();
      let login = logins.get_mut(&newer.login_id).unwrap();
      login.polling = true;
      login.cancel.subscribe()
    };
    complete.send(()).unwrap();
    older.await.unwrap();
    assert!(!*cancelled.borrow());
    assert!(jobs.pending.lock().unwrap().is_empty());
    assert!(
      jobs
        .logins
        .lock()
        .unwrap()
        .get(&newer.login_id)
        .unwrap()
        .polling
    );
  }

  #[test]
  fn settings_mutation_and_explicit_cancel_invalidate_pending_device_code_requests() {
    let jobs = ProviderJobs::default();
    let (removed, removed_cancel) = jobs.reserve("removed").unwrap();
    let (kept, kept_cancel) = jobs.reserve("kept").unwrap();
    jobs.cancel_connection("removed");
    assert!(*removed_cancel.borrow());
    assert!(!*kept_cancel.borrow());
    assert!(jobs.publish(&removed, "codex", login_handle()).is_err());
    jobs.cancel_login(&kept.login_id).unwrap();
    assert!(*kept_cancel.borrow());
    assert!(jobs.publish(&kept, "codex", login_handle()).is_err());
    assert!(jobs.pending.lock().unwrap().is_empty());
    assert!(jobs.logins.lock().unwrap().is_empty());
  }

  #[test]
  fn pending_requests_share_the_login_limit_and_drop_releases_capacity() {
    let jobs = ProviderJobs::default();
    let mut reservations = Vec::new();
    for index in 0..4 {
      reservations.push(jobs.reserve(&format!("connection_{index}")).unwrap().0);
    }
    jobs
      .publish(&reservations[0], "codex", login_handle())
      .unwrap();
    assert!(jobs.reserve("overflow").is_err());
    reservations.pop();
    let (replacement, _) = jobs.reserve("replacement").unwrap();
    assert_eq!(jobs.pending.lock().unwrap().len(), 3);
    drop(replacement);
    drop(reservations);
    assert!(jobs.pending.lock().unwrap().is_empty());
    assert_eq!(jobs.logins.lock().unwrap().len(), 1);
  }
}
