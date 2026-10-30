use crate::{bundles::atomic_write, valid_name};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::{io::Read, path::PathBuf, sync::Mutex};

const MAX_VALUE_BYTES: usize = 64 * 1024;
const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_KEYS: usize = 1000;

/// Small non-secret JSON preferences. Kept outside the applet's file directory.
pub struct ConfigStore {
  root: PathBuf,
  gate: Mutex<()>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyRequest {
  key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetRequest {
  key: String,
  value: Value,
}

impl ConfigStore {
  pub fn new(root: PathBuf) -> Self {
    Self {
      root,
      gate: Mutex::new(()),
    }
  }

  pub fn dispatch(&self, applet_id: &str, method: &str, params: Value) -> Result<Value, String> {
    if !valid_name(applet_id) {
      return Err("Invalid applet identifier.".into());
    }
    let _guard = self
      .gate
      .lock()
      .map_err(|_| "Configuration storage is unavailable.")?;
    let (key, value) = match method {
      "config.set" => {
        let request: SetRequest =
          serde_json::from_value(params).map_err(|_| "Invalid config.set parameters.")?;
        let encoded =
          serde_json::to_vec(&request.value).map_err(|_| "Invalid JSON configuration value.")?;
        if encoded.len() > MAX_VALUE_BYTES {
          return Err("A configuration value may not exceed 64 KiB.".into());
        }
        (request.key, Some(request.value))
      }
      "config.get" | "config.delete" => {
        let request: KeyRequest =
          serde_json::from_value(params).map_err(|_| "Invalid configuration parameters.")?;
        (request.key, None)
      }
      _ => return Err("Unknown configuration operation.".into()),
    };
    if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
      return Err("Configuration keys must contain 1–128 bytes and no control characters.".into());
    }
    let path = self.root.join(format!("{applet_id}.json"));
    let mut values = self.load(&path)?;
    match method {
      "config.get" => Ok(match values.get(&key) {
        Some(value) => json!({"found":true,"value":value}),
        None => json!({"found":false}),
      }),
      "config.set" => {
        values.insert(key, value.expect("set requests always contain a value"));
        self.save(&path, &values)?;
        Ok(Value::Null)
      }
      "config.delete" => {
        let deleted = values.remove(&key).is_some();
        if deleted {
          self.save(&path, &values)?;
        }
        Ok(json!({"deleted":deleted}))
      }
      _ => unreachable!(),
    }
  }

  fn load(&self, path: &std::path::Path) -> Result<Map<String, Value>, String> {
    match std::fs::symlink_metadata(&self.root) {
      Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
        return Err("Invalid configuration storage directory.".into())
      }
      Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
        return Err("Could not access configuration storage.".into())
      }
      _ => {}
    }
    match std::fs::symlink_metadata(path) {
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
      Ok(metadata)
        if metadata.is_file()
          && !metadata.file_type().is_symlink()
          && metadata.len() <= MAX_CONFIG_BYTES as u64 => {}
      _ => return Err("Invalid configuration file.".into()),
    }
    let file = std::fs::File::open(path).map_err(|_| "Could not read configuration.")?;
    let mut bytes = Vec::new();
    file
      .take(MAX_CONFIG_BYTES as u64 + 1)
      .read_to_end(&mut bytes)
      .map_err(|_| "Could not read configuration.")?;
    if bytes.len() > MAX_CONFIG_BYTES {
      return Err("Configuration exceeds its storage limit.".into());
    }
    serde_json::from_slice(&bytes)
      .map_err(|_| "Saved configuration is invalid JSON. It was preserved.".into())
  }

  fn save(&self, path: &std::path::Path, values: &Map<String, Value>) -> Result<(), String> {
    if values.len() > MAX_KEYS {
      return Err("An applet may store at most 1000 configuration keys.".into());
    }
    let bytes = serde_json::to_vec(values).map_err(|_| "Could not encode configuration.")?;
    if bytes.len() > MAX_CONFIG_BYTES {
      return Err("Applet configuration may not exceed 1 MiB.".into());
    }
    atomic_write(path, &bytes)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn json_values_persist_with_scope_and_null_is_distinct_from_missing() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(dir.path().to_owned());
    let value = json!({"theme":"dark","items":[1,true,null]});
    store
      .dispatch(
        "one",
        "config.set",
        json!({"key":"preferences","value":value}),
      )
      .unwrap();
    store
      .dispatch("one", "config.set", json!({"key":"null","value":null}))
      .unwrap();
    assert_eq!(
      store
        .dispatch("two", "config.get", json!({"key":"preferences"}))
        .unwrap(),
      json!({"found":false})
    );
    drop(store);
    let reopened = ConfigStore::new(dir.path().to_owned());
    assert_eq!(
      reopened
        .dispatch("one", "config.get", json!({"key":"preferences"}))
        .unwrap(),
      json!({"found":true,"value":value})
    );
    assert_eq!(
      reopened
        .dispatch("one", "config.get", json!({"key":"null"}))
        .unwrap(),
      json!({"found":true,"value":null})
    );
    assert_eq!(
      reopened
        .dispatch("one", "config.delete", json!({"key":"preferences"}))
        .unwrap(),
      json!({"deleted":true})
    );
    assert_eq!(
      reopened
        .dispatch("one", "config.delete", json!({"key":"preferences"}))
        .unwrap(),
      json!({"deleted":false})
    );
  }

  #[test]
  fn invalid_updates_preserve_saved_values() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(dir.path().to_owned());
    store
      .dispatch("one", "config.set", json!({"key":"saved","value":42}))
      .unwrap();
    for (id, method, params) in [
      ("../escape", "config.get", json!({"key":"saved"})),
      (
        "one",
        "config.set",
        json!({"key":"saved","value":"x".repeat(MAX_VALUE_BYTES)}),
      ),
      ("one", "config.set", json!({"key":"saved"})),
      ("one", "config.set", json!({"key":"","value":0})),
      (
        "one",
        "config.get",
        json!({"key":"saved","applet_id":"two"}),
      ),
    ] {
      assert!(store.dispatch(id, method, params).is_err());
    }
    assert_eq!(
      store
        .dispatch("one", "config.get", json!({"key":"saved"}))
        .unwrap()["value"],
      42
    );
    std::fs::write(dir.path().join("one.json"), b"broken").unwrap();
    assert!(store
      .dispatch("one", "config.set", json!({"key":"saved","value":1}))
      .is_err());
    assert_eq!(
      std::fs::read(dir.path().join("one.json")).unwrap(),
      b"broken"
    );
  }

  #[test]
  fn concurrent_writes_do_not_lose_independent_keys() {
    let dir = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(ConfigStore::new(dir.path().to_owned()));
    let workers: Vec<_> = (0..16)
      .map(|index| {
        let store = store.clone();
        std::thread::spawn(move || {
          store
            .dispatch(
              "one",
              "config.set",
              json!({"key":format!("key_{index}"),"value":index}),
            )
            .unwrap()
        })
      })
      .collect();
    for worker in workers {
      worker.join().unwrap();
    }
    for index in 0..16 {
      assert_eq!(
        store
          .dispatch("one", "config.get", json!({"key":format!("key_{index}")}))
          .unwrap()["value"],
        index
      );
    }
  }

  #[test]
  fn config_limits_preserve_existing_keys_and_allow_recovery_by_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new(dir.path().to_owned());
    let path = dir.path().join("one.json");
    let mut values = Map::new();
    for index in 0..MAX_KEYS {
      values.insert(format!("key_{index}"), json!(index));
    }
    store.save(&path, &values).unwrap();
    assert!(store
      .dispatch("one", "config.set", json!({"key":"extra","value":0}))
      .is_err());
    store
      .dispatch(
        "one",
        "config.set",
        json!({"key":"key_0","value":"updated"}),
      )
      .unwrap();
    assert_eq!(store.load(&path).unwrap().len(), MAX_KEYS);
    store
      .dispatch("one", "config.delete", json!({"key":"key_0"}))
      .unwrap();
    store
      .dispatch("one", "config.set", json!({"key":"extra","value":0}))
      .unwrap();
    values.clear();
    for index in 0..16 {
      values.insert(
        format!("key_{index}"),
        json!("x".repeat(MAX_VALUE_BYTES - 2)),
      );
    }
    let previous = std::fs::read(&path).unwrap();
    assert!(store.save(&path, &values).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), previous);
  }

  #[cfg(unix)]
  #[test]
  fn config_rejects_linked_directories_and_files() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("one.json"), br#"{"saved":42}"#).unwrap();
    let root = dir.path().join("config");
    symlink(&outside, &root).unwrap();
    let store = ConfigStore::new(root.clone());
    for method in ["config.get", "config.set", "config.delete"] {
      let params = if method == "config.set" {
        json!({"key":"saved","value":0})
      } else {
        json!({"key":"saved"})
      };
      assert!(store.dispatch("one", method, params).is_err());
    }
    std::fs::remove_file(&root).unwrap();
    std::fs::create_dir(&root).unwrap();
    symlink(outside.join("one.json"), root.join("one.json")).unwrap();
    assert!(store
      .dispatch("one", "config.set", json!({"key":"saved","value":0}))
      .is_err());
    assert_eq!(
      std::fs::read(outside.join("one.json")).unwrap(),
      br#"{"saved":42}"#
    );
  }
}
