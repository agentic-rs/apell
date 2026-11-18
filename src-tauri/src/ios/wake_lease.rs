//! Give an in-flight Store download time to finish when iOS locks or suspends
//! Apell, without changing the Rust transport's redirect and package checks.

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_ui_kit::{UIApplication, UIBackgroundTaskIdentifier, UIBackgroundTaskInvalid};
use std::sync::{
  atomic::{AtomicUsize, Ordering},
  Arc,
};
use tokio::sync::{oneshot, watch};

pub(crate) struct WakeLease {
  app: tauri::AppHandle,
  task: Arc<AtomicUsize>,
  expired: watch::Receiver<bool>,
}

impl WakeLease {
  /// The OS may decline a background assertion; the transfer can still run in
  /// the foreground. Call before spending time on the network.
  pub(crate) async fn begin(app: &tauri::AppHandle) -> Option<Self> {
    let (started, wait) = oneshot::channel();
    let handle = app.clone();
    app
      .run_on_main_thread(move || {
        let Some(main) = MainThreadMarker::new() else {
          return;
        };
        let (expiration, expired) = watch::channel(false);
        let task = Arc::new(AtomicUsize::new(invalid_task()));
        let expiring_task = task.clone();
        let handler = RcBlock::new(move || {
          expiration.send_replace(true);
          // UIKit calls its expiration handler on the main thread. End the
          // task synchronously here, even if the Rust future can't run yet.
          if let Some(main) = MainThreadMarker::new() {
            end(&expiring_task, main);
          }
        });
        let id = UIApplication::sharedApplication(main)
          .beginBackgroundTaskWithExpirationHandler(Some(&handler));
        if id == invalid_task() {
          return;
        }
        task.store(id, Ordering::Relaxed);
        if *expired.borrow() {
          end(&task, main);
        }
        // Dropping an assertion if the waiting caller is cancelled also
        // closes the task instead of leaving it alive until the deadline.
        let _ = started.send(Self {
          app: handle,
          task,
          expired,
        });
      })
      .ok()?;
    wait.await.ok()
  }

  pub(crate) async fn expired(&self) {
    let mut expired = self.expired.clone();
    let _ = expired.wait_for(|value| *value).await;
  }
}

impl Drop for WakeLease {
  fn drop(&mut self) {
    if let Some(main) = MainThreadMarker::new() {
      end(&self.task, main);
    } else {
      let task = self.task.clone();
      let _ = self.app.run_on_main_thread(move || {
        if let Some(main) = MainThreadMarker::new() {
          end(&task, main);
        }
      });
    }
  }
}

fn end(task: &AtomicUsize, main: MainThreadMarker) {
  let id = task.swap(invalid_task(), Ordering::Relaxed);
  if id != invalid_task() {
    UIApplication::sharedApplication(main).endBackgroundTask(id);
  }
}

fn invalid_task() -> UIBackgroundTaskIdentifier {
  // SAFETY: UIKit exposes an immutable process-wide constant.
  unsafe { UIBackgroundTaskInvalid }
}
