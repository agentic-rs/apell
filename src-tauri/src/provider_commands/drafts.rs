use super::{
  blocking, commit_settings, credential_status, encode_account, AccountConfig, AppHandle,
  Connection, Host, KeyAction, ProviderJobs, SaveConnection, Settings,
};
use apell_core::providers;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
  future::Future,
  sync::Arc,
  time::{Duration, Instant},
};
use tokio::sync::{watch, Mutex};

const MAX_DRAFTS: usize = 4;
const DRAFT_LIFETIME: Duration = Duration::from_secs(30 * 60);
const EXPIRED: &str =
  "This connection draft expired or was discarded. Reopen the editor to continue.";

#[derive(Clone)]
pub(super) struct Draft {
  connection: Connection,
  source: Option<Connection>,
  oauth: Option<AccountConfig>,
  oauth_login_id: Option<String>,
  expires_at: Instant,
  gate: Arc<Mutex<()>>,
  cancel: watch::Sender<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateDraft {
  connection_id: Option<String>,
  provider_id: String,
  base_url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DiscoverDraft {
  draft_id: String,
  key_action: KeyAction,
  api_key: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SaveDraft {
  draft_id: String,
  name: String,
  model_id: String,
  key_action: KeyAction,
  api_key: Option<String>,
}

impl Draft {
  fn inherited(&self) -> Option<&Connection> {
    self.source.as_ref().filter(|source| {
      source.connection_id == self.connection.connection_id
        && source.provider_id == self.connection.provider_id
        && source.base_url.trim().trim_end_matches('/') == self.connection.base_url
    })
  }

  fn check_source(&self, settings: &Settings) -> Result<(), String> {
    if let Some(source) = &self.source {
      if settings.connection(&source.connection_id)? != source {
        return Err(
          "This saved connection changed while you were editing. Reopen it before saving.".into(),
        );
      }
    }
    Ok(())
  }

  fn check_alive(&self) -> Result<(), String> {
    if self.expires_at <= Instant::now() || *self.cancel.borrow() {
      return Err(EXPIRED.into());
    }
    Ok(())
  }
}

fn get(jobs: &ProviderJobs, id: &str) -> Result<Draft, String> {
  let drafts = jobs
    .drafts
    .lock()
    .map_err(|_| "Connection drafts are unavailable.")?;
  let draft = drafts.get(id).ok_or(EXPIRED)?;
  draft.check_alive()?;
  Ok(draft.clone())
}

fn public(id: &str, status: &str) -> Value {
  json!({"draft_id":id,"credential_status":status})
}

fn prepare_draft(settings: &Settings, input: CreateDraft) -> Result<Draft, String> {
  let source = input
    .connection_id
    .as_deref()
    .map(|id| settings.connection(id).cloned())
    .transpose()?;
  let name = providers::presets()
    .into_iter()
    .find(|preset| preset.provider_id == input.provider_id)
    .ok_or("Choose a supported provider.")?
    .name;
  let connection = settings.prepare_save(&SaveConnection {
    connection_id: input.connection_id,
    name,
    provider_id: input.provider_id,
    base_url: input.base_url,
    model_id: "pending-model".into(),
    key_action: KeyAction::Remove,
  })?;
  Ok(Draft {
    connection,
    source,
    oauth: None,
    oauth_login_id: None,
    expires_at: Instant::now() + DRAFT_LIFETIME,
    gate: Arc::new(Mutex::new(())),
    cancel: watch::channel(false).0,
  })
}

pub(super) async fn create(
  app: AppHandle,
  host: &Host,
  input: CreateDraft,
) -> Result<Value, String> {
  let settings = host.settings.lock().await;
  let draft = prepare_draft(&settings, input)?;
  let inherited = draft.inherited().cloned();
  let expires_at = draft.expires_at;
  let id = uuid::Uuid::new_v4().to_string();
  {
    let mut drafts = host
      .provider_jobs
      .drafts
      .lock()
      .map_err(|_| "Connection drafts are unavailable.")?;
    drafts.retain(|id, previous| {
      let alive = previous.check_alive().is_ok();
      if !alive {
        previous.cancel.send_replace(true);
        host.provider_jobs.cancel_connection(id);
      }
      alive
    });
    if drafts.len() >= MAX_DRAFTS {
      return Err("Too many connection drafts are open. Close an editor and try again.".into());
    }
    drafts.insert(id.clone(), draft);
  }
  drop(settings);
  let jobs = Arc::downgrade(&host.provider_jobs);
  let expiring_id = id.clone();
  tauri::async_runtime::spawn(async move {
    tokio::time::sleep_until(tokio::time::Instant::from_std(expires_at)).await;
    if let Some(jobs) = jobs.upgrade() {
      let _ = discard(&jobs, &expiring_id).await;
    }
  });
  let status = if let Some(inherited) = inherited {
    blocking(move || Ok(credential_status(&app, &inherited))).await?
  } else {
    "absent"
  };
  // The editor may have closed while a secure-store status read was running.
  get(&host.provider_jobs, &id)?;
  Ok(public(&id, status))
}

pub(super) async fn discard(jobs: &ProviderJobs, id: &str) -> Result<(), String> {
  let gate = {
    let drafts = jobs
      .drafts
      .lock()
      .map_err(|_| "Connection drafts are unavailable.")?;
    let Some(draft) = drafts.get(id) else {
      return Ok(());
    };
    // Abort network operations immediately, then wait for any final Save that
    // already owns the draft. After acknowledgement no later auth can reappear.
    draft.cancel.send_replace(true);
    draft.gate.clone()
  };
  jobs.cancel_connection(id);
  let _gate = gate.lock().await;
  jobs
    .drafts
    .lock()
    .map_err(|_| "Connection drafts are unavailable.")?
    .remove(id);
  Ok(())
}

pub(super) fn invalidate_source(jobs: &ProviderJobs, source_id: &str, except: Option<&str>) {
  if let Ok(mut drafts) = jobs.drafts.lock() {
    drafts.retain(|id, draft| {
      let remove = Some(id.as_str()) != except
        && draft
          .source
          .as_ref()
          .is_some_and(|source| source.connection_id == source_id);
      if remove {
        draft.cancel.send_replace(true);
        jobs.cancel_connection(id);
      }
      !remove
    });
  }
}

fn check_key_input(action: &KeyAction, key: Option<&str>) -> Result<(), String> {
  if !matches!(action, KeyAction::Replace) && key.is_some() {
    return Err("Choose replace to use a new API key.".into());
  }
  Ok(())
}

fn replacement(draft: &Draft, key: Option<&str>) -> Result<AccountConfig, String> {
  let key = key
    .filter(|key| !key.is_empty())
    .ok_or("Enter the replacement API key.")?;
  providers::static_account(
    &draft.connection.provider_id,
    &draft.connection.connection_id,
    Some(key),
  )
}

async fn account_for_discovery(
  app: &AppHandle,
  host: &Host,
  id: &str,
  draft: &Draft,
  action: &KeyAction,
  key: Option<&str>,
) -> Result<Option<AccountConfig>, String> {
  check_key_input(action, key)?;
  match action {
    KeyAction::Replace => Ok(Some(replacement(draft, key)?)),
    KeyAction::Remove => Ok(Some(providers::static_account(
      &draft.connection.provider_id,
      &draft.connection.connection_id,
      None,
    )?)),
    KeyAction::Keep => {
      if let Some(mut oauth) = draft.oauth.clone() {
        if providers::refresh_account(&host.provider_http, &mut oauth).await? {
          encode_account(&oauth)?;
          let mut drafts = host
            .provider_jobs
            .drafts
            .lock()
            .map_err(|_| "Connection drafts are unavailable.")?;
          let current = drafts.get_mut(id).ok_or(EXPIRED)?;
          current.check_alive()?;
          current.oauth = Some(oauth.clone());
        }
        Ok(Some(oauth))
      } else if let Some(inherited) = draft.inherited() {
        // Reference the saved credential, including legacy slots. Reading and
        // refreshing it on demand prevents a draft from restoring stale tokens.
        let super::ModelSnapshot { config, account } = super::snapshot(
          app,
          host,
          super::ModelRoute::Saved(&inherited.connection_id),
        )
        .await?;
        if config.provider_id != draft.connection.provider_id
          || config.connection_id != draft.connection.connection_id
          || config.base_url.trim().trim_end_matches('/') != draft.connection.base_url
        {
          return Err("The saved connection changed. Reopen the editor.".into());
        }
        account
      } else {
        Ok(Some(providers::static_account(
          &draft.connection.provider_id,
          &draft.connection.connection_id,
          None,
        )?))
      }
    }
  }
}

pub(super) async fn discover(
  app: AppHandle,
  host: &Host,
  input: DiscoverDraft,
) -> Result<Value, String> {
  let draft = get(&host.provider_jobs, &input.draft_id)?;
  let mut cancellation = draft.cancel.subscribe();
  // Finish inherited-token maintenance before honoring discard: a refresh can
  // rotate a token remotely and must persist that rotation to the existing vault.
  // Discard marks cancellation immediately and waits for this bounded section.
  let gate = draft.gate.lock().await;
  let current = get(&host.provider_jobs, &input.draft_id)?;
  current.check_source(&*host.settings.lock().await)?;
  let account = account_for_discovery(
    &app,
    host,
    &input.draft_id,
    &current,
    &input.key_action,
    input.api_key.as_deref(),
  )
  .await?;
  get(&host.provider_jobs, &input.draft_id)?;
  drop(gate);
  let work = async {
    let models =
      providers::discover_models(&host.provider_http, &current.connection.config(), account)
        .await?;
    get(&host.provider_jobs, &input.draft_id)?;
    serde_json::to_value(models).map_err(|_| "Could not encode the model list.".to_owned())
  };
  tokio::select! {
    biased;
    _ = cancellation.changed() => Err(EXPIRED.into()),
    result = work => result,
  }
}

fn final_updates(
  draft: &Draft,
  connection: &mut Connection,
  action: &KeyAction,
  key: Option<&str>,
) -> Result<Vec<(String, Option<String>)>, String> {
  check_key_input(action, key)?;
  let mut updates = Vec::new();
  match action {
    KeyAction::Keep => {
      if let Some(account) = &draft.oauth {
        updates.push((
          connection.credential.account.clone(),
          Some(encode_account(account)?),
        ));
      } else if let Some(inherited) = draft.inherited() {
        connection.credential = inherited.credential.clone();
      } else {
        // A new required-auth destination cannot be saved without authentication.
        providers::static_account(&connection.provider_id, &connection.connection_id, None)?;
      }
    }
    KeyAction::Replace => {
      updates.push((
        connection.credential.account.clone(),
        Some(encode_account(&replacement(draft, key)?)?),
      ));
    }
    KeyAction::Remove => {
      if draft.inherited().is_none() {
        providers::static_account(&connection.provider_id, &connection.connection_id, None)?;
      }
    }
  }
  if let Some(source) = &draft.source {
    if matches!(action, KeyAction::Remove)
      || source.credential.account != connection.credential.account
    {
      updates.push((source.credential.account.clone(), None));
    }
  }
  Ok(updates)
}

pub(super) async fn save(app: AppHandle, host: &Host, input: SaveDraft) -> Result<Value, String> {
  let root = host.root.clone();
  let app_write = app.clone();
  let connection = save_with(host, input, move |settings, updates| {
    blocking(move || commit_settings(&app_write, &root, &settings, updates))
  })
  .await?;
  blocking(move || Ok(connection.public(credential_status(&app, &connection)))).await
}

async fn save_with<F: Future<Output = Result<(), String>>>(
  host: &Host,
  input: SaveDraft,
  commit: impl FnOnce(Settings, Vec<(String, Option<String>)>) -> F,
) -> Result<Connection, String> {
  let draft = get(&host.provider_jobs, &input.draft_id)?;
  let _draft_gate = draft.gate.lock().await;
  let _credential_gate = host.credential_gate.lock().await;
  let mut settings = host.settings.lock().await;
  let draft = get(&host.provider_jobs, &input.draft_id)?;
  draft.check_source(&settings)?;
  if input.model_id.trim().is_empty() {
    return Err("Choose a model or enter its ID before saving.".into());
  }
  let source_id = draft
    .source
    .as_ref()
    .map(|source| source.connection_id.as_str());
  let mut connection = settings.prepare_save(&SaveConnection {
    connection_id: source_id.map(str::to_owned),
    name: input.name,
    provider_id: draft.connection.provider_id.clone(),
    base_url: draft.connection.base_url.clone(),
    model_id: input.model_id,
    // Credential binding is validated against the immutable draft below.
    key_action: KeyAction::Remove,
  })?;
  connection.connection_id = draft.connection.connection_id.clone();
  connection.credential = crate::settings::credential_ref(&connection.connection_id);
  let updates = final_updates(
    &draft,
    &mut connection,
    &input.key_action,
    input.api_key.as_deref(),
  )?;
  let mut next = settings.clone();
  next.replace(source_id, connection.clone());
  commit(next.clone(), updates).await?;
  *settings = next;
  host
    .provider_jobs
    .drafts
    .lock()
    .map_err(|_| "Connection drafts are unavailable.")?
    .remove(&input.draft_id);
  draft.cancel.send_replace(true);
  host.provider_jobs.cancel_connection(&input.draft_id);
  if let Some(source) = &draft.source {
    invalidate_source(&host.provider_jobs, &source.connection_id, None);
  }
  Ok(connection)
}

pub(super) async fn begin_login(host: &Host, id: &str) -> Result<Value, String> {
  let (draft, reservation, mut cancellation) = {
    let drafts = host
      .provider_jobs
      .drafts
      .lock()
      .map_err(|_| "Connection drafts are unavailable.")?;
    let draft = drafts.get(id).ok_or(EXPIRED)?;
    draft.check_alive()?;
    if !draft.connection.is_oauth() {
      return Err("This provider does not use sign-in.".into());
    }
    let (reservation, cancellation) = host.provider_jobs.reserve(id)?;
    (draft.clone(), reservation, cancellation)
  };
  let handle = tokio::select! {
    biased;
    _ = cancellation.changed() => return Err("Sign-in was cancelled or replaced.".into()),
    result = providers::begin_login(&draft.connection.provider_id, &host.provider_http) => result?,
  };
  let result = json!({"login_id":reservation.login_id,"user_code":handle.user_code,
    "verification_uri":handle.verification_uri,"expires_in":handle.expires_in,"interval":handle.interval});
  let drafts = host
    .provider_jobs
    .drafts
    .lock()
    .map_err(|_| "Connection drafts are unavailable.")?;
  drafts.get(id).ok_or(EXPIRED)?.check_alive()?;
  host
    .provider_jobs
    .publish(&reservation, &draft.connection.provider_id, handle)?;
  Ok(result)
}

pub(super) async fn poll_login(host: &Host, login_id: &str) -> Result<Value, String> {
  let (draft_id, provider_id, handle, expires_at, mut cancellation) = {
    let mut logins = host
      .provider_jobs
      .logins
      .lock()
      .map_err(|_| "Sign-in state is unavailable.")?;
    let login = logins
      .get_mut(login_id)
      .ok_or("This sign-in attempt no longer exists.")?;
    if login.polling {
      return Err("This sign-in attempt is already waiting for authorization.".into());
    }
    if login.expires_at <= Instant::now() {
      return Err("This sign-in attempt has expired. Start again.".into());
    }
    login.polling = true;
    (
      login.connection_id.clone(),
      login.provider_id.clone(),
      login.handle.clone(),
      login.expires_at,
      login.cancel.subscribe(),
    )
  };
  let outcome = tokio::select! {
    biased;
    _ = cancellation.changed() => Err("Sign-in was cancelled.".to_owned()),
    result = tokio::time::timeout_at(tokio::time::Instant::from_std(expires_at), providers::poll_login(&provider_id, &host.provider_http, handle)) => {
      result.map_err(|_| "This sign-in attempt has expired. Start again.".to_owned()).and_then(|result| result)
    }
  };
  if outcome.is_err() {
    host
      .provider_jobs
      .logins
      .lock()
      .map_err(|_| "Sign-in state is unavailable.")?
      .remove(login_id);
  }
  let outcome = outcome?;
  let draft = get(&host.provider_jobs, &draft_id)?;
  let _gate = draft.gate.lock().await;
  let mut drafts = host
    .provider_jobs
    .drafts
    .lock()
    .map_err(|_| "Connection drafts are unavailable.")?;
  let current = drafts.get_mut(&draft_id).ok_or(EXPIRED)?;
  current.check_alive()?;
  let login = host
    .provider_jobs
    .logins
    .lock()
    .map_err(|_| "Sign-in state is unavailable.")?
    .remove(login_id)
    .filter(|login| !*login.cancel.borrow() && login.expires_at > Instant::now())
    .ok_or("This sign-in attempt was cancelled or expired.")?;
  if current.connection.provider_id != login.provider_id {
    return Err("The connection changed. Sign in again.".into());
  }
  let account =
    providers::device_account(&provider_id, &current.connection.connection_id, outcome)?;
  encode_account(&account)?;
  current.oauth = Some(account);
  current.oauth_login_id = Some(login_id.into());
  Ok(public(&draft_id, "present"))
}

pub(super) async fn cancel_login(host: &Host, login_id: &str) -> Result<(), String> {
  host.provider_jobs.cancel_login(login_id)?;
  let completed = host
    .provider_jobs
    .drafts
    .lock()
    .map_err(|_| "Connection drafts are unavailable.")?
    .iter()
    .find(|(_, draft)| draft.oauth_login_id.as_deref() == Some(login_id))
    .map(|(id, draft)| (id.clone(), draft.gate.clone()));
  if let Some((id, gate)) = completed {
    let _gate = gate.lock().await;
    let mut drafts = host
      .provider_jobs
      .drafts
      .lock()
      .map_err(|_| "Connection drafts are unavailable.")?;
    if let Some(draft) = drafts
      .get_mut(&id)
      .filter(|draft| draft.oauth_login_id.as_deref() == Some(login_id))
    {
      draft.oauth = None;
      draft.oauth_login_id = None;
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::settings::{credential_ref, CredentialFormat};
  use std::{cell::RefCell, collections::HashMap};

  fn fixture() -> (tempfile::TempDir, Arc<Host>) {
    let directory = tempfile::tempdir().unwrap();
    let host = Arc::new(Host::new(directory.path().to_owned()).unwrap());
    (directory, host)
  }

  fn create_input(provider: &str) -> CreateDraft {
    CreateDraft {
      connection_id: None,
      provider_id: provider.into(),
      base_url: providers::default_base_url(provider)
        .unwrap_or("https://example.invalid/v1")
        .into(),
    }
  }

  fn save_input(id: &str) -> SaveDraft {
    SaveDraft {
      draft_id: id.into(),
      name: "Fixture".into(),
      model_id: "fixture-model".into(),
      key_action: KeyAction::Keep,
      api_key: None,
    }
  }

  async fn install_draft(host: &Host, provider: &str) -> String {
    let draft = prepare_draft(&*host.settings.lock().await, create_input(provider)).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    host
      .provider_jobs
      .drafts
      .lock()
      .unwrap()
      .insert(id.clone(), draft);
    id
  }

  fn oauth(draft: &Draft) -> AccountConfig {
    serde_json::from_value(json!({
      "id":draft.connection.connection_id,"provider":draft.connection.provider_id,
      "access_token":"fixture-access","refresh_token":"fixture-refresh",
      "access_token_expires_at":4_000_000_000_i64,
    }))
    .unwrap()
  }

  #[tokio::test]
  async fn oauth_is_only_persisted_on_successful_save_and_failed_save_keeps_retryable_draft() {
    let (directory, host) = fixture();
    let id = install_draft(&host, "codex").await;
    {
      let mut drafts = host.provider_jobs.drafts.lock().unwrap();
      let draft = drafts.get_mut(&id).unwrap();
      draft.oauth = Some(oauth(draft));
      draft.oauth_login_id = Some("login-fixture".into());
    }
    let original = std::fs::read(directory.path().join("llm-connections.json")).unwrap();
    let vault = RefCell::new(HashMap::<String, String>::new());
    let failed = save_with(&host, save_input(&id), |_, updates| async {
      super::super::commit_with_vault(
        |slot| Ok(vault.borrow().get(slot).cloned()),
        |slot, value| {
          if let Some(value) = value {
            vault.borrow_mut().insert(slot.into(), value.into());
          } else {
            vault.borrow_mut().remove(slot);
          }
          Ok(())
        },
        || Err("fixture disk failure".into()),
        updates,
      )
    })
    .await;
    assert_eq!(failed.unwrap_err(), "fixture disk failure");
    assert!(vault.borrow().is_empty());
    assert!(get(&host.provider_jobs, &id).unwrap().oauth.is_some());
    assert_eq!(host.settings.lock().await.connections.len(), 1);
    assert_eq!(
      std::fs::read(directory.path().join("llm-connections.json")).unwrap(),
      original
    );

    let vault_ref = &vault;
    let directory_ref = &directory;
    let saved = save_with(&host, save_input(&id), |settings, updates| async move {
      super::super::commit_with_vault(
        |slot| Ok(vault_ref.borrow().get(slot).cloned()),
        |slot, value| {
          if let Some(value) = value {
            vault_ref.borrow_mut().insert(slot.into(), value.into());
          } else {
            vault_ref.borrow_mut().remove(slot);
          }
          Ok(())
        },
        || settings.persist(directory_ref.path()),
        updates,
      )
    })
    .await
    .unwrap();
    let stored: AccountConfig =
      serde_json::from_str(vault.borrow().get(&saved.credential.account).unwrap()).unwrap();
    assert_eq!(stored.id, saved.connection_id);
    assert!(get(&host.provider_jobs, &id).is_err());
    assert_eq!(
      Settings::load(directory.path()).unwrap().connections.len(),
      2
    );
  }

  #[tokio::test]
  async fn discovery_api_key_does_not_stage_a_replacement_for_later_keep_save() {
    let (_directory, host) = fixture();
    let id = install_draft(&host, "openai").await;
    let draft = get(&host.provider_jobs, &id).unwrap();
    let request_account = replacement(&draft, Some("request-only-key")).unwrap();
    assert_eq!(
      request_account.api_key.unwrap().expose(),
      "request-only-key"
    );
    assert!(get(&host.provider_jobs, &id).unwrap().oauth.is_none());
    let failed = save_with(&host, save_input(&id), |_, _| async {
      panic!("credential-free required provider must not commit")
    })
    .await;
    assert!(failed.is_err());
    assert!(get(&host.provider_jobs, &id).is_ok());
    let mut input = save_input(&id);
    input.key_action = KeyAction::Replace;
    input.api_key = Some("final-key".into());
    let saved = save_with(&host, input, |_, updates| async move {
      assert_eq!(updates.len(), 1);
      let account: AccountConfig = serde_json::from_str(updates[0].1.as_ref().unwrap()).unwrap();
      assert_eq!(account.api_key.unwrap().expose(), "final-key");
      Ok(())
    })
    .await
    .unwrap();
    assert_eq!(saved.provider_id, "openai");
  }

  #[tokio::test]
  async fn inherited_legacy_keep_preserves_reference_and_current_credential_without_rewriting() {
    let (_directory, host) = fixture();
    let mut source = prepare_draft(&Settings::default(), create_input("custom"))
      .unwrap()
      .connection;
    source.model_id = "saved-model".into();
    source.credential.account = "a".repeat(64);
    source.credential.format = CredentialFormat::ApiKey;
    host.settings.lock().await.replace(None, source.clone());
    let mut input = create_input("custom");
    input.connection_id = Some(source.connection_id.clone());
    let draft = prepare_draft(&*host.settings.lock().await, input).unwrap();
    let id = "legacy-draft";
    host
      .provider_jobs
      .drafts
      .lock()
      .unwrap()
      .insert(id.into(), draft);
    let saved = save_with(&host, save_input(id), |_, updates| async move {
      assert!(
        updates.is_empty(),
        "Keep must not read or overwrite a newer vault credential"
      );
      Ok(())
    })
    .await
    .unwrap();
    assert_eq!(saved.credential, source.credential);
    assert_eq!(saved.connection_id, source.connection_id);
  }

  #[tokio::test]
  async fn destination_changes_cannot_inherit_credentials_and_explicit_edit_removal_still_works() {
    let (_directory, host) = fixture();
    let source = prepare_draft(&Settings::default(), create_input("openai"))
      .unwrap()
      .connection;
    host.settings.lock().await.replace(None, source.clone());
    let mut input = create_input("openai");
    input.connection_id = Some(source.connection_id.clone());
    input.base_url = "https://another.invalid/v1".into();
    let changed = prepare_draft(&*host.settings.lock().await, input).unwrap();
    assert!(changed.inherited().is_none());
    assert_ne!(changed.connection.connection_id, source.connection_id);
    let mut final_connection = changed.connection.clone();
    assert!(final_updates(&changed, &mut final_connection, &KeyAction::Keep, None).is_err());

    let mut input = create_input("openai");
    input.connection_id = Some(source.connection_id.clone());
    let same = prepare_draft(&*host.settings.lock().await, input).unwrap();
    let mut final_connection = same.connection.clone();
    assert_eq!(
      final_updates(&same, &mut final_connection, &KeyAction::Remove, None).unwrap(),
      vec![(source.credential.account, None)]
    );
  }

  #[tokio::test]
  async fn stale_source_and_empty_model_never_reach_the_commit() {
    let (_directory, host) = fixture();
    let id = install_draft(&host, "custom").await;
    let mut input = save_input(&id);
    input.model_id = " ".into();
    assert!(save_with(&host, input, |_, _| async {
      panic!("empty model must not commit")
    })
    .await
    .is_err());
    let source = prepare_draft(&Settings::default(), create_input("custom"))
      .unwrap()
      .connection;
    host.settings.lock().await.replace(None, source.clone());
    let mut input = create_input("custom");
    input.connection_id = Some(source.connection_id.clone());
    let draft = prepare_draft(&*host.settings.lock().await, input).unwrap();
    host
      .provider_jobs
      .drafts
      .lock()
      .unwrap()
      .insert("stale".into(), draft);
    host
      .settings
      .lock()
      .await
      .connections
      .iter_mut()
      .find(|connection| connection.connection_id == source.connection_id)
      .unwrap()
      .name = "Changed elsewhere".into();
    assert!(save_with(&host, save_input("stale"), |_, _| async {
      panic!("stale editor must not commit")
    })
    .await
    .is_err());
  }

  #[tokio::test]
  async fn discard_cancels_pending_auth_and_discovery_and_drops_completed_credentials() {
    let (_directory, host) = fixture();
    let id = install_draft(&host, "codex").await;
    {
      let mut drafts = host.provider_jobs.drafts.lock().unwrap();
      let draft = drafts.get_mut(&id).unwrap();
      draft.oauth = Some(oauth(draft));
    }
    let draft = get(&host.provider_jobs, &id).unwrap();
    let mut discovery = draft.cancel.subscribe();
    let (reservation, auth) = host.provider_jobs.reserve(&id).unwrap();
    discard(&host.provider_jobs, &id).await.unwrap();
    discovery.changed().await.unwrap();
    assert!(*discovery.borrow());
    assert!(*auth.borrow());
    assert!(get(&host.provider_jobs, &id).is_err());
    assert!(host
      .provider_jobs
      .pending
      .lock()
      .unwrap()
      .get(&reservation.login_id)
      .is_none());
    assert_eq!(host.settings.lock().await.connections.len(), 1);
  }

  #[tokio::test]
  async fn discard_waits_for_a_save_that_already_started_and_never_resurrects_a_draft() {
    let (_directory, host) = fixture();
    let id = install_draft(&host, "custom").await;
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let saving_host = host.clone();
    let saving_id = id.clone();
    let save = tokio::spawn(async move {
      save_with(&saving_host, save_input(&saving_id), |_, _| async move {
        started.send(()).unwrap();
        released.await.unwrap();
        Ok(())
      })
      .await
    });
    ready.await.unwrap();
    let discarding_host = host.clone();
    let discarding_id = id.clone();
    let discard =
      tokio::spawn(async move { discard(&discarding_host.provider_jobs, &discarding_id).await });
    tokio::task::yield_now().await;
    assert!(!discard.is_finished());
    release.send(()).unwrap();
    let saved = save.await.unwrap().unwrap();
    discard.await.unwrap().unwrap();
    assert!(get(&host.provider_jobs, &id).is_err());
    assert_eq!(
      host
        .settings
        .lock()
        .await
        .connection(&saved.connection_id)
        .unwrap()
        .model_id,
      "fixture-model"
    );
  }

  #[tokio::test]
  async fn expired_drafts_are_unusable_and_cancel_targets_only_the_completed_login() {
    let (_directory, host) = fixture();
    let id = install_draft(&host, "codex").await;
    {
      let mut drafts = host.provider_jobs.drafts.lock().unwrap();
      let draft = drafts.get_mut(&id).unwrap();
      draft.oauth = Some(oauth(draft));
      draft.oauth_login_id = Some("latest-login".into());
    }
    cancel_login(&host, "older-login").await.unwrap();
    assert!(get(&host.provider_jobs, &id).unwrap().oauth.is_some());
    cancel_login(&host, "latest-login").await.unwrap();
    assert!(get(&host.provider_jobs, &id).unwrap().oauth.is_none());
    host
      .provider_jobs
      .drafts
      .lock()
      .unwrap()
      .get_mut(&id)
      .unwrap()
      .expires_at = Instant::now() - Duration::from_secs(1);
    assert!(get(&host.provider_jobs, &id)
      .err()
      .unwrap()
      .contains("expired"));
    discard(&host.provider_jobs, &id).await.unwrap();
    assert!(host.provider_jobs.drafts.lock().unwrap().is_empty());
    assert_eq!(credential_ref("fixture").account.len(), 64);
  }
}
