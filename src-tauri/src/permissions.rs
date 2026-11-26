use crate::host::Host;
use apell_core::{
  bundles::{digest, AppletListReport, InstalledApplet, PackageSource},
  config::ConfigStore,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashSet, path::PathBuf};
use tauri::{AppHandle, Emitter};

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

  pub fn system_authorization(self, app: &AppHandle) -> SystemAuthorization {
    if !self.available() {
      return SystemAuthorization::Unknown;
    }
    match self {
      Self::Microphone => {
        #[cfg(target_os = "macos")]
        {
          SystemAuthorization::from_native(crate::desktop::microphone_authorization())
        }
        #[cfg(any(target_os = "ios", target_os = "android"))]
        {
          tauri_plugin_apell_audio::system_authorization(app)
            .map(|status| SystemAuthorization::from_native(&status))
            .unwrap_or_default()
        }
        #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
        SystemAuthorization::Unknown
      }
      Self::Camera | Self::Location => {
        tauri_plugin_apell_sensors::system_authorization(app, self.label())
          .map(|status| SystemAuthorization::from_native(&status))
          .unwrap_or_default()
      }
    }
  }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemAuthorization {
  Allowed,
  Blocked,
  NotDetermined,
  NotGranted,
  #[default]
  Unknown,
}

impl SystemAuthorization {
  fn from_native(status: &str) -> Self {
    match status {
      "allowed" => Self::Allowed,
      "blocked" => Self::Blocked,
      "not_determined" => Self::NotDetermined,
      "not_granted" => Self::NotGranted,
      _ => Self::Unknown,
    }
  }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PermissionAccess {
  Ask,
  Remembered,
  Blocked,
  PreviousPackage,
  Undeclared,
  UnverifiedPackage,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
  Ask,
  Remembered,
  Blocked,
}

impl PermissionAccess {
  fn mode(self) -> PermissionMode {
    match self {
      Self::Remembered => PermissionMode::Remembered,
      Self::Blocked => PermissionMode::Blocked,
      _ => PermissionMode::Ask,
    }
  }
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

pub(crate) fn installed_revision(applet: &InstalledApplet) -> Result<String, String> {
  let snapshot = serde_json::to_vec(&(&applet.sha256, &applet.source))
    .map_err(|_| "Could not encode the installed applet identity.")?;
  Ok(digest(&snapshot))
}

fn blocked_value(source: &GrantSource) -> Value {
  json!({"mode": PermissionMode::Blocked, "source": source})
}

/// Separate host storage. The applet config bridge cannot access grants.
pub struct PermissionGrants(ConfigStore);

impl PermissionGrants {
  pub fn new(root: PathBuf) -> Self {
    Self(ConfigStore::new(root))
  }

  #[cfg(test)]
  pub fn remembered(
    &self,
    permission: ManagedPermission,
    applet_id: &str,
    source: &GrantSource,
  ) -> Result<bool, String> {
    Ok(self.mode(permission, applet_id, source)? == PermissionMode::Remembered)
  }

  pub fn mode(
    &self,
    permission: ManagedPermission,
    applet_id: &str,
    source: &GrantSource,
  ) -> Result<PermissionMode, String> {
    Ok(self.access(permission, applet_id, Some(source))?.mode())
  }

  /// Check the applet's policy before device access. True skips Apell's prompt.
  pub fn authorize_request(
    &self,
    permission: ManagedPermission,
    applet_id: &str,
    source: &GrantSource,
  ) -> Result<bool, String> {
    match self.mode(permission, applet_id, source)? {
      PermissionMode::Remembered => Ok(true),
      PermissionMode::Ask => Ok(false),
      PermissionMode::Blocked => Err(format!(
        "{} access is blocked for this applet in Apell. Change it in Applet permissions.",
        match permission {
          ManagedPermission::Microphone => "Microphone",
          ManagedPermission::Camera => "Camera",
          ManagedPermission::Location => "Location",
        }
      )),
    }
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
      PermissionMode::Blocked => self.store(permission, applet_id, blocked_value(source))?,
      PermissionMode::Ask if self.mode(permission, applet_id, source)? != PermissionMode::Ask => {
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
      Some(value) if source.is_some_and(|source| *value == blocked_value(source)) => {
        PermissionAccess::Blocked
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
    self.store(permission, applet_id, json!(source))
  }

  fn store(
    &self,
    permission: ManagedPermission,
    applet_id: &str,
    value: Value,
  ) -> Result<(), String> {
    self.0.dispatch(
      applet_id,
      "config.set",
      json!({"key": permission.label(), "value": value}),
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
        PermissionAccess::Remembered | PermissionAccess::Blocked if !declared => {
          PermissionAccess::Undeclared
        }
        access => access,
      };
      grants.push(GrantSummary {
        permission,
        access,
        mode: (declared && source.is_some()).then_some(access.mode()),
        available: permission.available(),
        system_authorization: SystemAuthorization::Unknown,
      });
    }
    Ok(grants)
  }

  fn installed_summary(
    &self,
    applet: InstalledApplet,
    restart_required: bool,
  ) -> Result<AppletPermissionSummary, String> {
    let revision = installed_revision(&applet)?;
    let source = GrantSource::from(&applet);
    let manifest = applet.manifest;
    let grants = self.summary(&manifest.applet_id, Some(&source), &manifest.permissions)?;
    Ok(AppletPermissionSummary {
      applet_id: manifest.applet_id,
      name: manifest.name,
      installed_revision: Some(revision),
      manifest: Some(PermissionManifest {
        network: manifest.network,
        permissions: manifest.permissions,
        has_lib: manifest.lib.is_some(),
      }),
      grants,
      restart_required,
    })
  }

  fn summaries(
    &self,
    report: AppletListReport,
    pending_restarts: &HashSet<String>,
  ) -> Result<Vec<AppletPermissionSummary>, String> {
    let mut entries = Vec::new();
    for applet in report.applets {
      let restart_required = pending_restarts.contains(&applet.manifest.applet_id);
      entries.push(self.installed_summary(applet, restart_required)?);
    }
    for applet in report.unavailable {
      let grants = self.summary(&applet.applet_id, None, &[])?;
      if !grants.is_empty() {
        entries.push(AppletPermissionSummary {
          applet_id: applet.applet_id.clone(),
          name: applet.applet_id,
          installed_revision: None,
          manifest: None,
          grants,
          restart_required: false,
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
  #[serde(skip_serializing_if = "Option::is_none")]
  mode: Option<PermissionMode>,
  available: bool,
  system_authorization: SystemAuthorization,
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
  #[serde(skip_serializing_if = "Option::is_none")]
  installed_revision: Option<String>,
  manifest: Option<PermissionManifest>,
  grants: Vec<GrantSummary>,
  restart_required: bool,
}

fn attach_system_authorizations(
  entries: &mut [AppletPermissionSummary],
  mut read: impl FnMut(ManagedPermission) -> SystemAuthorization,
) {
  for permission in ManagedPermission::ALL {
    let mut grants = entries
      .iter_mut()
      .filter(|entry| {
        entry.manifest.as_ref().is_some_and(|manifest| {
          manifest
            .permissions
            .iter()
            .any(|group| group == permission.manifest_group())
        })
      })
      .flat_map(|entry| &mut entry.grants)
      .filter(|grant| grant.permission == permission && grant.available)
      .peekable();
    if grants.peek().is_some() {
      let authorization = read(permission);
      for grant in grants {
        grant.system_authorization = authorization;
      }
    }
  }
}

pub async fn list(app: AppHandle, host: &Host) -> Result<Value, String> {
  let bundles = host.bundles.clone();
  let store = host.grants.clone();
  let report = crate::commands::blocking(move || bundles.list_report()).await?;
  let pending_restarts = report
    .applets
    .iter()
    .filter(|applet| host.needs_applet_restart(applet))
    .map(|applet| applet.manifest.applet_id.clone())
    .collect();
  crate::commands::blocking(move || {
    let mut entries = store.summaries(report, &pending_restarts)?;
    attach_system_authorizations(&mut entries, |permission| {
      permission.system_authorization(&app)
    });
    Ok(json!(entries))
  })
  .await
}

pub fn publish_device_change(app: &AppHandle) {
  let _ = app.emit_to("main", "apell-permissions-changed", ());
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
    let revision = installed_revision(&applet).unwrap();
    let summary = grants
      .summaries(
        AppletListReport {
          applets: vec![applet],
          unavailable: Vec::new(),
          retained: Default::default(),
        },
        &HashSet::new(),
      )
      .unwrap();
    assert_eq!(
      json!(summary),
      json!([{
        "applet_id":"recorder",
        "name":"Recorder",
        "installed_revision":revision,
        "manifest":{
          "network":false,
          "permissions":["secrets","fs","photos"],
          "has_lib":true,
        },
        "grants":[],
        "restart_required":false,
      }])
    );
  }

  #[test]
  fn shared_system_status_preserves_each_applets_approval() {
    let entry = |id: &str, access: PermissionAccess, declared: bool| AppletPermissionSummary {
      applet_id: id.into(),
      name: id.into(),
      installed_revision: None,
      manifest: Some(PermissionManifest {
        permissions: if declared {
          vec!["audio".into()]
        } else {
          vec![]
        },
        network: true,
        has_lib: false,
      }),
      grants: vec![GrantSummary {
        permission: ManagedPermission::Microphone,
        access,
        mode: declared.then_some(access.mode()),
        available: true,
        system_authorization: SystemAuthorization::Unknown,
      }],
      restart_required: false,
    };
    let mut summaries = [
      entry("recorder", PermissionAccess::Remembered, true),
      entry("memo", PermissionAccess::Ask, true),
      entry("previous", PermissionAccess::Undeclared, false),
    ];
    let mut calls = Vec::new();
    attach_system_authorizations(&mut summaries, |permission| {
      calls.push(permission);
      SystemAuthorization::Blocked
    });
    assert_eq!(calls, [ManagedPermission::Microphone]);
    assert!(summaries[0].grants[0].access == PermissionAccess::Remembered);
    assert!(summaries[1].grants[0].access == PermissionAccess::Ask);
    assert_eq!(
      summaries[0].grants[0].system_authorization,
      SystemAuthorization::Blocked
    );
    assert_eq!(
      summaries[1].grants[0].system_authorization,
      SystemAuthorization::Blocked
    );
    assert_eq!(
      summaries[2].grants[0].system_authorization,
      SystemAuthorization::Unknown
    );
  }

  #[test]
  fn settings_marks_package_restart_independently_of_saved_grants() {
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
    let source = GrantSource::from(&applet);
    grants
      .remember(
        ManagedPermission::Microphone,
        &applet.manifest.applet_id,
        &source,
      )
      .unwrap();
    applet.manifest.network = false;
    applet.manifest.permissions.clear();
    let pending_restarts = HashSet::from([applet.manifest.applet_id.clone()]);
    let entries = grants
      .summaries(
        AppletListReport {
          applets: vec![applet],
          unavailable: Vec::new(),
          retained: Default::default(),
        },
        &pending_restarts,
      )
      .unwrap();
    let [entry] = entries.as_slice() else {
      panic!("Expected one applet permission summary.");
    };
    assert!(entry.restart_required);
    assert!(!entry.manifest.as_ref().unwrap().network);
    assert!(entry.manifest.as_ref().unwrap().permissions.is_empty());
    let [approval] = entry.grants.as_slice() else {
      panic!("Expected one saved device approval.");
    };
    assert_eq!(approval.permission, ManagedPermission::Microphone);
    assert!(approval.access == PermissionAccess::Undeclared);
    assert!(grants
      .remembered(ManagedPermission::Microphone, &entry.applet_id, &source)
      .unwrap());
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
        {"permission":"camera","access":"previous_package","available":sensors_available,"system_authorization":"unknown"},
        {"permission":"location","access":"ask","mode":"ask","available":sensors_available,"system_authorization":"unknown"},
      ]),
    );
    assert_eq!(
      json!(grants
        .summary(applet_id, Some(&source), &["camera".into()])
        .unwrap()),
      json!([{"permission":"camera","access":"remembered","mode":"remembered","available":sensors_available,"system_authorization":"unknown"}]),
    );
    assert_eq!(
      json!(grants.summary(applet_id, Some(&source), &[]).unwrap()),
      json!([{"permission":"camera","access":"undeclared","available":sensors_available,"system_authorization":"unknown"}]),
    );
    assert_eq!(
      json!(grants.summary(applet_id, None, &[]).unwrap()),
      json!([{"permission":"camera","access":"unverified_package","available":sensors_available,"system_authorization":"unknown"}]),
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

  #[test]
  fn blocked_access_persists_and_rejects_every_managed_device_request() {
    let dir = tempfile::tempdir().unwrap();
    let grants = PermissionGrants::new(dir.path().into());
    let source = GrantSource::File {
      sha256: "a".repeat(64),
    };
    let replacement = GrantSource::File {
      sha256: "b".repeat(64),
    };
    let applet_id = "fixture";
    let declared = vec!["audio".into(), "camera".into(), "location".into()];
    for permission in ManagedPermission::ALL {
      grants.remember(permission, applet_id, &source).unwrap();
      assert!(grants
        .authorize_request(permission, applet_id, &source)
        .unwrap());
      assert_eq!(
        grants
          .set_mode(permission, applet_id, &source, PermissionMode::Blocked)
          .unwrap(),
        PermissionMode::Blocked,
      );
    }

    let reopened = PermissionGrants::new(dir.path().into());
    let summary = reopened
      .summary(applet_id, Some(&source), &declared)
      .unwrap();
    let replacement_summary = reopened
      .summary(applet_id, Some(&replacement), &declared)
      .unwrap();
    for ((permission, active), previous) in ManagedPermission::ALL
      .into_iter()
      .zip(summary)
      .zip(replacement_summary)
    {
      assert!(reopened
        .authorize_request(permission, applet_id, &source)
        .unwrap_err()
        .contains("blocked for this applet"));
      assert!(!reopened
        .authorize_request(permission, applet_id, &replacement)
        .unwrap());
      assert!(active.access == PermissionAccess::Blocked);
      assert_eq!(active.mode, Some(PermissionMode::Blocked));
      assert!(previous.access == PermissionAccess::PreviousPackage);
      assert_eq!(previous.mode, Some(PermissionMode::Ask));

      reopened
        .set_mode(permission, applet_id, &source, PermissionMode::Ask)
        .unwrap();
      assert!(!reopened
        .authorize_request(permission, applet_id, &source)
        .unwrap());
    }
  }

  #[test]
  fn installed_permission_revision_includes_package_hash_and_full_source() {
    let source = StoreSource {
      store_url: "https://store.example".into(),
      applet_id: "recorder".into(),
      channel: StoreChannel::Stable,
    };
    let mut applet = installed(source.clone().into());
    let original = installed_revision(&applet).unwrap();
    let grant = GrantSource::from(&applet);
    applet.source = Some(
      StoreSource {
        channel: StoreChannel::Nightly,
        ..source
      }
      .into(),
    );
    let nightly = installed_revision(&applet).unwrap();
    assert_ne!(original, nightly);
    assert_eq!(GrantSource::from(&applet), grant);
    applet.sha256 = "b".repeat(64);
    assert_ne!(installed_revision(&applet).unwrap(), nightly);
  }
}
