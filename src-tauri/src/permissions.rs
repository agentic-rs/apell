use crate::host::Host;
use apell_core::{
  bundles::{
    digest, AppletListReport, AppletReportEntry, BundleStore, InstalledApplet, PackageSource,
    StoreSwitchSnapshot, UnavailableApplet,
  },
  config::ConfigStore,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashSet, path::PathBuf};
use tauri::{AppHandle, Emitter, Manager};

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

  pub fn open_system_settings(self, app: &AppHandle) -> Result<(), String> {
    if !self.system_authorization(app).can_open_system_settings() {
      publish_device_change(app);
      return Err("System settings are unavailable for the current permission state.".into());
    }
    #[cfg(target_os = "macos")]
    return crate::desktop::open_microphone_settings();
    #[cfg(any(target_os = "ios", target_os = "android"))]
    return tauri_plugin_apell_sensors::open_system_settings(app);
    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
    Err("System settings are unavailable on this platform.".into())
  }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemAuthorization {
  Allowed,
  Blocked,
  Restricted,
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
      "restricted" => Self::Restricted,
      "not_determined" => Self::NotDetermined,
      "not_granted" => Self::NotGranted,
      _ => Self::Unknown,
    }
  }

  pub fn can_open_system_settings(self) -> bool {
    match self {
      Self::Blocked => cfg!(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
      )),
      Self::NotGranted => cfg!(target_os = "android"),
      _ => false,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermissionEditBlocker {
  SourceChanged,
  PermissionRemoved,
  InstallationUnavailable,
}

impl PermissionEditBlocker {
  pub(crate) fn message(self) -> &'static str {
    match self {
      Self::SourceChanged | Self::PermissionRemoved => {
        "Restart the applet before changing its device permissions."
      }
      Self::InstallationUnavailable => {
        "The installed applet is unavailable. Review it in Library before changing its device permissions."
      }
    }
  }
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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
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

pub(crate) const GRANT_CHANGED: &str =
  "This permission changed. Review the current setting and try again.";

#[derive(Serialize)]
enum GrantTarget<'a> {
  Installed(&'a str),
  Unavailable(Option<&'a StoreSwitchSnapshot>, &'a str),
}

impl<'a> From<&'a UnavailableApplet> for GrantTarget<'a> {
  fn from(applet: &'a UnavailableApplet) -> Self {
    Self::Unavailable(applet.active_snapshot.as_ref(), &applet.error)
  }
}

fn grant_revision(
  applet_id: &str,
  permission: ManagedPermission,
  target: &GrantTarget<'_>,
  stored: Option<&Value>,
) -> Result<String, String> {
  // Including presence distinguishes an absent setting from a stored JSON null.
  let snapshot = serde_json::to_vec(&(applet_id, permission, target, stored.is_some(), stored))
    .map_err(|_| "Could not encode the reviewed permission setting.")?;
  Ok(digest(&snapshot))
}

fn permission_access(stored: Option<&Value>, source: Option<&GrantSource>) -> PermissionAccess {
  match stored {
    None => PermissionAccess::Ask,
    Some(value) if source.is_some_and(|source| *value == json!(source)) => {
      PermissionAccess::Remembered
    }
    Some(value) if source.is_some_and(|source| *value == blocked_value(source)) => {
      PermissionAccess::Blocked
    }
    Some(_) => PermissionAccess::PreviousPackage,
  }
}

fn saved_permission_mode(stored: &Value) -> Option<PermissionMode> {
  // Compare with the canonical grant so a partial or legacy record stays opaque.
  let source = GrantSource::deserialize(stored.get("source").unwrap_or(stored)).ok()?;
  match permission_access(Some(stored), Some(&source)) {
    PermissionAccess::Remembered => Some(PermissionMode::Remembered),
    PermissionAccess::Blocked => Some(PermissionMode::Blocked),
    _ => None,
  }
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
    let stored = self.saved_grant(permission, applet_id)?;
    Ok(permission_access(stored.as_ref(), Some(source)).mode())
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

  fn saved_grant(
    &self,
    permission: ManagedPermission,
    applet_id: &str,
  ) -> Result<Option<Value>, String> {
    let mut record =
      self
        .0
        .dispatch(applet_id, "config.get", json!({"key": permission.label()}))?;
    Ok(
      record
        .as_object_mut()
        .and_then(|record| record.remove("value")),
    )
  }

  fn checked_grant(
    &self,
    applet_id: &str,
    permission: ManagedPermission,
    target: &GrantTarget<'_>,
    expected: &str,
  ) -> Result<Option<Value>, String> {
    let current = self.saved_grant(permission, applet_id)?;
    if grant_revision(applet_id, permission, target, current.as_ref())? != expected {
      return Err(GRANT_CHANGED.into());
    }
    Ok(current)
  }

  pub(crate) fn set_reviewed_mode(
    &self,
    applet: &InstalledApplet,
    installed_revision: &str,
    expected_grant: &str,
    permission: ManagedPermission,
    mode: PermissionMode,
  ) -> Result<PermissionMode, String> {
    let applet_id = &applet.manifest.applet_id;
    self.checked_grant(
      applet_id,
      permission,
      &GrantTarget::Installed(installed_revision),
      expected_grant,
    )?;
    self.set_mode(permission, applet_id, &GrantSource::from(applet), mode)
  }

  pub(crate) fn reset_reviewed(
    &self,
    applet_id: &str,
    target: AppletReportEntry,
    permission: ManagedPermission,
    expected_grant: &str,
  ) -> Result<(), String> {
    let current = match target {
      AppletReportEntry::Installed(applet) => {
        let revision = installed_revision(&applet)?;
        self.checked_grant(
          applet_id,
          permission,
          &GrantTarget::Installed(&revision),
          expected_grant,
        )?
      }
      AppletReportEntry::Unavailable(applet) => self.checked_grant(
        applet_id,
        permission,
        &GrantTarget::from(&applet),
        expected_grant,
      )?,
      AppletReportEntry::Retained(_) => return Err(GRANT_CHANGED.into()),
    };
    if current.is_none() {
      return Err(GRANT_CHANGED.into());
    }
    self.forget(permission, applet_id)
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
    target: &GrantTarget<'_>,
  ) -> Result<Vec<GrantSummary>, String> {
    let mut grants = Vec::new();
    for permission in ManagedPermission::ALL {
      let stored = self.saved_grant(permission, applet_id)?;
      let access = permission_access(stored.as_ref(), source);
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
      let saved_mode = match access {
        PermissionAccess::UnverifiedPackage => stored.as_ref().and_then(saved_permission_mode),
        _ => None,
      };
      grants.push(GrantSummary {
        permission,
        access,
        grant_revision: grant_revision(applet_id, permission, target, stored.as_ref())?,
        mode: (declared && source.is_some()).then_some(access.mode()),
        saved_mode,
        available: permission.available(),
        system_authorization: SystemAuthorization::Unknown,
        can_open_system_settings: None,
      });
    }
    Ok(grants)
  }

  fn installed_summary(
    &self,
    applet: &InstalledApplet,
    restart_required: bool,
  ) -> Result<AppletPermissionSummary, String> {
    let revision = installed_revision(applet)?;
    let source = GrantSource::from(applet);
    let manifest = &applet.manifest;
    let grants = self.summary(
      &manifest.applet_id,
      Some(&source),
      &manifest.permissions,
      &GrantTarget::Installed(&revision),
    )?;
    Ok(AppletPermissionSummary {
      applet_id: manifest.applet_id.clone(),
      name: manifest.name.clone(),
      installed_revision: Some(revision),
      manifest: Some(PermissionManifest {
        network: manifest.network,
        permissions: manifest.permissions.clone(),
        has_lib: manifest.lib.is_some(),
      }),
      grants,
      restart_required,
    })
  }

  fn installed_info(
    &self,
    applet: InstalledApplet,
    restart_required: bool,
    read: impl FnMut(ManagedPermission) -> SystemAuthorization,
  ) -> Result<Value, String> {
    let mut permissions = self.installed_summary(&applet, restart_required)?;
    attach_system_authorizations(std::slice::from_mut(&mut permissions), read);
    Ok(json!({ "applet": applet, "permissions": permissions }))
  }

  fn unavailable_summary(
    &self,
    applet: UnavailableApplet,
  ) -> Result<AppletPermissionSummary, String> {
    let grants = self.summary(&applet.applet_id, None, &[], &GrantTarget::from(&applet))?;
    Ok(AppletPermissionSummary {
      applet_id: applet.applet_id.clone(),
      name: applet.applet_id,
      installed_revision: None,
      manifest: None,
      grants,
      restart_required: false,
    })
  }

  fn read_unavailable(
    &self,
    bundles: &BundleStore,
    applet_id: &str,
  ) -> Result<Option<AppletPermissionSummary>, String> {
    match bundles.report_applet(applet_id)? {
      Some(AppletReportEntry::Unavailable(applet)) => self.unavailable_summary(applet).map(Some),
      _ => Ok(None),
    }
  }

  fn summaries(
    &self,
    report: AppletListReport,
    pending_restarts: &HashSet<String>,
  ) -> Result<Vec<AppletPermissionSummary>, String> {
    let mut entries = Vec::new();
    for applet in report.applets {
      let restart_required = pending_restarts.contains(&applet.manifest.applet_id);
      entries.push(self.installed_summary(&applet, restart_required)?);
    }
    for applet in report.unavailable {
      let entry = self.unavailable_summary(applet)?;
      if !entry.grants.is_empty() {
        entries.push(entry);
      }
    }
    Ok(entries)
  }

  #[cfg(test)]
  pub(crate) fn reported_grant_revision(
    &self,
    report: AppletListReport,
    applet_id: &str,
    permission: ManagedPermission,
  ) -> String {
    self
      .summaries(report, &HashSet::new())
      .unwrap()
      .into_iter()
      .find(|entry| entry.applet_id == applet_id)
      .unwrap()
      .grants
      .into_iter()
      .find(|grant| grant.permission == permission)
      .unwrap()
      .grant_revision
  }
}

#[derive(Serialize)]
struct GrantSummary {
  permission: ManagedPermission,
  access: PermissionAccess,
  grant_revision: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  mode: Option<PermissionMode>,
  #[serde(skip_serializing_if = "Option::is_none")]
  saved_mode: Option<PermissionMode>,
  available: bool,
  system_authorization: SystemAuthorization,
  #[serde(skip_serializing_if = "Option::is_none")]
  can_open_system_settings: Option<bool>,
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
        grant.can_open_system_settings = authorization.can_open_system_settings().then_some(true);
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

pub(crate) async fn unavailable(host: &Host, applet_id: String) -> Result<Value, String> {
  let bundles = host.bundles.clone();
  let grants = host.grants.clone();
  crate::commands::blocking(move || Ok(json!(grants.read_unavailable(&bundles, &applet_id)?))).await
}

pub async fn installed_info(app: AppHandle, applet_id: String) -> Result<Value, String> {
  crate::commands::blocking(move || {
    let host = app.state::<Host>();
    let applet = host.bundles.installed(&applet_id)?;
    let restart_required = host.needs_applet_restart(&applet);
    host
      .grants
      .installed_info(applet, restart_required, |permission| {
        permission.system_authorization(&app)
      })
  })
  .await
}

pub fn publish_device_change(app: &AppHandle) {
  let _ = app.emit_to("main", "apell-permissions-changed", ());
}

#[cfg(test)]
mod tests {
  use super::*;
  use apell_core::bundles::{pack_directory, Manifest, StoreChannel, StoreSource, API_VERSION};
  use std::fs;

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
  fn installed_info_pairs_permissions_with_the_exact_store_installation() {
    let dir = tempfile::tempdir().unwrap();
    let grants = PermissionGrants::new(dir.path().into());
    let mut applet = installed(
      StoreSource {
        store_url: "https://store.example".into(),
        applet_id: "recorder".into(),
        channel: StoreChannel::Nightly,
      }
      .into(),
    );
    applet.manifest.version = "1.1.0-nightly.1".into();
    applet.manifest.network = false;
    let revision = installed_revision(&applet).unwrap();
    let source = GrantSource::from(&applet);
    grants
      .set_mode(
        ManagedPermission::Microphone,
        "recorder",
        &source,
        PermissionMode::Blocked,
      )
      .unwrap();

    let info = grants
      .installed_info(applet, true, |_| SystemAuthorization::Allowed)
      .unwrap();
    assert_eq!(
      info["applet"]["applet_id"],
      info["permissions"]["applet_id"]
    );
    assert_eq!(info["applet"]["version"], "1.1.0-nightly.1");
    assert_eq!(info["applet"]["source"]["channel"], "nightly");
    assert_eq!(info["permissions"]["installed_revision"], revision);
    assert_eq!(info["permissions"]["restart_required"], true);
    assert_eq!(info["permissions"]["manifest"]["network"], false);
    assert_eq!(info["permissions"]["grants"][0]["mode"], "blocked");
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
        grant_revision: "fixture".into(),
        mode: declared.then_some(access.mode()),
        saved_mode: None,
        available: true,
        system_authorization: SystemAuthorization::Unknown,
        can_open_system_settings: None,
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
    assert_eq!(
      summaries[0].grants[0].can_open_system_settings,
      SystemAuthorization::Blocked
        .can_open_system_settings()
        .then_some(true)
    );
    assert_eq!(summaries[2].grants[0].can_open_system_settings, None);
    attach_system_authorizations(&mut summaries, |_| SystemAuthorization::Allowed);
    assert_eq!(summaries[0].grants[0].can_open_system_settings, None);
  }

  #[test]
  fn permission_recovery_requires_an_actionable_system_status() {
    assert_eq!(
      SystemAuthorization::from_native("restricted"),
      SystemAuthorization::Restricted
    );
    for status in [
      SystemAuthorization::Restricted,
      SystemAuthorization::Allowed,
      SystemAuthorization::NotDetermined,
      SystemAuthorization::Unknown,
    ] {
      assert!(!status.can_open_system_settings());
    }
    assert_eq!(
      SystemAuthorization::NotGranted.can_open_system_settings(),
      cfg!(target_os = "android")
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
    let target = GrantTarget::Installed("fixture-package");
    let unverified = GrantTarget::Unavailable(None, "Fixture unavailable.");
    let saved = json!(source);
    let saved_revision = grant_revision(applet_id, Camera, &target, Some(&saved)).unwrap();
    let unverified_revision = grant_revision(applet_id, Camera, &unverified, Some(&saved)).unwrap();
    let ask_revision = grant_revision(applet_id, Location, &target, None).unwrap();
    grants.remember(Camera, applet_id, &source).unwrap();
    assert!(!grants.remembered(Location, applet_id, &source).unwrap());
    assert!(!grants.remembered(Microphone, applet_id, &source).unwrap());
    let sensors_available = cfg!(any(target_os = "ios", target_os = "android"));
    assert_eq!(
      json!(grants
        .summary(applet_id, Some(&updated), &["location".into()], &target)
        .unwrap()),
      json!([
        {"permission":"camera","access":"previous_package","grant_revision":saved_revision,"available":sensors_available,"system_authorization":"unknown"},
        {"permission":"location","access":"ask","grant_revision":ask_revision,"mode":"ask","available":sensors_available,"system_authorization":"unknown"},
      ]),
    );
    assert_eq!(
      json!(grants
        .summary(applet_id, Some(&source), &["camera".into()], &target)
        .unwrap()),
      json!([{"permission":"camera","access":"remembered","grant_revision":saved_revision,"mode":"remembered","available":sensors_available,"system_authorization":"unknown"}]),
    );
    assert_eq!(
      json!(grants
        .summary(applet_id, Some(&source), &[], &target)
        .unwrap()),
      json!([{"permission":"camera","access":"undeclared","grant_revision":saved_revision,"available":sensors_available,"system_authorization":"unknown"}]),
    );
    assert_eq!(
      json!(grants.summary(applet_id, None, &[], &unverified).unwrap()),
      json!([{"permission":"camera","access":"unverified_package","grant_revision":unverified_revision,"saved_mode":"remembered","available":sensors_available,"system_authorization":"unknown"}]),
    );
    grants.remember(Location, applet_id, &updated).unwrap();
    grants.forget(Camera, applet_id).unwrap();
    assert!(grants
      .summary(applet_id, Some(&source), &[], &target)
      .unwrap()
      .iter()
      .all(|grant| grant.permission != Camera));
    assert!(!grants.remembered(Camera, applet_id, &source).unwrap());
    assert!(grants.remembered(Location, applet_id, &updated).unwrap());
  }

  #[test]
  fn unavailable_lookup_reviews_only_the_current_damaged_applet() {
    use ManagedPermission::{Camera, Location, Microphone};

    let dir = tempfile::tempdir().unwrap();
    let bundle_dir = dir.path().join("bundles");
    let bundles = BundleStore::new(bundle_dir.clone());
    let grants = PermissionGrants::new(dir.path().join("grants"));
    let files = tempfile::tempdir().unwrap();
    let applet_id = "recorder";
    let read = || grants.read_unavailable(&bundles, applet_id).unwrap();
    fs::write(
      files.path().join("manifest.json"),
      json!({
        "schema_version": 1, "api_version": API_VERSION, "applet_id": applet_id,
        "name": "Recorder", "version": "1.0.0", "permissions": ["audio", "camera", "location"],
      })
      .to_string(),
    )
    .unwrap();
    fs::write(files.path().join("index.html"), "Recorder").unwrap();
    let bytes = pack_directory(files.path()).unwrap();
    let sha256 = digest(&bytes);

    assert!(read().is_none());
    let installed = bundles.install(&bytes, &sha256).unwrap();
    assert!(read().is_none());
    fs::remove_file(bundle_dir.join(applet_id).join(format!("{sha256}.apel"))).unwrap();
    assert_eq!(
      json!(read()),
      json!({
        "applet_id": applet_id, "name": applet_id, "manifest": null,
        "grants": [], "restart_required": false,
      }),
    );
    assert!(grants
      .summaries(bundles.list_report().unwrap(), &HashSet::new())
      .unwrap()
      .is_empty());

    let source = GrantSource::from(&installed);
    grants.remember(Microphone, applet_id, &source).unwrap();
    grants
      .set_mode(Camera, applet_id, &source, PermissionMode::Blocked)
      .unwrap();
    grants.store(Location, applet_id, json!(true)).unwrap();
    let entry = json!(read());
    let settings = grants
      .summaries(bundles.list_report().unwrap(), &HashSet::new())
      .unwrap();
    assert_eq!(entry, json!(settings[0]));
    let saved_grants = entry["grants"].as_array().unwrap();
    assert_eq!(saved_grants[0]["saved_mode"], "remembered");
    assert_eq!(saved_grants[1]["saved_mode"], "blocked");
    assert!(saved_grants[2].get("saved_mode").is_none());
    for grant in saved_grants {
      assert_eq!(grant["access"], "unverified_package");
      assert!(grant.get("mode").is_none());
      assert_eq!(grant["system_authorization"], "unknown");
      assert!(grant.get("can_open_system_settings").is_none());
    }

    let target = bundles.report_applet(applet_id).unwrap().unwrap();
    grants
      .reset_reviewed(
        applet_id,
        target,
        Location,
        saved_grants[2]["grant_revision"].as_str().unwrap(),
      )
      .unwrap();
    assert_eq!(read().unwrap().grants.len(), 2);
    let Some(AppletReportEntry::Unavailable(unavailable)) =
      bundles.report_applet(applet_id).unwrap()
    else {
      panic!("Expected a damaged package");
    };
    bundles
      .restore_package(applet_id, &unavailable.restore_snapshot.unwrap(), &bytes)
      .unwrap();
    assert!(read().is_none());
    assert_eq!(
      grants.mode(Camera, applet_id, &source).unwrap(),
      PermissionMode::Blocked,
    );
    bundles.remove(applet_id, || Ok(())).unwrap();
    assert!(read().is_none());
  }

  #[test]
  fn saved_permission_modes_require_the_exact_persisted_grant_shape() {
    for source in [
      GrantSource::Github {
        repository_id: 1,
        asset_name: "recorder.apel".into(),
        release_tag: None,
      },
      GrantSource::Github {
        repository_id: 1,
        asset_name: "recorder.apel".into(),
        release_tag: Some("nightly".into()),
      },
      GrantSource::Store {
        store_url: "https://store.example".into(),
        applet_id: "recorder".into(),
      },
      GrantSource::File {
        sha256: "a".repeat(64),
      },
    ] {
      assert_eq!(
        saved_permission_mode(&json!(source)),
        Some(PermissionMode::Remembered)
      );
      assert_eq!(
        saved_permission_mode(&blocked_value(&source)),
        Some(PermissionMode::Blocked)
      );
      assert_eq!(
        saved_permission_mode(&json!({"mode": "ask", "source": source})),
        None
      );
      assert_eq!(
        saved_permission_mode(&json!({"mode": "blocked", "source": source, "extra": true})),
        None
      );
    }
    for malformed in [
      json!(null),
      json!(true),
      json!({"repository_id":1,"asset_name":"recorder.apel","release_tag":null}),
      json!({"kind":"github","repository_id":1,"asset_name":"recorder.apel"}),
      json!({"kind":"file","sha256":1}),
      json!({"mode":"blocked","source":{"kind":"file","sha256":1}}),
    ] {
      assert_eq!(saved_permission_mode(&malformed), None);
    }
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
    let target = GrantTarget::Installed("fixture-package");
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
      .summary(applet_id, Some(&source), &declared, &target)
      .unwrap();
    let replacement_summary = reopened
      .summary(applet_id, Some(&replacement), &declared, &target)
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
