use apell_core::{
  bundles::{atomic_write, digest},
  llm::{validate_config, LlmConfig},
  providers,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::Path};
use uuid::Uuid;

pub const DEMO_ID: &str = "local-demo";
pub(crate) const MAX_CONNECTIONS: usize = 32;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialFormat {
  ApiKey,
  Account,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CredentialRef {
  pub account: String,
  pub format: CredentialFormat,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Connection {
  pub connection_id: String,
  pub name: String,
  pub provider_id: String,
  pub base_url: String,
  pub model_id: String,
  pub credential: CredentialRef,
}

impl Connection {
  pub fn config(&self) -> LlmConfig {
    LlmConfig {
      connection_id: self.connection_id.clone(),
      provider_id: self.provider_id.clone(),
      base_url: self.base_url.clone(),
      model_id: self.model_id.clone(),
    }
  }

  pub fn is_demo(&self) -> bool {
    self.provider_id == "demo"
  }

  pub fn is_oauth(&self) -> bool {
    matches!(self.provider_id.as_str(), "codex" | "github-copilot")
  }

  pub fn public(&self, credential_status: &str) -> serde_json::Value {
    serde_json::json!({
      "connection_id": self.connection_id,
      "name": self.name,
      "provider_id": self.provider_id,
      "base_url": self.base_url,
      "model_id": self.model_id,
      "credential_status": credential_status,
    })
  }

  fn demo() -> Self {
    Self {
      connection_id: DEMO_ID.into(),
      name: "Local demo".into(),
      provider_id: "demo".into(),
      base_url: "demo".into(),
      model_id: DEMO_ID.into(),
      credential: credential_ref(DEMO_ID),
    }
  }
}

pub fn credential_ref(connection_id: &str) -> CredentialRef {
  CredentialRef {
    account: digest(format!("apell-connection:{connection_id}").as_bytes()),
    format: CredentialFormat::Account,
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
  pub schema_version: u32,
  pub active_connection_id: String,
  pub connections: Vec<Connection>,
}

impl Default for Settings {
  fn default() -> Self {
    Self {
      schema_version: 2,
      active_connection_id: DEMO_ID.into(),
      connections: vec![Connection::demo()],
    }
  }
}

impl Settings {
  pub fn load(root: &Path) -> Result<Self, String> {
    let path = root.join("llm-connections.json");
    if path.exists() {
      let bytes = std::fs::read(&path).map_err(|_| "Could not read model connections.")?;
      if bytes.len() > 128 * 1024 {
        return Err("Saved model connections exceed their storage limit.".into());
      }
      let saved: Self = serde_json::from_slice(&bytes)
        .map_err(|_| "Saved model connections are invalid. They were preserved.")?;
      saved.validate_storage()?;
      return Ok(saved);
    }
    let mut settings = Self::default();
    let legacy_path = root.join("llm.json");
    if legacy_path.exists() {
      let bytes =
        std::fs::read(&legacy_path).map_err(|_| "Could not read previous model settings.")?;
      let legacy: LlmConfig = serde_json::from_slice(&bytes)
        .map_err(|_| "Previous model settings are invalid. They were preserved.")?;
      if legacy.base_url != "demo" {
        settings.active_connection_id = legacy.connection_id.clone();
        settings.connections.push(Connection {
          connection_id: legacy.connection_id,
          name: "Saved connection".into(),
          provider_id: "custom".into(),
          credential: CredentialRef {
            account: digest(legacy.base_url.as_bytes()),
            format: CredentialFormat::ApiKey,
          },
          base_url: legacy.base_url,
          model_id: legacy.model_id,
        });
      }
    }
    settings.persist(root)?;
    Ok(settings)
  }

  fn validate_storage(&self) -> Result<(), String> {
    let mut ids = HashSet::new();
    let mut slots = HashSet::new();
    if self.schema_version != 2
      || self.connections.len() > MAX_CONNECTIONS
      || self.connections.iter().any(|connection| {
        connection.connection_id.is_empty()
          || connection.connection_id.len() > 128
          || !connection
            .connection_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
          || !ids.insert(&connection.connection_id)
          || !slots.insert(&connection.credential.account)
          || connection.credential.account.len() != 64
          || !connection
            .credential
            .account
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
      })
      || self.connection(DEMO_ID).is_err()
      || self.connection(&self.active_connection_id).is_err()
    {
      return Err("Saved model connections are invalid. They were preserved.".into());
    }
    Ok(())
  }

  pub fn persist(&self, root: &Path) -> Result<(), String> {
    self.validate_storage()?;
    let bytes = serde_json::to_vec(self).map_err(|_| "Could not encode model connections.")?;
    publish_settings(&root.join("llm-connections.json"), &bytes, atomic_write)
  }

  pub fn connection(&self, id: &str) -> Result<&Connection, String> {
    self
      .connections
      .iter()
      .find(|connection| connection.connection_id == id)
      .ok_or_else(|| "This model connection no longer exists.".into())
  }

  pub fn active(&self) -> &Connection {
    self
      .connection(&self.active_connection_id)
      .expect("validated active connection")
  }

  pub fn prepare_save(&self, input: &SaveConnection) -> Result<Connection, String> {
    let previous = input
      .connection_id
      .as_deref()
      .map(|id| self.connection(id))
      .transpose()?;
    if previous.is_some_and(Connection::is_demo) || input.provider_id == "demo" {
      return Err("The local demo connection cannot be edited.".into());
    }
    if previous.is_none() && self.connections.len() >= MAX_CONNECTIONS {
      return Err("You can save up to 31 provider connections.".into());
    }
    let name = input.name.trim();
    if name.is_empty() || name.len() > 80 || name.chars().any(char::is_control) {
      return Err("Enter a connection name of 1–80 bytes.".into());
    }
    let preset = providers::presets()
      .into_iter()
      .find(|p| p.provider_id == input.provider_id)
      .ok_or("Choose a supported provider.")?;
    let base_url = input.base_url.trim().trim_end_matches('/').to_owned();
    if matches!(input.provider_id.as_str(), "codex" | "github-copilot")
      && base_url != preset.base_url.trim_end_matches('/')
    {
      return Err("Sign-in providers use their official endpoint.".into());
    }
    let same_destination = previous.is_some_and(|old| {
      old.provider_id == input.provider_id && old.base_url.trim().trim_end_matches('/') == base_url
    });
    if previous.is_some() && !same_destination && matches!(input.key_action, KeyAction::Keep) {
      return Err(
        "Changing the provider or endpoint requires a new credential or explicit removal.".into(),
      );
    }
    let connection_id = if same_destination {
      previous.unwrap().connection_id.clone()
    } else {
      Uuid::new_v4().to_string()
    };
    let connection = Connection {
      credential: credential_ref(&connection_id),
      connection_id,
      name: name.into(),
      provider_id: input.provider_id.clone(),
      base_url,
      model_id: input.model_id.trim().into(),
    };
    if previous.is_some_and(|old| old.connection_id == self.active_connection_id)
      && connection.model_id.is_empty()
    {
      return Err("Choose a model before changing the active connection.".into());
    }
    let mut validation = connection.config();
    if validation.model_id.is_empty() {
      validation.model_id = "pending-model".into();
    }
    validate_config(&validation)?;
    Ok(connection)
  }

  pub fn replace(&mut self, old_id: Option<&str>, connection: Connection) {
    if let Some(index) = old_id.and_then(|id| {
      self
        .connections
        .iter()
        .position(|entry| entry.connection_id == id)
    }) {
      if self.active_connection_id == self.connections[index].connection_id {
        self.active_connection_id = connection.connection_id.clone();
      }
      self.connections[index] = connection;
    } else {
      self.connections.push(connection);
    }
  }
}

fn publish_settings(
  path: &Path,
  bytes: &[u8],
  write: impl FnOnce(&Path, &[u8]) -> Result<(), String>,
) -> Result<(), String> {
  match write(path, bytes) {
    Ok(()) => Ok(()),
    Err(error) => {
      // A directory sync can fail after rename has published the new settings.
      // Crash durability is uncertain, but the visible commit already happened:
      // keep its matching credentials and memory instead of rolling them back.
      // Only exact on-disk bytes establish that this settings write is published.
      if std::fs::read(path).is_ok_and(|published| published == bytes) {
        Ok(())
      } else {
        Err(error)
      }
    }
  }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyAction {
  Keep,
  Replace,
  Remove,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveConnection {
  pub connection_id: Option<String>,
  pub name: String,
  pub provider_id: String,
  pub base_url: String,
  pub model_id: String,
  pub key_action: KeyAction,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn settings_failure_before_publication_preserves_the_error_and_previous_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("llm-connections.json");
    let previous = serde_json::to_vec(&Settings::default()).unwrap();
    std::fs::write(&path, &previous).unwrap();
    let mut next = Settings::default();
    let connection = next.prepare_save(&input()).unwrap();
    next.active_connection_id = connection.connection_id.clone();
    next.replace(None, connection);
    let bytes = serde_json::to_vec(&next).unwrap();

    let result = publish_settings(&path, &bytes, |_, _| Err("rename failed".into()));
    assert_eq!(result, Err("rename failed".into()));
    assert_eq!(std::fs::read(&path).unwrap(), previous);

    std::fs::remove_file(&path).unwrap();
    let result = publish_settings(&path, &bytes, |_, _| Err("write failed".into()));
    assert_eq!(result, Err("write failed".into()));
    assert!(!path.exists());
  }

  #[test]
  fn settings_failure_after_publication_keeps_the_committed_credential_reference() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("llm-connections.json");
    Settings::default().persist(directory.path()).unwrap();
    let mut next = Settings::default();
    let connection = next.prepare_save(&input()).unwrap();
    let expected_slot = connection.credential.account.clone();
    next.active_connection_id = connection.connection_id.clone();
    next.replace(None, connection);
    let bytes = serde_json::to_vec(&next).unwrap();

    let result = publish_settings(&path, &bytes, |destination, data| {
      let temporary = destination.with_extension("pending");
      std::fs::write(&temporary, data).unwrap();
      std::fs::rename(&temporary, destination).unwrap();
      Err("directory sync failed".into())
    });
    assert!(result.is_ok());
    let published = Settings::load(directory.path()).unwrap();
    assert_eq!(published.active_connection_id, next.active_connection_id);
    assert_eq!(published.active().credential.account, expected_slot);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
  }

  fn input() -> SaveConnection {
    SaveConnection {
      connection_id: None,
      name: "Test connection".into(),
      provider_id: "custom".into(),
      base_url: "https://example.invalid/v1".into(),
      model_id: "test-model".into(),
      key_action: KeyAction::Keep,
    }
  }

  #[test]
  fn same_endpoint_accounts_have_separate_host_owned_identities_and_vault_slots() {
    let settings = Settings::default();
    let first = settings.prepare_save(&input()).unwrap();
    let second = settings.prepare_save(&input()).unwrap();
    assert_ne!(first.connection_id, second.connection_id);
    assert_ne!(first.credential.account, second.credential.account);
    assert!(Uuid::parse_str(&first.connection_id).is_ok());
    let mut untrusted = input();
    untrusted.connection_id = Some("caller-chosen-id".into());
    assert!(settings.prepare_save(&untrusted).is_err());
  }

  #[test]
  fn destination_change_requires_explicit_credentials_and_rotates_identity() {
    let mut settings = Settings::default();
    let first = settings.prepare_save(&input()).unwrap();
    settings.replace(None, first.clone());
    settings.active_connection_id = first.connection_id.clone();
    let mut change = input();
    change.connection_id = Some(first.connection_id.clone());
    change.name = "Renamed".into();
    change.model_id = "another-model".into();
    assert_eq!(
      settings.prepare_save(&change).unwrap().connection_id,
      first.connection_id
    );
    change.base_url = "https://another.invalid/v1".into();
    assert!(settings.prepare_save(&change).is_err());
    change.key_action = KeyAction::Remove;
    let next = settings.prepare_save(&change).unwrap();
    assert_ne!(next.connection_id, first.connection_id);
    settings.replace(Some(&first.connection_id), next.clone());
    assert_eq!(settings.active_connection_id, next.connection_id);
  }

  #[test]
  fn draft_models_and_oauth_destinations_are_validated_separately() {
    let mut settings = Settings::default();
    let mut draft = input();
    draft.model_id.clear();
    let connection = settings.prepare_save(&draft).unwrap();
    settings.replace(None, connection.clone());
    settings.active_connection_id = connection.connection_id.clone();
    draft.connection_id = Some(connection.connection_id);
    assert!(settings.prepare_save(&draft).is_err());
    draft.connection_id = None;
    draft.provider_id = "codex".into();
    assert!(settings.prepare_save(&draft).is_err());
    draft.base_url = providers::default_base_url("codex").unwrap().into();
    assert!(settings.prepare_save(&draft).is_ok());
  }

  #[test]
  fn migration_keeps_identity_and_old_credential_reference_without_copying_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let old = serde_json::json!({"base_url":"https://example.invalid/v1","model_id":"model","connection_id":"previous-id"});
    std::fs::write(dir.path().join("llm.json"), old.to_string()).unwrap();
    let settings = Settings::load(dir.path()).unwrap();
    let active = settings.active();
    assert_eq!(active.connection_id, "previous-id");
    assert_eq!(active.provider_id, "custom");
    assert_eq!(
      active.credential.account,
      digest(b"https://example.invalid/v1")
    );
    assert!(matches!(active.credential.format, CredentialFormat::ApiKey));
    let reopened = Settings::load(dir.path()).unwrap();
    assert_eq!(reopened.active().connection_id, active.connection_id);
    assert_eq!(
      std::fs::read_to_string(dir.path().join("llm.json")).unwrap(),
      old.to_string()
    );
    assert!(active.public("present").get("credential").is_none());
  }

  #[test]
  fn editing_legacy_trailing_slashes_keeps_the_connection_identity() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("llm.json"), br#"{"base_url":"https://example.invalid/v1///","model_id":"model","connection_id":"previous-id"}"#).unwrap();
    let settings = Settings::load(dir.path()).unwrap();
    let mut edit = input();
    edit.connection_id = Some(settings.active_connection_id.clone());
    edit.base_url = settings.active().base_url.clone();
    let next = settings.prepare_save(&edit).unwrap();
    assert_eq!(next.connection_id, settings.active_connection_id);
  }

  #[test]
  fn invalid_old_endpoint_stays_editable_and_is_never_admitted_for_generation() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
      dir.path().join("llm.json"),
      br#"{"base_url":"https://example.invalid./v1","model_id":"model"}"#,
    )
    .unwrap();
    let settings = Settings::load(dir.path()).unwrap();
    assert!(validate_config(&settings.active().config()).is_err());
    let mut fixed = input();
    fixed.connection_id = Some(settings.active_connection_id.clone());
    fixed.key_action = KeyAction::Remove;
    assert!(settings.prepare_save(&fixed).is_ok());
  }
}
