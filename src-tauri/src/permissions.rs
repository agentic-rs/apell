use crate::host::Host;
use apell_core::{bundles::InstalledApplet, config::ConfigStore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedPermission {
  Microphone,
  Camera,
  Location,
}

impl ManagedPermission {
  const ALL: [Self; 3] = [Self::Microphone, Self::Camera, Self::Location];

  pub fn manifest_group(self) -> &'static str {
    match self {
      Self::Microphone => "audio",
      permission => permission.label(),
    }
  }

  pub fn label(self) -> &'static str {
    match self {
      Self::Microphone => "microphone",
      Self::Camera => "camera",
      Self::Location => "location",
    }
  }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PermissionAccess {
  Ask,
  Remembered,
  PreviousPackage,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
  Ask,
  Remembered,
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
pub struct PermissionGrants(ConfigStore);

impl PermissionGrants {
  pub fn new(root: PathBuf) -> Self {
    Self(ConfigStore::new(root))
  }

  pub fn remembered(
    &self,
    permission: ManagedPermission,
    applet_id: &str,
    source: &GrantSource,
  ) -> Result<bool, String> {
    Ok(self.access(permission, applet_id, Some(source))? == PermissionAccess::Remembered)
  }

  pub fn mode(
    &self,
    permission: ManagedPermission,
    applet_id: &str,
    source: &GrantSource,
  ) -> Result<PermissionMode, String> {
    Ok(if self.remembered(permission, applet_id, source)? {
      PermissionMode::Remembered
    } else {
      PermissionMode::Ask
    })
  }

  pub fn set_mode(
    &self,
    permission: ManagedPermission,
    applet_id: &str,
    source: &GrantSource,
    mode: PermissionMode,
  ) -> Result<PermissionMode, String> {
    match mode {
      PermissionMode::Remembered => self.remember(permission, applet_id, source)?,
      PermissionMode::Ask if self.remembered(permission, applet_id, source)? => {
        self.forget(permission, applet_id)?;
      }
      PermissionMode::Ask => {}
    }
    Ok(mode)
  }

  fn access(
    &self,
    permission: ManagedPermission,
    applet_id: &str,
    source: Option<&GrantSource>,
  ) -> Result<PermissionAccess, String> {
    let stored = self
      .0
      .dispatch(applet_id, "config.get", json!({"key": permission.label()}))?;
    Ok(match stored.get("value") {
      None => PermissionAccess::Ask,
      Some(value) if source.is_some_and(|source| *value == json!(source)) => {
        PermissionAccess::Remembered
      }
      Some(_) => PermissionAccess::PreviousPackage,
    })
  }

  pub fn remember(
    &self,
    permission: ManagedPermission,
    applet_id: &str,
    source: &GrantSource,
  ) -> Result<(), String> {
    self.0.dispatch(
      applet_id,
      "config.set",
      json!({"key": permission.label(), "value": source}),
    )?;
    Ok(())
  }

  pub fn forget(&self, permission: ManagedPermission, applet_id: &str) -> Result<(), String> {
    self.0.dispatch(
      applet_id,
      "config.delete",
      json!({"key": permission.label()}),
    )?;
    Ok(())
  }

  fn summary(
    &self,
    applet_id: &str,
    source: Option<&GrantSource>,
    declared: &[String],
  ) -> Result<Vec<GrantSummary>, String> {
    let mut grants = Vec::new();
    for permission in ManagedPermission::ALL {
      let access = self.access(permission, applet_id, source)?;
      if access != PermissionAccess::Ask
        || declared
          .iter()
          .any(|group| group == permission.manifest_group())
      {
        grants.push(GrantSummary { permission, access });
      }
    }
    Ok(grants)
  }
}

#[derive(Serialize)]
struct GrantSummary {
  permission: ManagedPermission,
  access: PermissionAccess,
}

pub async fn list(host: &Host) -> Result<Value, String> {
  let bundles = host.bundles.clone();
  let store = host.grants.clone();
  crate::commands::blocking(move || {
    let mut entries = Vec::new();
    let report = bundles.list_report()?;
    for applet in report.applets {
      let grants = store.summary(
        &applet.manifest.applet_id,
        Some(&GrantSource::from(&applet)),
        &applet.manifest.permissions,
      )?;
      if !grants.is_empty() {
        entries.push(json!({
          "applet_id": applet.manifest.applet_id,
          "name": applet.manifest.name,
          "grants": grants,
        }));
      }
    }
    for applet in report.unavailable {
      let grants = store.summary(&applet.applet_id, None, &[])?;
      if !grants.is_empty() {
        entries.push(json!({
          "applet_id": applet.applet_id,
          "name": applet.applet_id,
          "grants": grants,
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
    let microphone = ManagedPermission::Microphone;
    let source = GrantSource::Github {
      repository_id: 7,
      asset_name: "recordings.apel".into(),
      release_tag: None,
    };
    let local = GrantSource::File {
      sha256: "a".repeat(64),
    };
    // Grant files written before Camera and Location keep their meaning.
    ConfigStore::new(dir.path().into())
      .dispatch(
        "recorder",
        "config.set",
        json!({"key":"microphone","value":source}),
      )
      .unwrap();
    let reopened = PermissionGrants::new(dir.path().into());
    assert!(reopened
      .remembered(microphone, "recorder", &source)
      .unwrap());
    assert!(!reopened.remembered(microphone, "recorder", &local).unwrap());
    assert!(!reopened.remembered(microphone, "other", &source).unwrap());
    reopened.remember(microphone, "recorder", &local).unwrap();
    assert!(!reopened
      .remembered(microphone, "recorder", &source)
      .unwrap());
    assert!(!reopened
      .remembered(
        microphone,
        "recorder",
        &GrantSource::File {
          sha256: "b".repeat(64),
        },
      )
      .unwrap());
    reopened.forget(microphone, "recorder").unwrap();
    assert!(!reopened.remembered(microphone, "recorder", &local).unwrap());
  }

  #[test]
  fn independent_sensor_grants_include_earlier_packages_in_settings() {
    use ManagedPermission::{Camera, Location, Microphone};

    let dir = tempfile::tempdir().unwrap();
    let grants = PermissionGrants::new(dir.path().into());
    let source = GrantSource::File {
      sha256: "a".repeat(64),
    };
    let updated = GrantSource::File {
      sha256: "b".repeat(64),
    };
    let applet_id = "fixture";
    grants.remember(Camera, applet_id, &source).unwrap();
    assert!(!grants.remembered(Location, applet_id, &source).unwrap());
    assert!(!grants.remembered(Microphone, applet_id, &source).unwrap());
    assert_eq!(
      json!(grants
        .summary(applet_id, Some(&updated), &["location".into()])
        .unwrap()),
      json!([
        {"permission":"camera","access":"previous_package"},
        {"permission":"location","access":"ask"},
      ]),
    );
    grants.remember(Location, applet_id, &updated).unwrap();
    grants.forget(Camera, applet_id).unwrap();
    assert!(!grants.remembered(Camera, applet_id, &source).unwrap());
    assert!(grants.remembered(Location, applet_id, &updated).unwrap());
  }
}
