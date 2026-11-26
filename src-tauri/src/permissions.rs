use crate::host::Host;
use apell_core::{
  bundles::{AppletListReport, InstalledApplet, PackageSource},
  config::ConfigStore,
};
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

  pub fn available(self) -> bool {
    match self {
      Self::Microphone => crate::media::recording_available(),
      Self::Camera | Self::Location => tauri_plugin_apell_sensors::available(),
    }
  }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PermissionAccess {
  Ask,
  Remembered,
  PreviousPackage,
  Undeclared,
  UnverifiedPackage,
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
  Store {
    store_url: String,
    applet_id: String,
  },
  File {
    sha256: String,
  },
}

impl From<&InstalledApplet> for GrantSource {
  fn from(applet: &InstalledApplet) -> Self {
    match &applet.source {
      Some(PackageSource::Github(source)) => Self::Github {
        repository_id: source.repository_id,
        asset_name: source.asset_name.clone(),
        release_tag: source.release_tag.clone(),
      },
      Some(PackageSource::Store(source)) => Self::Store {
        store_url: source.store_url.clone(),
        applet_id: source.applet_id.clone(),
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

  pub fn forget_all(&self, applet_id: &str) -> Result<(), String> {
    self.0.clear(applet_id)
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
      let declared = declared
        .iter()
        .any(|group| group == permission.manifest_group());
      let access = match access {
        PermissionAccess::Ask if !declared => continue,
        _ if source.is_none() => PermissionAccess::UnverifiedPackage,
        PermissionAccess::Remembered if !declared => PermissionAccess::Undeclared,
        access => access,
      };
      grants.push(GrantSummary {
        permission,
        access,
        available: permission.available(),
      });
    }
    Ok(grants)
  }

  fn installed_summary(&self, applet: InstalledApplet) -> Result<AppletPermissionSummary, String> {
    let source = GrantSource::from(&applet);
    let manifest = applet.manifest;
    let grants = self.summary(&manifest.applet_id, Some(&source), &manifest.permissions)?;
    Ok(AppletPermissionSummary {
      applet_id: manifest.applet_id,
      name: manifest.name,
      manifest: Some(PermissionManifest {
        network: manifest.network,
        permissions: manifest.permissions,
        has_lib: manifest.lib.is_some(),
      }),
      grants,
    })
  }

  fn summaries(&self, report: AppletListReport) -> Result<Vec<AppletPermissionSummary>, String> {
    let mut entries = Vec::new();
    for applet in report.applets {
      entries.push(self.installed_summary(applet)?);
    }
    for applet in report.unavailable {
      let grants = self.summary(&applet.applet_id, None, &[])?;
      if !grants.is_empty() {
        entries.push(AppletPermissionSummary {
          applet_id: applet.applet_id.clone(),
          name: applet.applet_id,
          manifest: None,
          grants,
        });
      }
    }
    Ok(entries)
  }
}

#[derive(Serialize)]
struct GrantSummary {
  permission: ManagedPermission,
  access: PermissionAccess,
  available: bool,
}

#[derive(Serialize)]
struct PermissionManifest {
  network: bool,
  permissions: Vec<String>,
  has_lib: bool,
}

#[derive(Serialize)]
struct AppletPermissionSummary {
  applet_id: String,
  name: String,
  manifest: Option<PermissionManifest>,
  grants: Vec<GrantSummary>,
}

pub async fn list(host: &Host) -> Result<Value, String> {
  let bundles = host.bundles.clone();
  let store = host.grants.clone();
  crate::commands::blocking(move || Ok(json!(store.summaries(bundles.list_report()?)?))).await
}

#[cfg(test)]
mod tests {
  use super::*;
  use apell_core::bundles::{Manifest, StoreChannel, StoreSource};

  fn installed(source: PackageSource) -> InstalledApplet {
    InstalledApplet {
      manifest: serde_json::from_value::<Manifest>(json!({
        "schema_version": 1, "api_version": 2, "applet_id": "recorder", "name": "Recorder",
        "version": "1.0.0", "permissions": ["audio"]
      }))
      .unwrap(),
      sha256: "a".repeat(64),
      source: Some(source),
      icon_data_url: None,
    }
  }

  #[test]
  fn settings_includes_installed_applets_without_managed_grants() {
    let dir = tempfile::tempdir().unwrap();
    let grants = PermissionGrants::new(dir.path().into());
    let mut applet = installed(
      StoreSource {
        store_url: "https://store.example".into(),
        applet_id: "recorder".into(),
        channel: StoreChannel::Stable,
      }
      .into(),
    );
    applet.manifest.network = false;
    applet.manifest.permissions = vec!["secrets".into(), "fs".into(), "photos".into()];
    applet.manifest.lib = Some("lib/app.wasm".into());
    let summary = grants
      .summaries(AppletListReport {
        applets: vec![applet],
        unavailable: Vec::new(),
        retained: Default::default(),
      })
      .unwrap();
    assert_eq!(
      json!(summary),
      json!([{
        "applet_id":"recorder",
        "name":"Recorder",
        "manifest":{
          "network":false,
          "permissions":["secrets","fs","photos"],
          "has_lib":true,
        },
        "grants":[],
      }])
    );
  }

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
        json!({"key":"microphone","value":{
          "kind":"github", "repository_id":7, "asset_name":"recordings.apel", "release_tag":null
        }}),
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
  fn legacy_github_package_sources_keep_existing_grants_after_tagged_migration() {
    let dir = tempfile::tempdir().unwrap();
    let old_source = json!({
      "repository_id":7, "repository":"example/recordings", "asset_name":"recordings.apel"
    });
    let applet = installed(serde_json::from_value(old_source).unwrap());
    let source = GrantSource::from(&applet);
    let old_grant = json!({
      "kind":"github", "repository_id":7, "asset_name":"recordings.apel", "release_tag":null
    });
    assert_eq!(json!(source), old_grant);
    ConfigStore::new(dir.path().into())
      .dispatch(
        "recorder",
        "config.set",
        json!({"key":"microphone", "value":old_grant}),
      )
      .unwrap();
    let tagged: PackageSource = serde_json::from_value(json!(applet.source.unwrap())).unwrap();
    let migrated = GrantSource::from(&installed(tagged));
    assert!(PermissionGrants::new(dir.path().into())
      .remembered(ManagedPermission::Microphone, "recorder", &migrated)
      .unwrap());
  }

  #[test]
  fn store_grants_survive_version_and_channel_changes_but_not_other_origins() {
    let dir = tempfile::tempdir().unwrap();
    let stable = StoreSource {
      store_url: "https://store.example".into(),
      applet_id: "recorder".into(),
      channel: StoreChannel::Stable,
    };
    let applet = installed(stable.clone().into());
    let source = GrantSource::from(&applet);
    let permission = ManagedPermission::Microphone;
    PermissionGrants::new(dir.path().into())
      .remember(permission, "recorder", &source)
      .unwrap();
    let mut update = installed(
      StoreSource {
        channel: StoreChannel::Nightly,
        ..stable.clone()
      }
      .into(),
    );
    update.manifest.version = "2.0.0".into();
    update.sha256 = "b".repeat(64);
    let reopened = PermissionGrants::new(dir.path().into());
    assert!(reopened
      .remembered(permission, "recorder", &GrantSource::from(&update))
      .unwrap());
    assert_eq!(
      json!(source),
      json!({"kind":"store", "store_url":"https://store.example", "applet_id":"recorder"})
    );
    for changed in [
      StoreSource {
        store_url: "https://another.example".into(),
        ..stable.clone()
      },
      StoreSource {
        applet_id: "another".into(),
        ..stable
      },
    ] {
      assert!(!reopened
        .remembered(
          permission,
          "recorder",
          &GrantSource::from(&installed(changed.into()))
        )
        .unwrap());
    }
    update.source = None;
    assert!(!reopened
      .remembered(permission, "recorder", &GrantSource::from(&update))
      .unwrap());
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
    let sensors_available = cfg!(any(target_os = "ios", target_os = "android"));
    assert_eq!(
      json!(grants
        .summary(applet_id, Some(&updated), &["location".into()])
        .unwrap()),
      json!([
        {"permission":"camera","access":"previous_package","available":sensors_available},
        {"permission":"location","access":"ask","available":sensors_available},
      ]),
    );
    assert_eq!(
      json!(grants
        .summary(applet_id, Some(&source), &["camera".into()])
        .unwrap()),
      json!([{"permission":"camera","access":"remembered","available":sensors_available}]),
    );
    assert_eq!(
      json!(grants.summary(applet_id, Some(&source), &[]).unwrap()),
      json!([{"permission":"camera","access":"undeclared","available":sensors_available}]),
    );
    assert_eq!(
      json!(grants.summary(applet_id, None, &[]).unwrap()),
      json!([{"permission":"camera","access":"unverified_package","available":sensors_available}]),
    );
    grants.remember(Location, applet_id, &updated).unwrap();
    grants.forget(Camera, applet_id).unwrap();
    assert!(grants
      .summary(applet_id, Some(&source), &[])
      .unwrap()
      .iter()
      .all(|grant| grant.permission != Camera));
    assert!(!grants.remembered(Camera, applet_id, &source).unwrap());
    assert!(grants.remembered(Location, applet_id, &updated).unwrap());
  }
}
