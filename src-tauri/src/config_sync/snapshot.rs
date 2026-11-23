use super::saved_selection;
use crate::settings::{
  credential_ref, Connection, Settings, LEGACY_DEMO_CONNECTION_ID, MAX_CONNECTIONS,
};
use apell_core::{bundles::digest, llm::validate_config, providers::default_base_url};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Snapshot {
  schema_version: u32,
  connections: Vec<Selection>,
  pub active_connection_id: String,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Selection {
  connection_id: String,
  name: String,
  provider_id: String,
  model_id: String,
}

#[derive(Serialize)]
pub(super) struct Summary<'a> {
  connections: Vec<SummaryConnection<'a>>,
  active_model: Option<&'a str>,
}

#[derive(Serialize)]
struct SummaryConnection<'a> {
  name: &'a str,
  provider_id: &'a str,
  model_id: &'a str,
}

fn built_in_endpoint(provider_id: &str) -> Option<&'static str> {
  if matches!(provider_id, "custom" | "llama-cpp") {
    return None;
  }
  default_base_url(provider_id)
    .filter(|url| url.starts_with("https://"))
    .map(|url| url.trim_end_matches('/'))
}

fn eligible(connection: &Connection) -> bool {
  built_in_endpoint(&connection.provider_id)
    .is_some_and(|endpoint| connection.base_url.trim_end_matches('/') == endpoint)
}

pub(super) fn device_only(connection: &Connection) -> bool {
  !eligible(connection)
}

impl Snapshot {
  pub fn capture(settings: &Settings, previous_selection: Option<&str>) -> Self {
    let mut connections: Vec<_> = settings
      .connections
      .iter()
      .filter(|connection| eligible(connection))
      .map(|connection| Selection {
        connection_id: connection.connection_id.clone(),
        name: connection.name.clone(),
        provider_id: connection.provider_id.clone(),
        model_id: connection.model_id.clone(),
      })
      .collect();
    connections.sort_unstable_by(|a, b| a.connection_id.cmp(&b.connection_id));
    // Keep the v1 wire value for no preferred connection so existing Store
    // revisions and their fingerprints survive the local settings migration.
    let active_connection_id = previous_selection
      .or(settings.active_connection_id.as_deref())
      .filter(|id| connections.iter().any(|entry| entry.connection_id == *id))
      .unwrap_or(LEGACY_DEMO_CONNECTION_ID)
      .to_owned();
    Self {
      schema_version: 1,
      connections,
      active_connection_id,
    }
  }

  pub fn validate(&mut self) -> Result<(), String> {
    let mut ids = HashSet::new();
    if self.schema_version != 1
      || self.connections.len() > MAX_CONNECTIONS
      || self.connections.iter().any(|entry| {
        entry.connection_id == LEGACY_DEMO_CONNECTION_ID
          || !ids.insert(entry.connection_id.as_str())
          || entry.name.is_empty()
          || entry.name.len() > 80
          || entry.name.trim() != entry.name
          || entry.name.chars().any(char::is_control)
          || entry.model_id.trim() != entry.model_id
      })
      || saved_selection(&self.active_connection_id).is_some_and(|id| !ids.contains(id))
    {
      return Err("The synced provider settings are invalid.".into());
    }
    for entry in &self.connections {
      validate_config(&entry.connection()?.config())?;
    }
    self
      .connections
      .sort_unstable_by(|a, b| a.connection_id.cmp(&b.connection_id));
    Ok(())
  }

  pub fn fingerprint(&self) -> Result<String, String> {
    serde_json::to_vec(self)
      .map(|bytes| digest(&bytes))
      .map_err(|_| "Could not encode provider settings for sync.".into())
  }

  pub fn is_empty(&self) -> bool {
    self.connections.is_empty()
  }

  pub fn summary(&self) -> Summary<'_> {
    Summary {
      connections: self
        .connections
        .iter()
        .map(|entry| SummaryConnection {
          name: &entry.name,
          provider_id: &entry.provider_id,
          model_id: &entry.model_id,
        })
        .collect(),
      active_model: self
        .connections
        .iter()
        .find(|entry| entry.connection_id == self.active_connection_id)
        .map(|entry| entry.model_id.as_str()),
    }
  }

  pub fn removes_connections_from(&self, local: &Self) -> bool {
    local.connections.iter().any(|current| {
      !self
        .connections
        .iter()
        .any(|remote| remote.connection_id == current.connection_id)
    })
  }

  pub fn settings(&self, current: &Settings) -> Result<(Settings, Vec<String>), String> {
    let mut next = current.clone();
    next.connections.retain(|connection| !eligible(connection));
    let mut credential_removals: Vec<_> = current
      .connections
      .iter()
      .filter(|old| {
        eligible(old)
          && !self
            .connections
            .iter()
            .any(|entry| entry.connection_id == old.connection_id)
      })
      .map(|old| old.credential.account.clone())
      .collect();
    for entry in &self.connections {
      let mut connection = entry.connection()?;
      if let Ok(old) = current.connection(&entry.connection_id) {
        if !eligible(old) || old.provider_id != entry.provider_id {
          return Err("A synced connection conflicts with a local provider or endpoint.".into());
        }
        connection.credential = old.credential.clone();
      } else {
        // A cloud ID must never attach to a stale Keychain slot for a removed connection.
        connection.credential = credential_ref(&Uuid::new_v4().to_string());
      }
      next.connections.push(connection);
    }
    if next.connections.len() > MAX_CONNECTIONS {
      return Err(
        "These synced settings exceed the connection limit with your device-only connections."
          .into(),
      );
    }
    if current.active().is_none_or(eligible) {
      next.active_connection_id = saved_selection(&self.active_connection_id).map(str::to_owned);
    }
    credential_removals.sort_unstable();
    Ok((next, credential_removals))
  }

  pub(super) fn missing_key_connections(
    &self,
    current: &Settings,
    ids: &HashSet<String>,
  ) -> Result<Vec<Connection>, String> {
    let remaining = MAX_CONNECTIONS.saturating_sub(current.connections.len());
    self
      .connections
      .iter()
      .filter(|entry| {
        ids.contains(&entry.connection_id)
          && current.connection(&entry.connection_id).is_err()
          && !matches!(entry.provider_id.as_str(), "codex" | "github-copilot")
      })
      .take(remaining)
      .map(|entry| {
        let mut connection = entry.connection()?;
        connection.credential = credential_ref(&Uuid::new_v4().to_string());
        Ok(connection)
      })
      .collect()
  }
}

impl Selection {
  fn connection(&self) -> Result<Connection, String> {
    let endpoint = built_in_endpoint(&self.provider_id)
      .ok_or("Update Apell to use the synced provider, or keep this device's settings.")?;
    Ok(Connection {
      connection_id: self.connection_id.clone(),
      name: self.name.clone(),
      provider_id: self.provider_id.clone(),
      base_url: endpoint.into(),
      model_id: self.model_id.clone(),
      credential: credential_ref(&self.connection_id),
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  fn saved(connection_id: &str, provider_id: &str, base_url: &str) -> Connection {
    Connection {
      connection_id: connection_id.into(),
      name: "Fixture connection".into(),
      provider_id: provider_id.into(),
      base_url: base_url.into(),
      model_id: "model-fixture".into(),
      credential: credential_ref(&format!("local-secret-slot-{connection_id}")),
    }
  }

  #[test]
  fn snapshot_excludes_endpoints_credential_references_and_device_only_connections() {
    let mut local = Settings::default();
    local.connections.extend([
      saved("public", "openai", built_in_endpoint("openai").unwrap()),
      saved("private", "openai", "https://private.example.invalid/v1"),
      saved("custom", "custom", "https://public.example.invalid/v1"),
    ]);
    local.active_connection_id = Some("private".into());
    let snapshot = Snapshot::capture(&local, Some("public"));
    assert_eq!(
      serde_json::to_value(snapshot).unwrap(),
      json!({
        "schema_version":1,
        "active_connection_id":"public",
        "connections":[{"connection_id":"public","name":"Fixture connection","provider_id":"openai","model_id":"model-fixture"}],
      })
    );
  }

  #[test]
  fn remote_application_preserves_device_only_connections_and_credential_destinations() {
    let mut local = Settings::default();
    let openai = saved("fixture", "openai", built_in_endpoint("openai").unwrap());
    let private = saved("private", "custom", "https://private.example.invalid/v1");
    local.connections.extend([openai.clone(), private.clone()]);
    local.active_connection_id = Some(private.connection_id.clone());
    let mut remote = Snapshot::capture(&local, Some("fixture"));
    remote.connections[0].model_id = "updated-model".into();
    assert!(remote.validate().is_ok());
    let (next, removed) = remote.settings(&local).unwrap();
    assert!(removed.is_empty());
    assert_eq!(next.active(), Some(&private));
    assert_eq!(
      next.connection("fixture").unwrap().model_id,
      "updated-model"
    );
    assert_eq!(
      next.connection("fixture").unwrap().credential,
      openai.credential
    );

    remote.connections[0].provider_id = "codex".into();
    assert!(built_in_endpoint("codex").is_some());
    assert!(remote.settings(&local).is_err());
    remote.connections[0].connection_id = "new-connection".into();
    remote.active_connection_id = "new-connection".into();
    let (next, removed) = remote.settings(&local).unwrap();
    assert_eq!(removed, [openai.credential.account]);
    assert_ne!(
      next.connection("new-connection").unwrap().credential,
      credential_ref("new-connection")
    );
    assert_eq!(next.connection("private").unwrap(), &private);
  }

  #[test]
  fn key_sync_metadata_only_adds_missing_requested_static_connections() {
    let mut local = Settings::default();
    let openai = saved(
      "already-local",
      "openai",
      built_in_endpoint("openai").unwrap(),
    );
    local.connections.push(openai.clone());
    let mut cloud = local.clone();
    cloud.connections[0].model_id = "changed-cloud-model".into();
    cloud.connections.extend([
      saved("with-key", "openai", built_in_endpoint("openai").unwrap()),
      saved(
        "without-key",
        "openai",
        built_in_endpoint("openai").unwrap(),
      ),
      saved("with-login", "codex", built_in_endpoint("codex").unwrap()),
    ]);
    let ids = HashSet::from([
      "already-local".into(),
      "with-key".into(),
      "with-login".into(),
    ]);
    let added = Snapshot::capture(&cloud, None)
      .missing_key_connections(&local, &ids)
      .unwrap();
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].connection_id, "with-key");
    assert_ne!(added[0].credential, credential_ref("with-key"));
    assert_eq!(local.connections, vec![openai]);
  }
}
