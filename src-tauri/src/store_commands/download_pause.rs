use serde_json::json;
use std::{collections::HashMap, sync::Mutex};
use tauri::{AppHandle, Emitter};
use tokio::sync::watch;
use uuid::Uuid;

const PAUSE_UNAVAILABLE: &str = "Could not access the Store download controls.";

#[derive(Default)]
pub(crate) struct DownloadPauses {
  // Retain an entry during publication to reserve its operation ID even after
  // it can no longer be paused.
  active: Mutex<HashMap<Uuid, Option<watch::Sender<bool>>>>,
}

pub(super) struct ActiveDownload<'a> {
  pauses: &'a DownloadPauses,
  operation_id: Uuid,
  requested: watch::Receiver<bool>,
  #[cfg(target_os = "ios")]
  wake_lease: Option<crate::ios::WakeLease>,
}

impl DownloadPauses {
  fn register(&self, operation_id: Uuid) -> Result<ActiveDownload<'_>, String> {
    let mut active = self.active.lock().map_err(|_| PAUSE_UNAVAILABLE)?;
    if active.contains_key(&operation_id) {
      return Err("This Store download operation is already active.".into());
    }
    let (pause, requested) = watch::channel(false);
    active.insert(operation_id, Some(pause));
    Ok(ActiveDownload {
      pauses: self,
      operation_id,
      requested,
      #[cfg(target_os = "ios")]
      wake_lease: None,
    })
  }

  pub(super) async fn begin(
    &self,
    app: &AppHandle,
    operation_id: Uuid,
  ) -> Result<ActiveDownload<'_>, String> {
    let download = self.register(operation_id)?;
    #[cfg(target_os = "ios")]
    let download = {
      let mut download = download;
      download.wake_lease = crate::ios::WakeLease::begin(app).await;
      download
    };
    let _ = app.emit_to(
      "main",
      "apell-store-transfer-started",
      json!({ "operation_id": operation_id }),
    );
    Ok(download)
  }

  pub(super) fn request(&self, operation_id: Uuid) -> Result<bool, String> {
    let active = self.active.lock().map_err(|_| PAUSE_UNAVAILABLE)?;
    let Some(Some(pause)) = active.get(&operation_id) else {
      return Ok(false);
    };
    pause.send_replace(true);
    Ok(true)
  }
}

impl ActiveDownload<'_> {
  pub(super) async fn wait_for_pause(&self) {
    let mut receiver = self.requested.clone();
    let requested = async {
      let _ = receiver.wait_for(|paused| *paused).await;
    };
    #[cfg(target_os = "ios")]
    if let Some(lease) = &self.wake_lease {
      tokio::select! {
        _ = requested => (),
        _ = lease.expired() => (),
      }
      return;
    }
    requested.await;
  }

  pub(super) fn finish_transfer(&self) {
    if let Ok(mut active) = self.pauses.active.lock() {
      if let Some(pause) = active.get_mut(&self.operation_id) {
        *pause = None;
      }
    }
  }
}

impl Drop for ActiveDownload<'_> {
  fn drop(&mut self) {
    if let Ok(mut active) = self.pauses.active.lock() {
      active.remove(&self.operation_id);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn pause_signals_target_only_the_registered_operation() {
    let pauses = DownloadPauses::default();
    let first_id = Uuid::new_v4();
    let second_id = Uuid::new_v4();
    assert!(!pauses.request(first_id).unwrap());

    let first = pauses.register(first_id).unwrap();
    let second = pauses.register(second_id).unwrap();
    assert!(pauses.register(first_id).is_err());
    assert!(pauses.request(first_id).unwrap());
    first.wait_for_pause().await;
    assert!(!*second.requested.borrow());

    first.finish_transfer();
    assert!(!pauses.request(first_id).unwrap());
    assert!(pauses.register(first_id).is_err());
    drop(first);
    assert!(!pauses.request(first_id).unwrap());
    assert!(pauses.request(second_id).unwrap());
    second.wait_for_pause().await;
    drop(second);
    assert!(!pauses.request(second_id).unwrap());
    assert!(pauses.active.lock().unwrap().is_empty());
  }
}
