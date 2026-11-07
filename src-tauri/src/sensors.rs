//! Sensor requests belong to a view. Native image writers use host staging
//! until the active session can publish a validated image to its private files.
use crate::{
  commands::blocking,
  host::{Host, Session},
  permissions::ManagedPermission,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
  fs::{self, File},
  io::Read,
  path::{Path, PathBuf},
};
use tauri::AppHandle;
use tauri_plugin_apell_sensors::Command;
use tokio::sync::Mutex;
use uuid::Uuid;

const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

pub struct SensorService {
  gate: Mutex<()>,
  staging: PathBuf,
}

impl SensorService {
  pub fn new(staging: PathBuf) -> Result<Self, String> {
    if tauri_plugin_apell_sensors::available() {
      // A process that was killed during capture may have left a staging file.
      match fs::remove_dir_all(&staging) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("Could not clear unfinished sensor files.".into()),
      }
      fs::create_dir(&staging).map_err(|_| "Could not prepare sensor storage.")?;
    }
    Ok(Self {
      gate: Mutex::new(()),
      staging,
    })
  }

  pub fn edit_grant<T>(&self, edit: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let _operation = self
      .gate
      .try_lock()
      .map_err(|_| "Wait for the current camera, photo, or location request to finish.")?;
    edit()
  }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
  Camera,
  Photos,
  Location,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PhotoFormat {
  Jpeg,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PhotoOptions {
  output_format: Option<PhotoFormat>,
}

impl Kind {
  fn permission(self) -> Option<ManagedPermission> {
    match self {
      Self::Camera => Some(ManagedPermission::Camera),
      Self::Photos => None,
      Self::Location => Some(ManagedPermission::Location),
    }
  }

  fn writes_image(self) -> bool {
    matches!(self, Self::Camera | Self::Photos)
  }
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Outcome {
  Cancelled,
  Image {
    remember: bool,
  },
  Location {
    remember: bool,
    #[serde(flatten)]
    fix: LocationFix,
  },
}

#[derive(Deserialize, Serialize)]
struct LocationFix {
  latitude: f64,
  longitude: f64,
  accuracy_m: f64,
  timestamp_ms: u64,
}

impl LocationFix {
  fn validate(&self) -> Result<(), String> {
    if !(-90.0..=90.0).contains(&self.latitude)
      || !(-180.0..=180.0).contains(&self.longitude)
      || !self.accuracy_m.is_finite()
      || self.accuracy_m < 0.0
      || !(1..=9_007_199_254_740_991).contains(&self.timestamp_ms)
    {
      return Err("The device returned an invalid location.".into());
    }
    Ok(())
  }
}

fn image_format(header: &[u8; 12]) -> Option<(&'static str, &'static str)> {
  if header.starts_with(&[0xff, 0xd8, 0xff]) {
    Some(("jpg", "image/jpeg"))
  } else if header.starts_with(b"\x89PNG\r\n\x1a\n") {
    Some(("png", "image/png"))
  } else if header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a") {
    Some(("gif", "image/gif"))
  } else if header.starts_with(b"RIFF") && &header[8..] == b"WEBP" {
    Some(("webp", "image/webp"))
  } else if &header[4..8] == b"ftyp" {
    match &header[8..] {
      b"heic" | b"heix" => Some(("heic", "image/heic")),
      b"mif1" => Some(("heif", "image/heif")),
      b"avif" | b"avis" => Some(("avif", "image/avif")),
      _ => None,
    }
  } else {
    None
  }
}

fn inspect_image(staged: &Path) -> Result<(&'static str, &'static str, u64), String> {
  let metadata = fs::symlink_metadata(staged).map_err(|_| "The native image is unavailable.")?;
  let size_bytes = metadata.len();
  if !metadata.is_file() || !(12..=MAX_IMAGE_BYTES).contains(&size_bytes) {
    return Err("The native image is invalid or exceeds 32 MiB.".into());
  }
  let mut header = [0; 12];
  File::open(staged)
    .and_then(|mut file| file.read_exact(&mut header))
    .map_err(|_| "Could not read the native image.")?;
  let (extension, mime_type) = image_format(&header).ok_or("Unsupported image format.")?;
  Ok((extension, mime_type, size_bytes))
}

async fn run_native(
  app: AppHandle,
  session: &Session,
  identity: Value,
  args: Value,
) -> Result<Outcome, String> {
  let mut cancellation = session.cancellation.subscribe();
  if *cancellation.borrow() {
    return Err("Applet session is closed.".into());
  }
  let runner = app.clone();
  let mut operation = std::pin::pin!(blocking(move || {
    tauri_plugin_apell_sensors::call(&runner, Command::Run, args)
  }));
  let result = tokio::select! {
    biased;
    _ = cancellation.changed() => {
      // Poll the worker before awaiting cancel so either native ordering can
      // settle. The plugin retains a matching cancel that arrives before run.
      let _ = tokio::join!(
        &mut operation,
        blocking(move || tauri_plugin_apell_sensors::call(&app, Command::Cancel, identity)),
      );
      return Err("Applet session is closed.".into());
    }
    response = &mut operation => response?,
  };
  serde_json::from_value(result).map_err(|_| "The device returned an invalid response.".into())
}

pub async fn dispatch(
  app: AppHandle,
  host: &Host,
  view_id: &str,
  session: Session,
  method: &str,
  params: Value,
) -> Result<Value, String> {
  let kind = match method {
    "camera.capture" => Kind::Camera,
    "photos.pick" => Kind::Photos,
    "location.current" => Kind::Location,
    _ => return Err("Unknown sensor operation.".into()),
  };
  let output_format = if matches!(kind, Kind::Photos) {
    serde_json::from_value::<PhotoOptions>(params)
      .map_err(|_| "Invalid photo picker options.")?
      .output_format
  } else {
    if params != json!({}) {
      return Err("This request does not take parameters.".into());
    }
    None
  };
  if !tauri_plugin_apell_sensors::available() {
    return Err("Camera, photo library, and location require iOS or Android.".into());
  }
  if kind.writes_image()
    && !session
      .manifest
      .permissions
      .iter()
      .any(|group| group == "fs")
  {
    return Err("Declare app files (fs) to receive an image.".into());
  }

  let _operation = host
    .sensors
    .gate
    .try_lock()
    .map_err(|_| "A camera, photo, or location request is already running.")?;
  let _modal = host
    .native_prompt
    .try_lock()
    .map_err(|_| "Finish the current native prompt and try again.")?;
  let permission = kind.permission();
  let source = permission
    .map(|_| host.view_grant_source(view_id))
    .transpose()?;
  let grants = host.grants.clone();
  let applet_id = session.manifest.applet_id.clone();
  let staging_root = host.sensors.staging.clone();
  let (remembered, staging) = blocking({
    let applet_id = applet_id.clone();
    let source = source.clone();
    let grants = grants.clone();
    let session = session.clone();
    move || {
      session.run_local(|| {
        let remembered = match (permission, &source) {
          (Some(permission), Some(source)) => grants.remembered(permission, &applet_id, source)?,
          _ => false,
        };
        let staging = kind
          .writes_image()
          .then(|| tempfile::tempdir_in(staging_root))
          .transpose()
          .map_err(|_| "Could not prepare a private image.")?;
        Ok((remembered, staging))
      })
    }
  })
  .await?;

  let staged = staging
    .as_ref()
    .map(|directory| directory.path().join("image"));
  let identity = json!({"session_id":view_id,"operation_id":Uuid::new_v4().to_string()});
  let mut args = identity.clone();
  args["kind"] = json!(kind);
  args["applet_name"] = json!(session.manifest.name);
  args["remembered"] = json!(remembered);
  args["max_image_bytes"] = json!(MAX_IMAGE_BYTES);
  if let Some(output_format) = output_format {
    args["output_format"] = json!(output_format);
  }
  if let Some(path) = &staged {
    args["staging_path"] = json!(path);
  }
  let outcome = run_native(app, &session, identity, args).await?;
  let files = host.files.clone();
  blocking(move || {
    let _staging = staging;
    session.run_local(|| {
      let save = |remember| {
        if let (true, Some(permission), Some(source)) = (remember, permission, &source) {
          grants.remember(permission, &applet_id, source)?;
        }
        Ok::<(), String>(())
      };
      match (kind, outcome) {
        (_, Outcome::Cancelled) => Ok(Value::Null),
        (Kind::Camera | Kind::Photos, Outcome::Image { remember }) => {
          let staged = staged.ok_or("The native image is unavailable.")?;
          let (extension, mime_type, size_bytes) = inspect_image(&staged)?;
          if matches!(kind, Kind::Camera) && extension != "jpg" {
            return Err("The camera returned an invalid image.".into());
          }
          if output_format.is_some() && extension != "jpg" {
            return Err("The photo could not be converted to JPEG.".into());
          }
          let path = format!("images/{}.{extension}", Uuid::new_v4());
          files.import_file(&applet_id, &path, &staged)?;
          if let Err(error) = save(remember) {
            let _ = files.dispatch(&applet_id, "fs.remove", json!({"path":path}));
            return Err(error);
          }
          Ok(json!({"path":path,"mime_type":mime_type,"size_bytes":size_bytes}))
        }
        (Kind::Location, Outcome::Location { remember, fix }) => {
          fix.validate()?;
          save(remember)?;
          Ok(json!(fix))
        }
        _ => Err("The device returned an invalid response.".into()),
      }
    })
  })
  .await
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn photo_output_format_is_optional_and_rejects_unknown_options() {
    assert!(serde_json::from_value::<PhotoOptions>(json!({}))
      .unwrap()
      .output_format
      .is_none());
    let options: PhotoOptions = serde_json::from_value(json!({"output_format":"jpeg"})).unwrap();
    assert!(matches!(options.output_format, Some(PhotoFormat::Jpeg)));
    assert!(serde_json::from_value::<PhotoOptions>(json!({"output_format":"heic"})).is_err());
    assert!(serde_json::from_value::<PhotoOptions>(json!({"format":"jpeg"})).is_err());
  }

  #[test]
  fn staged_images_are_bounded_and_must_have_a_supported_signature() {
    let directory = tempfile::tempdir().unwrap();
    let staged = directory.path().join("image");
    fs::write(&staged, b"\0\0\0\x18ftypheic").unwrap();
    assert_eq!(inspect_image(&staged).unwrap(), ("heic", "image/heic", 12));
    File::options()
      .write(true)
      .open(&staged)
      .unwrap()
      .set_len(MAX_IMAGE_BYTES + 1)
      .unwrap();
    assert!(inspect_image(&staged).is_err());
    fs::write(&staged, b"<svg></svg>\n").unwrap();
    assert!(inspect_image(&staged).is_err());
  }

  #[test]
  fn location_wire_shape_preserves_approximate_accuracy_and_rejects_invalid_fixes() {
    let outcome: Outcome = serde_json::from_value(json!({
      "status":"location", "remember":false, "latitude":10.5, "longitude":20,
      "accuracy_m":1800, "timestamp_ms":1700000000000u64
    }))
    .unwrap();
    let Outcome::Location { mut fix, .. } = outcome else {
      panic!("Expected a location fix");
    };
    fix.validate().unwrap();
    assert_eq!(json!(fix)["accuracy_m"], 1800.0);
    fix.latitude = 91.0;
    assert!(fix.validate().is_err());
  }
}
