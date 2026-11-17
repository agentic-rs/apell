use apell_core::{bundles::atomic_write, store_client::normalize_store_url};
use serde::{Deserialize, Serialize};
use std::{io::Read, path::Path};

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
  pub store_url: Option<String>,
}

pub fn load(root: &Path) -> Result<StoreConfig, String> {
  let file = match std::fs::File::open(root.join("store-settings.json")) {
    Ok(file) => file,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(StoreConfig::default()),
    Err(_) => return Err("Could not read store settings.".into()),
  };
  let mut bytes = Vec::new();
  file
    .take(4097)
    .read_to_end(&mut bytes)
    .map_err(|_| "Could not read store settings.")?;
  if bytes.len() > 4096 {
    return Err("Saved store settings exceed their size limit.".into());
  }
  let mut settings: StoreConfig = serde_json::from_slice(&bytes)
    .map_err(|_| "Saved store settings are invalid. Enter the store address again.")?;
  if let Some(value) = &settings.store_url {
    settings.store_url = Some(normalize_store_url(value)?);
  }
  Ok(settings)
}

pub fn save(root: &Path, settings: &StoreConfig) -> Result<(), String> {
  let bytes = serde_json::to_vec(settings).map_err(|_| "Could not encode store settings.")?;
  atomic_write(&root.join("store-settings.json"), &bytes)
}
