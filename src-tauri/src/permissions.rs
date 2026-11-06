use crate::host::Host;
use apell_core::{bundles::InstalledApplet, config::ConfigStore};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::PathBuf;

const MICROPHONE: &str = "microphone";

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum MicrophoneAccess {
  Ask,
  Remembered,
  PreviousPackage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GrantSource {
  Github {
    repository_id: u64,
    asset_name: String,
    release_tag: Option<String>,
  },
  File {
    sha256: String,
  },
}

impl From<&InstalledApplet> for GrantSource {
  fn from(applet: &InstalledApplet) -> Self {
    match &applet.source {
      Some(source) => Self::Github {
        repository_id: source.repository_id,
        asset_name: source.asset_name.clone(),
        release_tag: source.release_tag.clone(),
      },
      None => Self::File {
        sha256: applet.sha256.clone(),
      },
    }
  }
}

/// Separate host storage. The applet config bridge cannot access grants.
pub struct MicrophoneGrants(ConfigStore);

impl MicrophoneGrants {
  pub fn new(root: PathBuf) -> Self {
    Self(ConfigStore::new(root))
  }

  pub fn remembered(&self, applet_id: &str, source: &GrantSource) -> Result<bool, String> {
    Ok(self.access(applet_id, Some(source))? == MicrophoneAccess::Remembered)
  }

  fn access(
    &self,
    applet_id: &str,
    source: Option<&GrantSource>,
  ) -> Result<MicrophoneAccess, String> {
    let stored = self
      .0
      .dispatch(applet_id, "config.get", json!({"key": MICROPHONE}))?;
    Ok(match stored.get("value") {
      None => MicrophoneAccess::Ask,
      Some(value) if source.is_some_and(|source| *value == json!(source)) => {
        MicrophoneAccess::Remembered
      }
      Some(_) => MicrophoneAccess::PreviousPackage,
    })
  }

  pub fn remember(&self, applet_id: &str, source: &GrantSource) -> Result<(), String> {
    self.0.dispatch(
      applet_id,
      "config.set",
      json!({"key": MICROPHONE, "value": source}),
    )?;
    Ok(())
  }

  pub fn forget(&self, applet_id: &str) -> Result<(), String> {
    self
      .0
      .dispatch(applet_id, "config.delete", json!({"key": MICROPHONE}))?;
    Ok(())
  }
}

pub async fn list(host: &Host) -> Result<Value, String> {
  let bundles = host.bundles.clone();
  let microphone = host.microphone.clone();
  crate::commands::blocking(move || {
    let mut entries = Vec::new();
    let report = bundles.list_report()?;
    for applet in report.applets {
      let access = microphone.access(
        &applet.manifest.applet_id,
        Some(&GrantSource::from(&applet)),
      )?;
      if access != MicrophoneAccess::Ask
        || applet
          .manifest
          .permissions
          .iter()
          .any(|item| item == "audio")
      {
        entries.push(json!({
          "applet_id": applet.manifest.applet_id,
          "name": applet.manifest.name,
          "microphone_access": access,
        }));
      }
    }
    for applet in report.unavailable {
      let access = microphone.access(&applet.applet_id, None)?;
      if access != MicrophoneAccess::Ask {
        entries.push(json!({
          "applet_id": applet.applet_id,
          "name": applet.applet_id,
          "microphone_access": access,
        }));
      }
    }
    Ok(json!(entries))
  })
  .await
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn remembered_grant_persists_for_the_applet_and_verified_source() {
    let dir = tempfile::tempdir().unwrap();
    let source = GrantSource::Github {
      repository_id: 7,
      asset_name: "recordings.apel".into(),
      release_tag: None,
    };
    let local = GrantSource::File {
      sha256: "a".repeat(64),
    };
    MicrophoneGrants::new(dir.path().into())
      .remember("recorder", &source)
      .unwrap();
    let reopened = MicrophoneGrants::new(dir.path().into());
    assert!(reopened.remembered("recorder", &source).unwrap());
    assert!(!reopened.remembered("recorder", &local).unwrap());
    assert!(!reopened.remembered("other", &source).unwrap());
    reopened.remember("recorder", &local).unwrap();
    assert!(!reopened.remembered("recorder", &source).unwrap());
    assert!(!reopened
      .remembered(
        "recorder",
        &GrantSource::File {
          sha256: "b".repeat(64),
        },
      )
      .unwrap());
    reopened.forget("recorder").unwrap();
    assert!(!reopened.remembered("recorder", &local).unwrap());
  }
}
