use super::{compare_settings, load_state, synchronize, Run, RunError, SyncOptions};
use crate::{account::Access, host::Host, key_sync::BackgroundStop};
use serde_json::{json, Value};
use std::{future::Future, time::Duration};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{watch, Mutex, Notify};
use uuid::Uuid;

const INTERVAL: Duration = Duration::from_secs(60);
const CHANGED: &str = "apell-config-sync-changed";
const PAUSED: &str = "Automatic model settings sync was stopped.";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Activity {
  syncing: bool,
  error: Option<String>,
}

impl Activity {
  pub(super) fn running() -> Self {
    Self {
      syncing: true,
      error: None,
    }
  }

  fn failed(error: String) -> Self {
    Self {
      syncing: false,
      error: Some(error),
    }
  }

  pub(super) fn after_manual_attempt(self, error: Option<&RunError>) -> Option<Self> {
    match error {
      Some(RunError::Failed(error)) => Some(Self::failed(error.clone())),
      Some(RunError::ReviewRequired(_)) => self.error.map(Self::failed),
      None => None,
    }
  }
}

pub(super) struct Automatic {
  wake: Notify,
  pause: watch::Sender<bool>,
  activity: Mutex<Option<(Uuid, Activity)>>,
}

impl Default for Automatic {
  fn default() -> Self {
    Self {
      wake: Notify::new(),
      pause: watch::channel(false).0,
      activity: Mutex::new(None),
    }
  }
}

impl Automatic {
  pub(super) fn pause(&self) -> PauseAutomatic<'_> {
    self.pause.send_replace(true);
    PauseAutomatic(&self.pause)
  }

  pub(super) async fn activity(&self, session_id: Uuid) -> Activity {
    self
      .activity
      .lock()
      .await
      .as_ref()
      .filter(|(owner, _)| *owner == session_id)
      .map(|(_, activity)| activity.clone())
      .unwrap_or_default()
  }

  pub(super) async fn add_activity(&self, session_id: Uuid, status: &mut Value) {
    let activity = self.activity(session_id).await;
    status["syncing"] = json!(activity.syncing);
    status["sync_error"] = json!(activity.error);
  }

  pub(super) async fn publish(&self, app: &AppHandle, activity: Option<(Uuid, Activity)>) {
    let mut current = self.activity.lock().await;
    if *current != activity {
      *current = activity;
      let _ = app.emit_to("main", CHANGED, ());
    }
  }
}

pub(super) struct PauseAutomatic<'a>(&'a watch::Sender<bool>);

impl Drop for PauseAutomatic<'_> {
  fn drop(&mut self) {
    self.0.send_replace(false);
  }
}

pub(super) struct SyncStop {
  account: BackgroundStop,
  automatic: Option<watch::Receiver<bool>>,
}

impl SyncStop {
  pub(super) fn manual(account: BackgroundStop) -> Self {
    Self {
      account,
      automatic: None,
    }
  }

  fn automatic(account: BackgroundStop, state: &Automatic) -> Self {
    Self {
      account,
      automatic: Some(state.pause.subscribe()),
    }
  }

  pub(super) fn check(&self) -> Result<(), String> {
    self.account.check()?;
    if self
      .automatic
      .as_ref()
      .is_some_and(|pause| *pause.borrow() || pause.has_changed().unwrap_or(true))
    {
      return Err(PAUSED.into());
    }
    Ok(())
  }

  pub(super) async fn wait<T>(&self, work: impl Future<Output = T>) -> Result<T, String> {
    self.check()?;
    let Some(mut pause) = self.automatic.clone() else {
      return self.account.wait(work).await;
    };
    self
      .account
      .wait(async {
        tokio::select! {
          biased;
          _ = pause.changed() => Err(PAUSED.into()),
          result = work => Ok(result),
        }
      })
      .await?
  }

  pub(super) async fn access(&self, app: &AppHandle, host: &Host) -> Result<Access, String> {
    self
      .wait(self.account.access(app, host, false, None))
      .await?
  }

  pub(super) async fn session(
    &self,
    app: &AppHandle,
    host: &Host,
  ) -> Result<Option<(String, Uuid)>, String> {
    self.wait(self.account.session(app, host)).await?
  }
}

pub(crate) fn kick(app: &AppHandle) {
  if let Some(host) = app.try_state::<Host>() {
    host.config_sync.automatic.wake.notify_one();
  }
}

pub(crate) async fn clear_activity(app: &AppHandle, host: &Host) {
  host.config_sync.automatic.publish(app, None).await;
}

pub(crate) fn start(app: &AppHandle) {
  let app = app.clone();
  tauri::async_runtime::spawn(async move {
    let host = app.state::<Host>();
    let automatic = &host.config_sync.automatic;
    let mut interval = tokio::time::interval(INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
      tokio::select! {
        _ = interval.tick() => (),
        _ = automatic.wake.notified() => (),
      }
      let stop = SyncStop::automatic(host.key_sync.background_stop(), automatic);
      let Ok(_gate) = stop.wait(host.config_sync.gate.lock()).await else {
        continue;
      };
      let Ok(session) = stop.session(&app, &host).await else {
        continue;
      };
      let Some((account, session_id)) = session else {
        clear_activity(&app, &host).await;
        continue;
      };
      let stop = SyncStop {
        account: stop.account.for_session(session_id),
        automatic: stop.automatic,
      };
      let result = synchronize_automatically(&app, &host, &account, session_id, &stop).await;
      if stop.check().is_ok() {
        let activity = result
          .err()
          .map(|error| (session_id, Activity::failed(error)));
        automatic.publish(&app, activity).await;
      }
    }
  });
}

async fn synchronize_automatically(
  app: &AppHandle,
  host: &Host,
  account: &str,
  session_id: Uuid,
  stop: &SyncStop,
) -> Result<(), String> {
  let state = stop.wait(load_state(host, account)).await??;
  if !state.enabled {
    return Ok(());
  }
  stop.check()?;
  host
    .config_sync
    .automatic
    .publish(app, Some((session_id, Activity::running())))
    .await;
  let (access, compared) = compare_settings(app, host, account, &state, stop).await?;
  synchronize(
    app,
    host,
    &access,
    state,
    Run {
      resolution: None,
      conflict_id: None,
    },
    compared,
    SyncOptions {
      stop,
      automatic: true,
    },
  )
  .await
  .map_err(RunError::into_message)?;
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::key_sync::KeySync;

  #[tokio::test]
  async fn model_failure_is_visible_only_within_its_sign_in() {
    let automatic = Automatic::default();
    let original = Uuid::new_v4();
    *automatic.activity.lock().await = Some((original, Activity::failed("Offline".into())));
    assert_eq!(
      automatic.activity(original).await,
      Activity::failed("Offline".into())
    );
    assert_eq!(
      automatic.activity(Uuid::new_v4()).await,
      Activity::default()
    );
  }

  #[test]
  fn manual_sync_reports_a_failure_and_preserves_it_until_a_successful_run() {
    let skipped = RunError::ReviewRequired("Refresh the review.".into());
    let unavailable = RunError::Failed("Sync service unavailable.".into());
    assert_eq!(
      Activity::default().after_manual_attempt(Some(&skipped)),
      None
    );

    let previous = Activity::default()
      .after_manual_attempt(Some(&unavailable))
      .unwrap();
    assert_eq!(
      previous,
      Activity::failed("Sync service unavailable.".into())
    );
    assert_eq!(
      previous.clone().after_manual_attempt(Some(&skipped)),
      Some(previous.clone())
    );
    assert_eq!(previous.after_manual_attempt(None), None);
  }

  #[tokio::test]
  async fn opting_out_cancels_only_existing_automatic_waits() {
    let key_sync = KeySync::new(reqwest::Client::new());
    let automatic = Automatic::default();
    let stop = SyncStop::automatic(key_sync.background_stop(), &automatic);
    let manual = SyncStop::manual(key_sync.background_stop());
    let (started, entered) = tokio::sync::oneshot::channel();
    let waiting = tokio::spawn(async move {
      stop
        .wait(async move {
          started.send(()).unwrap();
          std::future::pending::<()>().await
        })
        .await
    });
    entered.await.unwrap();
    let pause = automatic.pause();
    let during_pause = SyncStop::automatic(key_sync.background_stop(), &automatic);
    drop(pause);
    assert_eq!(
      tokio::time::timeout(Duration::from_secs(1), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err(),
      PAUSED
    );
    assert!(during_pause.check().is_err());
    assert!(SyncStop::automatic(key_sync.background_stop(), &automatic)
      .check()
      .is_ok());
    assert!(manual.check().is_ok());
  }
}
