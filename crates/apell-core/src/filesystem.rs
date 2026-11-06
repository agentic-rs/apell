//! A private filesystem for each applet, separate from host-owned storage.
use crate::{bundles::atomic_write, valid_name};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
  fs::{self, File, Metadata},
  io::{Read, Seek, SeekFrom},
  path::{Path, PathBuf},
  sync::Mutex,
};

pub const MAX_READ_BYTES: usize = 1024 * 1024;
pub const MAX_WRITE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_LIST_ENTRIES: usize = 1000;
const MAX_PATH_BYTES: usize = 4096;
const DEFAULT_READ_BYTES: usize = 64 * 1024;

type Result<T> = std::result::Result<T, String>;

pub struct FileStore {
  root: PathBuf,
  operations: Mutex<()>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadRequest {
  path: String,
  #[serde(default)]
  offset: u64,
  #[serde(default = "default_read_length")]
  length: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteRequest {
  path: String,
  bytes: Vec<u8>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathRequest {
  path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListRequest {
  #[serde(default)]
  path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameRequest {
  from_path: String,
  to_path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
  File,
  Directory,
}

#[derive(Serialize)]
struct FileInfo {
  kind: Kind,
  size_bytes: u64,
}

#[derive(Serialize)]
struct Entry {
  name: String,
  #[serde(flatten)]
  info: FileInfo,
}

impl FileStore {
  /// The host supplies this private root; its parent directory must exist.
  pub fn new(root: PathBuf) -> Self {
    Self {
      root,
      operations: Mutex::new(()),
    }
  }

  /// Resolve a host-only path without exposing private roots to an applet.
  /// Existing paths and every parent reject links and special files. A caller
  /// creating a new file must request a fresh destination, never overwrite one.
  pub fn native_path(&self, applet_id: &str, path: &str, create: bool) -> Result<PathBuf> {
    if !valid_name(applet_id) {
      return Err("Invalid applet identity.".into());
    }
    let _operation = self
      .operations
      .lock()
      .map_err(|_| "Applet filesystem unavailable.")?;
    let components = validate_path(path, false)?;
    let root = self.applet_root(applet_id)?;
    if create {
      let path = destination(&root, &components)?;
      if metadata_if_present(&path)?.is_some() {
        return Err("The destination already exists.".into());
      }
      Ok(path)
    } else {
      resolve(&root, &components).map(|(path, _)| path)
    }
  }

  /// Call on a blocking worker. One lock serializes path validation and mutations
  /// so another applet API call cannot replace a checked directory mid-operation.
  pub fn dispatch(&self, applet_id: &str, method: &str, params: Value) -> Result<Value> {
    if !valid_name(applet_id) {
      return Err("Invalid applet identity.".into());
    }
    let _operation = self
      .operations
      .lock()
      .map_err(|_| "The applet filesystem is unavailable.".to_string())?;
    match method {
      "fs.read" => {
        let request: ReadRequest = decode(params)?;
        let components = validate_path(&request.path, false)?;
        if request.length > MAX_READ_BYTES {
          return Err("File reads are limited to 1 MiB per request.".into());
        }
        let root = self.applet_root(applet_id)?;
        let (path, metadata) = resolve(&root, &components)?;
        if !metadata.is_file() {
          return Err("Only regular files can be read.".into());
        }
        let mut file = File::open(path).map_err(|_| "Could not open the applet file.")?;
        let metadata = file
          .metadata()
          .map_err(|_| "Could not inspect the applet file.")?;
        if !metadata.is_file() {
          return Err("Only regular files can be read.".into());
        }
        let file_size = metadata.len();
        if request.offset >= file_size {
          return Ok(json!({ "bytes": [], "eof": true }));
        }
        file
          .seek(SeekFrom::Start(request.offset))
          .map_err(|_| "Could not seek the applet file.")?;
        let mut bytes = Vec::new();
        file
          .take(request.length as u64)
          .read_to_end(&mut bytes)
          .map_err(|_| "Could not read the applet file.")?;
        let eof = request.offset.saturating_add(bytes.len() as u64) >= file_size
          || bytes.len() < request.length;
        Ok(json!({ "bytes": bytes, "eof": eof }))
      }
      "fs.write" => {
        let request: WriteRequest = decode(params)?;
        let components = validate_path(&request.path, false)?;
        if request.bytes.len() > MAX_WRITE_BYTES {
          return Err("Whole-file writes are limited to 16 MiB.".into());
        }
        let root = self.applet_root(applet_id)?;
        let path = destination(&root, &components)?;
        if let Some(metadata) = metadata_if_present(&path)? {
          if !metadata.is_file() {
            return Err("A file cannot replace a directory.".into());
          }
        }
        // This helper stages and syncs bytes, renames atomically, and removes its
        // temporary file. The service lock keeps staging files private to a call.
        atomic_write(&path, &request.bytes)?;
        Ok(json!({ "size_bytes": request.bytes.len() as u64 }))
      }
      "fs.list" => {
        let request: ListRequest = decode(params)?;
        let components = validate_path(&request.path, true)?;
        let root = self.applet_root(applet_id)?;
        let (path, metadata) = resolve(&root, &components)?;
        if !metadata.is_dir() {
          return Err("Only directories can be listed.".into());
        }
        let directory = fs::read_dir(path).map_err(|_| "Could not list the applet directory.")?;
        let mut entries = Vec::new();
        for entry in directory {
          if entries.len() >= MAX_LIST_ENTRIES {
            return Err("Directory listings are limited to 1000 entries.".into());
          }
          let entry = entry.map_err(|_| "Could not inspect a directory entry.")?;
          let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "The directory contains an unsupported filename.")?;
          validate_path(&name, false)?;
          let metadata = fs::symlink_metadata(entry.path())
            .map_err(|_| "Could not inspect a directory entry.")?;
          entries.push(Entry {
            name,
            info: file_info(&metadata)?,
          });
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(json!({ "entries": entries }))
      }
      "fs.stat" => {
        let request: PathRequest = decode(params)?;
        let components = validate_path(&request.path, true)?;
        let root = self.applet_root(applet_id)?;
        let (_, metadata) = resolve(&root, &components)?;
        serde_json::to_value(file_info(&metadata)?)
          .map_err(|_| "Could not encode file information.".into())
      }
      "fs.mkdir" => {
        let request: PathRequest = decode(params)?;
        let components = validate_path(&request.path, false)?;
        let mut path = self.applet_root(applet_id)?;
        for component in components {
          path.push(component);
          ensure_directory(&path)?;
        }
        Ok(Value::Null)
      }
      "fs.remove" => {
        let request: PathRequest = decode(params)?;
        let components = validate_path(&request.path, false)?;
        let root = self.applet_root(applet_id)?;
        let (path, metadata) = resolve(&root, &components)?;
        if metadata.is_dir() {
          fs::remove_dir(&path).map_err(|_| "Could not remove directory; it must be empty.")?;
        } else {
          fs::remove_file(&path).map_err(|_| "Could not remove the applet file.")?;
        }
        sync_parent(&path)?;
        Ok(Value::Null)
      }
      "fs.rename" => {
        let request: RenameRequest = decode(params)?;
        let from_components = validate_path(&request.from_path, false)?;
        let to_components = validate_path(&request.to_path, false)?;
        let root = self.applet_root(applet_id)?;
        let (from, _) = resolve(&root, &from_components)?;
        let to = destination(&root, &to_components)?;
        if metadata_if_present(&to)?.is_some() {
          return Err("Rename destination already exists.".into());
        }
        if to.starts_with(&from) {
          return Err("A directory cannot be moved inside itself.".into());
        }
        // All mutations of this private tree go through the service lock; no
        // other API call can create the destination after this existence check.
        fs::rename(&from, &to).map_err(|_| "Could not rename the applet entry.")?;
        sync_parent(&from)?;
        if from.parent() != to.parent() {
          sync_parent(&to)?;
        }
        Ok(Value::Null)
      }
      _ => Err("Unknown filesystem operation.".into()),
    }
  }

  fn applet_root(&self, applet_id: &str) -> Result<PathBuf> {
    ensure_directory(&self.root)?;
    let root = self.root.join(applet_id);
    ensure_directory(&root)?;
    Ok(root)
  }
}

fn default_read_length() -> usize {
  DEFAULT_READ_BYTES
}

fn decode<T: serde::de::DeserializeOwned>(params: Value) -> Result<T> {
  serde_json::from_value(params).map_err(|_| "Invalid filesystem request parameters.".into())
}

fn validate_path(path: &str, allow_empty: bool) -> Result<Vec<&str>> {
  if path.is_empty() {
    return if allow_empty {
      Ok(Vec::new())
    } else {
      Err("This operation requires a nonempty relative path.".into())
    };
  }
  if path.len() > MAX_PATH_BYTES || path.contains(['\\', ':']) || path.chars().any(char::is_control)
  {
    return Err("Use a relative path without control characters, backslashes, or colons.".into());
  }
  let components = path.split('/').collect::<Vec<_>>();
  if components.iter().any(|component| {
    component.is_empty() || *component == "." || *component == ".." || component.len() > 255
  }) {
    return Err("Paths cannot be absolute or contain empty, dot, or parent components.".into());
  }
  Ok(components)
}

fn metadata_if_present(path: &Path) -> Result<Option<Metadata>> {
  match fs::symlink_metadata(path) {
    Ok(metadata) => {
      regular_kind(&metadata)?;
      Ok(Some(metadata))
    }
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(_) => Err("Could not inspect the applet entry.".into()),
  }
}

fn regular_kind(metadata: &Metadata) -> Result<Kind> {
  if metadata.file_type().is_symlink() {
    return Err("Filesystem links are not supported.".into());
  }
  if metadata.is_file() {
    Ok(Kind::File)
  } else if metadata.is_dir() {
    Ok(Kind::Directory)
  } else {
    Err("Only regular files and directories are supported.".into())
  }
}

fn file_info(metadata: &Metadata) -> Result<FileInfo> {
  Ok(FileInfo {
    kind: regular_kind(metadata)?,
    size_bytes: if metadata.is_file() {
      metadata.len()
    } else {
      0
    },
  })
}

fn ensure_directory(path: &Path) -> Result<()> {
  if metadata_if_present(path)?.is_none() {
    match fs::create_dir(path) {
      Ok(()) => sync_parent(path)?,
      Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
      Err(_) => return Err("Could not create the applet directory.".into()),
    }
  }
  let metadata = metadata_if_present(path)?.ok_or("The applet directory is unavailable.")?;
  if !metadata.is_dir() {
    return Err("A path component is not a directory.".into());
  }
  Ok(())
}

fn resolve(root: &Path, components: &[&str]) -> Result<(PathBuf, Metadata)> {
  let mut path = root.to_owned();
  let mut metadata = metadata_if_present(&path)?.ok_or("The applet directory is unavailable.")?;
  for component in components {
    if !metadata.is_dir() {
      return Err("A path component is not a directory.".into());
    }
    path.push(component);
    metadata = metadata_if_present(&path)?.ok_or("The applet entry does not exist.")?;
  }
  Ok((path, metadata))
}

fn destination(root: &Path, components: &[&str]) -> Result<PathBuf> {
  let (name, parents) = components.split_last().ok_or("A file path is required.")?;
  let (parent, metadata) = resolve(root, parents)?;
  if !metadata.is_dir() {
    return Err("The parent path is not a directory.".into());
  }
  Ok(parent.join(name))
}

fn sync_parent(path: &Path) -> Result<()> {
  #[cfg(unix)]
  {
    let parent = path.parent().ok_or("Invalid applet storage location.")?;
    File::open(parent)
      .and_then(|directory| directory.sync_all())
      .map_err(|_| "Could not persist the applet directory.".to_string())?;
  }
  #[cfg(not(unix))]
  let _ = path;
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn fixture() -> (tempfile::TempDir, FileStore) {
    let directory = tempfile::tempdir().unwrap();
    let store = FileStore::new(directory.path().join("files"));
    (directory, store)
  }

  #[test]
  fn native_paths_remain_private_and_require_fresh_destinations() {
    let (_directory, store) = fixture();
    store
      .dispatch("first", "fs.mkdir", json!({"path":"recordings"}))
      .unwrap();
    let fresh = store
      .native_path("first", "recordings/new.m4a", true)
      .unwrap();
    assert!(fresh.ends_with("first/recordings/new.m4a"));
    assert!(store
      .native_path("second", "recordings/new.m4a", false)
      .is_err());
    for path in ["../second/data", "/tmp/data", "recordings/../../data"] {
      assert!(store.native_path("first", path, true).is_err());
    }
    write(&store, "first", "recordings/new.m4a", b"audio").unwrap();
    assert!(store
      .native_path("first", "recordings/new.m4a", true)
      .is_err());
    assert!(store
      .native_path("first", "recordings/new.m4a", false)
      .is_ok());
  }

  #[cfg(unix)]
  #[test]
  fn native_paths_reject_symlink_targets_and_parents() {
    let (directory, store) = fixture();
    store
      .dispatch("first", "fs.mkdir", json!({"path":"recordings"}))
      .unwrap();
    let outside = directory.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(
      &outside,
      directory.path().join("files/first/recordings/link"),
    )
    .unwrap();
    assert!(store
      .native_path("first", "recordings/link", false)
      .is_err());
    assert!(store
      .native_path("first", "recordings/link/audio.m4a", true)
      .is_err());
  }

  fn write(store: &FileStore, applet: &str, path: &str, bytes: &[u8]) -> Result<Value> {
    store.dispatch(applet, "fs.write", json!({ "path": path, "bytes": bytes }))
  }

  #[test]
  fn files_persist_across_store_recreation_and_stay_applet_scoped() {
    let (directory, store) = fixture();
    store
      .dispatch("first", "fs.mkdir", json!({ "path": "notes/drafts" }))
      .unwrap();
    write(&store, "first", "notes/drafts/hello.txt", b"hello").unwrap();
    assert!(store
      .dispatch(
        "second",
        "fs.read",
        json!({ "path": "notes/drafts/hello.txt" })
      )
      .is_err());
    write(&store, "second", "hello.txt", b"other").unwrap();
    let reopened = FileStore::new(directory.path().join("files"));
    assert_eq!(
      reopened
        .dispatch(
          "first",
          "fs.read",
          json!({ "path": "notes/drafts/hello.txt" })
        )
        .unwrap(),
      json!({ "bytes": [104, 101, 108, 108, 111], "eof": true })
    );
    assert_eq!(
      reopened
        .dispatch(
          "first",
          "fs.stat",
          json!({ "path": "notes/drafts/hello.txt" })
        )
        .unwrap(),
      json!({ "kind": "file", "size_bytes": 5 })
    );
    assert_eq!(
      reopened
        .dispatch("first", "fs.stat", json!({ "path": "" }))
        .unwrap(),
      json!({ "kind": "directory", "size_bytes": 0 })
    );
    assert_eq!(
      reopened
        .dispatch("first", "fs.list", json!({ "path": "notes/drafts" }))
        .unwrap(),
      json!({ "entries": [{ "name": "hello.txt", "kind": "file", "size_bytes": 5 }] })
    );
  }

  #[test]
  fn binary_range_reads_report_end_of_file_and_enforce_bounds() {
    let (_directory, store) = fixture();
    write(&store, "chat", "data.bin", &[0, 255, 13, 10, 42]).unwrap();
    assert_eq!(
      store
        .dispatch(
          "chat",
          "fs.read",
          json!({ "path": "data.bin", "offset": 1, "length": 2 })
        )
        .unwrap(),
      json!({ "bytes": [255, 13], "eof": false })
    );
    assert_eq!(
      store
        .dispatch(
          "chat",
          "fs.read",
          json!({ "path": "data.bin", "offset": 3, "length": 10 })
        )
        .unwrap(),
      json!({ "bytes": [10, 42], "eof": true })
    );
    assert_eq!(
      store
        .dispatch(
          "chat",
          "fs.read",
          json!({ "path": "data.bin", "offset": u64::MAX })
        )
        .unwrap(),
      json!({ "bytes": [], "eof": true })
    );
    assert_eq!(
      store
        .dispatch(
          "chat",
          "fs.read",
          json!({ "path": "data.bin", "length": 0 })
        )
        .unwrap(),
      json!({ "bytes": [], "eof": false })
    );
    assert!(store
      .dispatch(
        "chat",
        "fs.read",
        json!({ "path": "data.bin", "length": MAX_READ_BYTES + 1 })
      )
      .is_err());
    assert!(write(&store, "chat", "large.bin", &vec![0; MAX_WRITE_BYTES + 1]).is_err());
    assert!(store
      .dispatch("chat", "fs.stat", json!({ "path": "large.bin" }))
      .is_err());
  }

  #[test]
  fn traversal_absolute_windows_and_invalid_paths_are_rejected() {
    let (_directory, store) = fixture();
    for path in [
      "",
      "/absolute",
      "../escape",
      "nested/../escape",
      ".",
      "nested/./file",
      "nested//file",
      "nested/",
      "C:/escape",
      "C:\\escape",
      "\\\\server\\share",
      "notes\\file",
      "file:stream",
      "nul\0file",
      "line\nfile",
    ] {
      assert!(
        write(&store, "chat", path, b"data").is_err(),
        "accepted {path:?}"
      );
    }
    assert!(write(&store, "../other", "file", b"data").is_err());
    for method in ["fs.read", "fs.mkdir", "fs.remove"] {
      assert!(store
        .dispatch("chat", method, json!({ "path": "" }))
        .is_err());
    }
    assert_eq!(
      store.dispatch("chat", "fs.list", json!({})).unwrap(),
      json!({ "entries": [] })
    );
    assert!(write(&store, "chat", "missing-parent/file", b"data").is_err());
  }

  #[test]
  fn writes_replace_files_atomically_and_rename_never_replaces_entries() {
    let (directory, store) = fixture();
    write(&store, "chat", "first", b"old").unwrap();
    write(&store, "chat", "first", b"new data").unwrap();
    write(&store, "chat", "second", b"preserve").unwrap();
    assert!(store
      .dispatch(
        "chat",
        "fs.rename",
        json!({ "from_path": "first", "to_path": "second" })
      )
      .is_err());
    assert_eq!(
      fs::read(directory.path().join("files/chat/second")).unwrap(),
      b"preserve"
    );
    assert_eq!(
      fs::read(directory.path().join("files/chat/first")).unwrap(),
      b"new data"
    );
    store
      .dispatch("chat", "fs.mkdir", json!({ "path": "archive" }))
      .unwrap();
    store
      .dispatch(
        "chat",
        "fs.rename",
        json!({ "from_path": "first", "to_path": "archive/moved" }),
      )
      .unwrap();
    assert!(!directory.path().join("files/chat/first").exists());
    assert_eq!(
      fs::read(directory.path().join("files/chat/archive/moved")).unwrap(),
      b"new data"
    );
    assert!(write(&store, "chat", "archive", b"invalid").is_err());
    let entries = store.dispatch("chat", "fs.list", json!({})).unwrap();
    assert_eq!(entries["entries"].as_array().unwrap().len(), 2);
    assert_eq!(entries["entries"][0]["name"], "archive");
    assert!(!fs::read_dir(directory.path().join("files/chat"))
      .unwrap()
      .any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".tmp")));
  }

  #[test]
  fn remove_preserves_nonempty_directories_and_root() {
    let (_directory, store) = fixture();
    store
      .dispatch("chat", "fs.mkdir", json!({ "path": "parent/child" }))
      .unwrap();
    write(&store, "chat", "parent/child/file", b"keep").unwrap();
    assert!(store
      .dispatch("chat", "fs.remove", json!({ "path": "parent" }))
      .is_err());
    assert!(store
      .dispatch("chat", "fs.remove", json!({ "path": "parent/child" }))
      .is_err());
    store
      .dispatch("chat", "fs.remove", json!({ "path": "parent/child/file" }))
      .unwrap();
    store
      .dispatch("chat", "fs.remove", json!({ "path": "parent/child" }))
      .unwrap();
    store
      .dispatch("chat", "fs.remove", json!({ "path": "parent" }))
      .unwrap();
    assert_eq!(
      store.dispatch("chat", "fs.list", json!({})).unwrap(),
      json!({ "entries": [] })
    );
    assert!(store
      .dispatch("chat", "fs.remove", json!({ "path": "" }))
      .is_err());
  }

  #[test]
  fn lists_refuse_excess_entries() {
    let (directory, store) = fixture();
    store
      .dispatch("chat", "fs.mkdir", json!({ "path": "many" }))
      .unwrap();
    let path = directory.path().join("files/chat/many");
    for index in 0..=MAX_LIST_ENTRIES {
      fs::write(path.join(format!("file-{index}")), []).unwrap();
    }
    assert!(store
      .dispatch("chat", "fs.list", json!({ "path": "many" }))
      .is_err());
  }

  #[test]
  fn concurrent_renames_cannot_replace_each_others_destination() {
    use std::sync::{Arc, Barrier};
    let (directory, store) = fixture();
    write(&store, "chat", "first", b"first").unwrap();
    write(&store, "chat", "second", b"second").unwrap();
    let store = Arc::new(store);
    let barrier = Arc::new(Barrier::new(2));
    let results = std::thread::scope(|scope| {
      let mut tasks = Vec::new();
      for from_path in ["first", "second"] {
        let store = store.clone();
        let barrier = barrier.clone();
        tasks.push(scope.spawn(move || {
          barrier.wait();
          (
            from_path,
            store.dispatch(
              "chat",
              "fs.rename",
              json!({
                "from_path": from_path, "to_path": "destination",
              }),
            ),
          )
        }));
      }
      tasks
        .into_iter()
        .map(|task| task.join().unwrap())
        .collect::<Vec<_>>()
    });
    assert_eq!(
      results.iter().filter(|(_, result)| result.is_ok()).count(),
      1
    );
    for (source, result) in results {
      let path = directory.path().join("files/chat");
      if result.is_ok() {
        assert_eq!(
          fs::read(path.join("destination")).unwrap(),
          source.as_bytes()
        );
        assert!(!path.join(source).exists());
      } else {
        assert_eq!(fs::read(path.join(source)).unwrap(), source.as_bytes());
      }
    }
  }

  #[cfg(unix)]
  #[test]
  fn symlinks_at_root_applet_ancestors_and_leaf_are_refused() {
    use std::os::unix::fs::symlink;
    let (directory, store) = fixture();
    let outside = directory.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret"), b"private").unwrap();
    symlink(&outside, directory.path().join("files")).unwrap();
    assert!(store.dispatch("chat", "fs.list", json!({})).is_err());
    fs::remove_file(directory.path().join("files")).unwrap();
    fs::create_dir(directory.path().join("files")).unwrap();
    symlink(&outside, directory.path().join("files/chat")).unwrap();
    assert!(store.dispatch("chat", "fs.list", json!({})).is_err());
    fs::remove_file(directory.path().join("files/chat")).unwrap();
    store
      .dispatch("chat", "fs.mkdir", json!({ "path": "nested" }))
      .unwrap();
    let root = directory.path().join("files/chat");
    symlink(&outside, root.join("nested/escape")).unwrap();
    symlink(outside.join("secret"), root.join("linked")).unwrap();
    for path in ["linked", "nested/escape/secret"] {
      assert!(store
        .dispatch("chat", "fs.read", json!({ "path": path }))
        .is_err());
      assert!(store
        .dispatch("chat", "fs.stat", json!({ "path": path }))
        .is_err());
      assert!(store
        .dispatch("chat", "fs.remove", json!({ "path": path }))
        .is_err());
      assert!(write(&store, "chat", path, b"overwrite").is_err());
      assert!(store
        .dispatch(
          "chat",
          "fs.rename",
          json!({ "from_path": path, "to_path": "safe" })
        )
        .is_err());
    }
    assert!(store
      .dispatch("chat", "fs.mkdir", json!({ "path": "nested/escape/new" }))
      .is_err());
    assert!(store.dispatch("chat", "fs.list", json!({})).is_err());
    write(&store, "chat", "safe", b"data").unwrap();
    assert!(store
      .dispatch(
        "chat",
        "fs.rename",
        json!({ "from_path": "safe", "to_path": "linked" })
      )
      .is_err());
    assert_eq!(fs::read(outside.join("secret")).unwrap(), b"private");
    assert!(!outside.join("new").exists());
  }
}
