//! Shared native file selection. Cancellation is recorded before dispatching UI
//! work, so a view reset cannot race a late picker onto the screen.
use block2::RcBlock;
use objc2::{rc::Retained, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSModalResponseCancel, NSModalResponseOK, NSOpenPanel};
use objc2_foundation::{NSArray, NSString};
use std::{
  cell::RefCell,
  collections::{HashMap, HashSet},
  fs::{self, File, OpenOptions},
  io::{Read, Write},
  path::{Path, PathBuf},
  sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex, OnceLock, Weak,
  },
};
use tauri::{AppHandle, Runtime};

const MAX_TOMBSTONES: usize = 8192;
const MAX_ACTIVE: usize = 64;
const CANCELLED: &str = "The file request was cancelled.";
const RESTART: &str = "The file request limit was reached. Restart Apell to continue.";

#[derive(Clone, Hash, PartialEq, Eq)]
struct Key {
  owner: String,
  operation_id: String,
}

#[derive(Default)]
struct Registry {
  active: HashMap<Key, ActiveOperation>,
  closed: HashSet<String>,
  cancelled: HashSet<Key>,
  exhausted: bool,
}

struct ActiveOperation {
  operation: Weak<OperationState>,
  cancelled: Arc<AtomicBool>,
}

impl Registry {
  fn cancel(&mut self, owner: &str, operation_id: Option<&str>) {
    // Never evict a closed owner: doing so would admit a delayed command from
    // that view. If this bounded registry fills, new work fails closed instead.
    if let Some(operation_id) = operation_id {
      let key = Key {
        owner: owner.into(),
        operation_id: operation_id.into(),
      };
      if !self.closed.contains(owner) && !self.cancelled.contains(&key) {
        if self.closed.len() + self.cancelled.len() >= MAX_TOMBSTONES {
          self.exhausted = true;
        } else {
          self.cancelled.insert(key);
        }
      }
    } else if !self.closed.contains(owner) {
      self.cancelled.retain(|key| key.owner != owner);
      if self.closed.len() + self.cancelled.len() >= MAX_TOMBSTONES {
        self.exhausted = true;
      } else {
        self.closed.insert(owner.into());
      }
    }
    for (key, operation) in &self.active {
      if key.owner == owner
        && operation_id.is_none_or(|operation_id| key.operation_id == operation_id)
      {
        operation.cancelled.store(true, Ordering::SeqCst);
      }
    }
  }
}

struct OperationState {
  key: Key,
  cancelled: Arc<AtomicBool>,
  registry: Weak<Mutex<Registry>>,
}

impl Drop for OperationState {
  fn drop(&mut self) {
    if let Some(registry) = self.registry.upgrade() {
      if let Ok(mut registry) = registry.lock() {
        if registry
          .active
          .get(&self.key)
          .is_some_and(|operation| std::ptr::eq(operation.operation.as_ptr(), self))
        {
          registry.active.remove(&self.key);
        }
      }
    }
  }
}

#[derive(Clone)]
pub struct Operation {
  state: Arc<OperationState>,
}

fn registry() -> &'static Arc<Mutex<Registry>> {
  static REGISTRY: OnceLock<Arc<Mutex<Registry>>> = OnceLock::new();
  REGISTRY.get_or_init(|| Arc::new(Mutex::new(Registry::default())))
}

impl Operation {
  pub fn begin(owner: &str, operation_id: &str) -> Result<Self, String> {
    Self::begin_in(registry(), owner, operation_id)
  }

  fn begin_in(
    registry: &Arc<Mutex<Registry>>,
    owner: &str,
    operation_id: &str,
  ) -> Result<Self, String> {
    if owner.is_empty() || operation_id.is_empty() || owner.len() > 256 || operation_id.len() > 256
    {
      return Err("Invalid file request.".into());
    }
    let key = Key {
      owner: owner.into(),
      operation_id: operation_id.into(),
    };
    let mut entries = registry
      .lock()
      .map_err(|_| "The file service is unavailable.")?;
    if entries.closed.contains(owner) || entries.cancelled.contains(&key) {
      return Err(CANCELLED.into());
    }
    if entries.exhausted {
      return Err(RESTART.into());
    }
    entries
      .active
      .retain(|_, operation| operation.operation.strong_count() > 0);
    if entries.active.contains_key(&key) {
      return Err("This file request is already running.".into());
    }
    if entries.active.len() >= MAX_ACTIVE {
      return Err("Too many file requests are running. Finish one and try again.".into());
    }
    let state = Arc::new(OperationState {
      key: key.clone(),
      cancelled: Arc::new(AtomicBool::new(false)),
      registry: Arc::downgrade(registry),
    });
    entries.active.insert(
      key,
      ActiveOperation {
        operation: Arc::downgrade(&state),
        cancelled: state.cancelled.clone(),
      },
    );
    Ok(Self { state })
  }

  pub fn cancelled(&self) -> bool {
    self.state.cancelled.load(Ordering::SeqCst)
  }

  pub fn ensure_active(&self) -> Result<(), String> {
    if self.cancelled() {
      Err(CANCELLED.into())
    } else {
      Ok(())
    }
  }
}

thread_local! {
  static PANELS: RefCell<HashMap<Key, Retained<NSOpenPanel>>> = RefCell::new(HashMap::new());
}

/// Called on a blocking worker. The sheet and its callback only touch AppKit on
/// the main thread, while the worker waits without blocking the native run loop.
pub fn choose<R: Runtime>(
  app: &AppHandle<R>,
  operation: &Operation,
  title: &str,
  extensions: &[&str],
) -> Result<Option<PathBuf>, String> {
  operation.ensure_active()?;
  let presentation_operation = operation.clone();
  let title = title.to_owned();
  let extensions: Vec<String> = extensions.iter().map(|value| (*value).to_owned()).collect();
  let (sender, receiver) = mpsc::sync_channel(1);
  app
    .run_on_main_thread(move || {
      let operation = presentation_operation;
      if let Err(error) = operation.ensure_active() {
        let _ = sender.send(Err(error));
        return;
      }
      let Some(main) = MainThreadMarker::new() else {
        let _ = sender.send(Err("Could not open the file picker.".into()));
        return;
      };
      let panel = NSOpenPanel::openPanel(main);
      panel.setTitle(Some(&NSString::from_str(&title)));
      panel.setCanChooseFiles(true);
      panel.setCanChooseDirectories(false);
      panel.setAllowsMultipleSelection(false);
      panel.setAllowsOtherFileTypes(false);
      if !extensions.is_empty() {
        let types: Vec<_> = extensions
          .iter()
          .map(|value| NSString::from_str(value))
          .collect();
        #[allow(deprecated)]
        panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&types)));
      }
      let key = operation.state.key.clone();
      PANELS.with(|panels| panels.borrow_mut().insert(key.clone(), panel.clone()));
      if let Err(error) = operation.ensure_active() {
        PANELS.with(|panels| panels.borrow_mut().remove(&key));
        let _ = sender.send(Err(error));
        return;
      }
      let completion = RcBlock::new(move |response| {
        // Removing the retained panel ends the borrow before any native calls;
        // cancel or completion can synchronously enter the AppKit run loop.
        let panel = PANELS.with(|panels| panels.borrow_mut().remove(&key));
        let result = operation.ensure_active().and_then(|()| {
          if response == NSModalResponseCancel {
            Ok(None)
          } else if response == NSModalResponseOK {
            panel
              .and_then(|panel| panel.URL())
              .and_then(|url| url.to_file_path())
              .map(Some)
              .ok_or_else(|| "Could not access the selected file.".into())
          } else {
            Err("The file picker could not finish.".into())
          }
        });
        let _ = sender.send(result);
      });
      let application = NSApplication::sharedApplication(main);
      if let Some(window) = application.mainWindow().or_else(|| application.keyWindow()) {
        panel.beginSheetModalForWindow_completionHandler(&window, &completion);
      } else {
        panel.beginWithCompletionHandler(&completion);
      }
    })
    .map_err(|_| "Could not open the file picker.")?;
  let selected = receiver
    .recv()
    .map_err(|_| "The file picker closed unexpectedly.")??;
  operation.ensure_active()?;
  Ok(selected)
}

pub fn cancel<R: Runtime>(
  app: &AppHandle<R>,
  owner: &str,
  operation_id: Option<&str>,
) -> Result<(), String> {
  if owner.is_empty()
    || owner.len() > 256
    || operation_id.is_some_and(|id| id.is_empty() || id.len() > 256)
  {
    return Err("Invalid file request.".into());
  }
  registry()
    .lock()
    .map_err(|_| "The file service is unavailable.")?
    .cancel(owner, operation_id);
  let owner = owner.to_owned();
  let operation_id = operation_id.map(str::to_owned);
  app
    .run_on_main_thread(move || {
      let panels: Vec<_> = PANELS.with(|panels| {
        panels
          .borrow()
          .iter()
          .filter(|(key, _)| {
            key.owner == owner
              && operation_id
                .as_ref()
                .is_none_or(|id| key.operation_id == *id)
          })
          .map(|(_, panel)| panel.clone())
          .collect()
      });
      for panel in panels {
        // SAFETY: nil is the documented action sender and the retained panel
        // belongs to this main thread. No RefCell or registry borrow is held.
        unsafe { panel.cancel(None) };
      }
    })
    .map_err(|_| "Could not cancel the file picker.".into())
}

/// Abort work already registered for a reversible rollback. Unlike retiring a
/// view, this must not reject a future attempt from the same owner.
pub fn cancel_active(owner: &str, operation_id: &str) -> Result<(), String> {
  let entries = registry()
    .lock()
    .map_err(|_| "The file service is unavailable.")?;
  if let Some(operation) = entries.active.get(&Key {
    owner: owner.into(),
    operation_id: operation_id.into(),
  }) {
    operation.cancelled.store(true, Ordering::SeqCst);
  }
  Ok(())
}

/// Copy explicitly selected bytes without exposing their native path to an
/// applet. The destination must be a new private staging file.
pub fn copy_file(
  source: &Path,
  destination: &Path,
  max_bytes: u64,
  operation: &Operation,
) -> Result<u64, String> {
  operation.ensure_active()?;
  let mut input = File::open(source).map_err(|_| "Could not read the selected file.")?;
  let metadata = input
    .metadata()
    .map_err(|_| "Could not read the selected file.")?;
  if !metadata.is_file() || metadata.len() == 0 {
    return Err("Choose a nonempty regular file.".into());
  }
  if metadata.len() > max_bytes {
    return Err("The selected file exceeds the size limit.".into());
  }
  let mut output = OpenOptions::new()
    .write(true)
    .create_new(true)
    .open(destination)
    .map_err(|_| "Could not prepare the imported file.")?;
  let result = (|| {
    let mut total = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
      operation.ensure_active()?;
      let count = input
        .read(&mut buffer)
        .map_err(|_| "Could not read the selected file.")?;
      if count == 0 {
        break;
      }
      total = total
        .checked_add(count as u64)
        .ok_or("The selected file exceeds the size limit.")?;
      if total > max_bytes {
        return Err("The selected file exceeds the size limit.".into());
      }
      operation.ensure_active()?;
      output
        .write_all(&buffer[..count])
        .map_err(|_| "Could not copy the selected file.")?;
    }
    if total == 0 {
      return Err("Choose a nonempty regular file.".into());
    }
    output
      .flush()
      .map_err(|_| "Could not finish importing the selected file.")?;
    operation.ensure_active()?;
    Ok(total)
  })();
  drop(output);
  if result.is_err() {
    let _ = fs::remove_file(destination);
  }
  result
}

#[cfg(test)]
mod tests {
  use super::*;

  fn isolated() -> Arc<Mutex<Registry>> {
    Arc::new(Mutex::new(Registry::default()))
  }

  #[test]
  fn cancellation_precedes_late_work_and_stays_scoped() {
    let registry = isolated();
    registry.lock().unwrap().cancel("closed", None);
    assert!(Operation::begin_in(&registry, "closed", "late").is_err());
    registry.lock().unwrap().cancel("photos", Some("old"));
    assert!(Operation::begin_in(&registry, "photos", "old").is_err());
    let next = Operation::begin_in(&registry, "photos", "next").unwrap();
    let other = Operation::begin_in(&registry, "other", "old").unwrap();
    registry.lock().unwrap().cancel("photos", Some("next"));
    assert!(next.cancelled());
    assert!(!other.cancelled());
  }

  #[test]
  fn completed_operations_release_slots_but_closed_owners_never_reopen() {
    let registry = isolated();
    let operation = Operation::begin_in(&registry, "audio", "import").unwrap();
    assert!(Operation::begin_in(&registry, "audio", "import").is_err());
    drop(operation);
    assert!(registry.lock().unwrap().active.is_empty());
    let retry = Operation::begin_in(&registry, "audio", "import").unwrap();
    registry.lock().unwrap().cancel("audio", None);
    assert!(retry.cancelled());
    drop(retry);
    assert!(Operation::begin_in(&registry, "audio", "another").is_err());
  }

  #[test]
  fn reversible_rollback_cancels_only_current_work_and_allows_retry() {
    let owner = format!("rollback-{}", uuid::Uuid::new_v4());
    let operation = Operation::begin(&owner, "start").unwrap();
    let other = Operation::begin(&owner, "other").unwrap();
    cancel_active(&owner, "start").unwrap();
    assert!(operation.cancelled());
    assert!(!other.cancelled());
    drop(operation);
    assert!(Operation::begin(&owner, "start").is_ok());
  }

  #[test]
  fn full_cancellation_registry_fails_closed_without_eviction() {
    let registry = isolated();
    for index in 0..MAX_TOMBSTONES {
      registry
        .lock()
        .unwrap()
        .cancel(&format!("owner-{index}"), None);
    }
    registry.lock().unwrap().cancel("overflow", None);
    let entries = registry.lock().unwrap();
    assert_eq!(entries.closed.len(), MAX_TOMBSTONES);
    assert!(entries.exhausted);
    drop(entries);
    assert!(Operation::begin_in(&registry, "owner-0", "late").is_err());
    assert!(Operation::begin_in(&registry, "overflow", "late").is_err());
    assert!(Operation::begin_in(&registry, "new", "new").is_err());
  }

  #[test]
  fn copy_is_bounded_and_never_overwrites_a_destination() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let destination = directory.path().join("destination");
    fs::write(&source, vec![7; 130_000]).unwrap();
    let registry = isolated();
    let operation = Operation::begin_in(&registry, "copy", "one").unwrap();
    assert!(copy_file(&source, &destination, 129_999, &operation).is_err());
    assert!(!destination.exists());
    assert_eq!(
      copy_file(&source, &destination, 130_000, &operation).unwrap(),
      130_000
    );
    assert_eq!(fs::read(&destination).unwrap(), vec![7; 130_000]);
    fs::write(&source, b"replacement").unwrap();
    assert!(copy_file(&source, &destination, 130_000, &operation).is_err());
    assert_eq!(fs::read(&destination).unwrap(), vec![7; 130_000]);
  }

  #[test]
  fn cancelled_copy_does_not_publish_a_file() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let destination = directory.path().join("destination");
    fs::write(&source, b"selected file").unwrap();
    let registry = isolated();
    let operation = Operation::begin_in(&registry, "copy", "cancelled").unwrap();
    registry.lock().unwrap().cancel("copy", None);
    assert!(copy_file(&source, &destination, 100, &operation).is_err());
    assert!(!destination.exists());
  }
}
