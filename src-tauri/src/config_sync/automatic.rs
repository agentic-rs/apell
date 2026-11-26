use super::{compare_settings, load_state, synchronize, Run, SyncOptions};
use crate::{account::Access, host::Host, key_sync::BackgroundStop};
use std::{future::Future, time::Duration};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{watch, Mutex, Notify};

const INTERVAL: Duration = Duration::from_secs(60);
const CHANGED: &str = "apell-config-sync-changed";
const PAUSED: &str = "Automatic model settings sync was stopped.";

#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct Activity {
  pub(super) syncing: bool,
  pub(super) error: Option<String>,
}

pub(super) struct Automatic {
  wake: Notify,
  pause: watch::Sender<bool>,
  activity: Mutex<Option<(String, Activity)>>,
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

  pub(super) async fn activity(&self, account: &str) -> Activity {
    self
      .activity
      .lock()
      .await
      .as_ref()
      .filter(|(owner, _)| owner == account)
      .map(|(_, activity)| activity.clone())
      .unwrap_or_default()
  }

  async fn publish(&self, app: &AppHandle, activity: Option<(String, Activity)>) {
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
      let Ok(account) = stop.wait(host.account.account_key(&app)).await else {
        continue;
      };
      let Ok(Some(account)) = account else {
        clear_activity(&app, &host).await;
        continue;
      };
      let Ok(_gate) = stop.wait(host.config_sync.gate.lock()).await else {
        continue;
      };
      let result = synchronize_automatically(&app, &host, &account, &stop).await;
      if stop.check().is_ok() {
        let activity = match result {
          Ok(()) => None,
          Err(error) => Some((
            account,
            Activity {
              syncing: false,
              error: Some(error),
            },
          )),
        };
        automatic.publish(&app, activity).await;
      }
    }
  });
}

async fn synchronize_automatically(
  app: &AppHandle,
  host: &Host,
  account: &str,
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
    .publish(
      app,
      Some((
        account.to_owned(),
        Activity {
          syncing: true,
          error: None,
        },
      )),
    )
    .await;
  let access = stop.access(app, host).await?;
  if access.account_key != account {
    return Err("The signed-in Account changed. Try syncing again.".into());
  }
  let compared = compare_settings(app, host, &access, &state, stop).await?;
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
  .await?;
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::key_sync::KeySync;

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
