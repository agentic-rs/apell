use serde::Deserialize;
use std::{
  ffi::{OsStr, OsString},
  io::{Cursor, Read},
  path::{Path, PathBuf},
};
use url::{Host, Url};

pub const MAX_METADATA_BYTES: usize = 16 * 1024;
pub const MAX_ICON_BYTES: usize = 256 * 1024;

pub struct PublishArgs {
  pub package: PathBuf,
  pub metadata: PathBuf,
  pub icon: Option<PathBuf>,
  pub endpoint: Url,
}

impl PublishArgs {
  pub fn parse(arguments: &[OsString]) -> Result<Self, String> {
    let Some(package) = arguments.first() else {
      return Err(super::USAGE.into());
    };
    if Path::new(package).extension() != Some(OsStr::new("apel")) {
      return Err(super::USAGE.into());
    }
    let mut store = None;
    let mut metadata = None;
    let mut icon = None;
    for pair in arguments[1..].chunks(2) {
      let [flag, value] = pair else {
        return Err(super::USAGE.into());
      };
      if value.is_empty() || value.to_string_lossy().starts_with("--") {
        return Err(super::USAGE.into());
      }
      match flag.to_str() {
        Some("--store") if store.is_none() => {
          store = Some(store_endpoint(
            value.to_str().ok_or("Invalid store address.")?,
          )?);
        }
        Some("--metadata") if metadata.is_none() => metadata = Some(PathBuf::from(value)),
        Some("--icon") if icon.is_none() => icon = Some(PathBuf::from(value)),
        _ => return Err(super::USAGE.into()),
      }
    }
    Ok(Self {
      package: package.into(),
      endpoint: store.ok_or(super::USAGE)?,
      metadata: metadata.ok_or(super::USAGE)?,
      icon,
    })
  }
}

pub fn store_endpoint(value: &str) -> Result<Url, String> {
  const ERROR: &str = "Use a root HTTPS store URL without credentials, query, or fragment. HTTP is allowed only for loopback development.";
  if value.len() > 2_048
    || value
      .chars()
      .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
  {
    return Err(ERROR.into());
  }
  let mut url = Url::parse(value).map_err(|_| ERROR)?;
  let remainder = value.split_once("://").ok_or(ERROR)?.1;
  let (authority, path) = remainder.split_once('/').unwrap_or((remainder, ""));
  let local = match url.host() {
    Some(Host::Domain(host)) => host == "localhost",
    Some(Host::Ipv4(ip)) => ip.is_loopback(),
    Some(Host::Ipv6(ip)) => ip.is_loopback(),
    None => false,
  };
  if url.host_str().is_none()
    || authority.contains('@')
    || !path.is_empty()
    || !url.username().is_empty()
    || url.password().is_some()
    || url.query().is_some()
    || url.fragment().is_some()
    || !(url.scheme() == "https" || url.scheme() == "http" && local)
  {
    return Err(ERROR.into());
  }
  url.set_path("/api/v1/publish");
  Ok(url)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishMetadata {
  description: String,
  #[serde(default)]
  project_url: Option<String>,
  #[serde(default)]
  release_notes: String,
}

impl PublishMetadata {
  pub fn validate(&self) -> Result<(), String> {
    if self.description.trim().is_empty()
      || self.description.len() > 2_000
      || self.description.chars().any(char::is_control)
    {
      return Err("Description must contain 1–2,000 bytes without control characters.".into());
    }
    if self.release_notes.len() > 10_000
      || self
        .release_notes
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
      return Err(
        "Release notes exceed 10,000 bytes or contain unsupported control characters.".into(),
      );
    }
    if let Some(value) = &self.project_url {
      let url = Url::parse(value).map_err(|_| "Project URL must be an HTTPS URL.")?;
      if value.len() > 2_048
        || value.chars().any(char::is_control)
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
      {
        return Err("Project URL must use HTTPS and cannot include credentials.".into());
      }
    }
    Ok(())
  }
}

pub fn read_bounded(file: &Path, limit: usize, label: &str) -> Result<Vec<u8>, String> {
  let source = std::fs::File::open(file).map_err(|_| format!("Could not read {label} file."))?;
  let metadata = source
    .metadata()
    .map_err(|_| format!("Could not inspect {label} file."))?;
  if !metadata.is_file() || metadata.len() > limit as u64 {
    return Err(format!(
      "{label} must be a regular file of at most {limit} bytes."
    ));
  }
  let mut bytes = Vec::new();
  source
    .take(limit as u64 + 1)
    .read_to_end(&mut bytes)
    .map_err(|_| format!("Could not read {label} file."))?;
  if bytes.len() > limit {
    return Err(format!("{label} exceeds {limit} bytes."));
  }
  Ok(bytes)
}

pub fn read_token(file: &Path) -> Result<String, String> {
  let bytes = read_bounded(file, 514, "Publishing token")?;
  let value = std::str::from_utf8(&bytes).map_err(|_| "Invalid publishing token.")?;
  let token = value.trim_end_matches(['\n', '\r']);
  if !(32..=512).contains(&token.len()) || !token.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
  {
    return Err("Publishing token must contain 32–512 non-space printable ASCII bytes.".into());
  }
  Ok(token.into())
}

pub fn validate_icon(bytes: &[u8]) -> Result<(), String> {
  let mut decoder = png::Decoder::new(Cursor::new(bytes));
  decoder.set_limits(png::Limits {
    bytes: 8 * 1024 * 1024,
  });
  let mut reader = decoder
    .read_info()
    .map_err(|_| "Icon must be a valid PNG image.")?;
  let info = reader.info();
  if info.width == 0
    || info.height == 0
    || info.width > 1024
    || info.height > 1024
    || info.animation_control.is_some()
  {
    return Err("Icon must be a still PNG no larger than 1,024 × 1,024 pixels.".into());
  }
  let size = reader.output_buffer_size();
  if size > 8 * 1024 * 1024 {
    return Err("Icon decoded content is too large.".into());
  }
  let mut decoded = vec![0; size];
  reader
    .next_frame(&mut decoded)
    .map_err(|_| "Icon must be a valid PNG image.")?;
  reader
    .finish()
    .map_err(|_| "Icon must be a valid PNG image.")?;
  Ok(())
}
