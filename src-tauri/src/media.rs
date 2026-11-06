//! Host-owned audio operations. Applets only see paths in their private file store.
use crate::host::{string, Host, Session};
use serde_json::{json, Value};
use std::sync::{atomic::Ordering, Arc};
use tauri::AppHandle;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Default)]
pub struct AudioService {
  state: Mutex<AudioSession>,
}

#[derive(Default)]
struct AudioSession {
  owner: Option<String>,
  recording_path: Option<String>,
}

impl AudioSession {
  fn reconcile_failed_stop(&mut self, native_status: Option<&Value>) {
    // Android releases MediaRecorder even when finalization fails. Clear the
    // host path only when native state confirms capture has ended; an unknown
    // or still-active state must retain ownership for a later stop/reset.
    if native_status
      .and_then(|status| status.get("recording_state"))
      .and_then(Value::as_str)
      == Some("idle")
    {
      self.recording_path = None;
    }
  }
}

async fn native(
  app: AppHandle,
  owner: &str,
  method: &str,
  mut params: Value,
) -> Result<Value, String> {
  params["session_id"] = json!(owner);
  let method = method.to_owned();
  tauri::async_runtime::spawn_blocking(move || {
    tauri_plugin_apell_audio::call(&app, &method, params)
  })
  .await
  .map_err(|_| "The native audio operation could not finish.".to_string())?
}

async fn checked_native(
  app: AppHandle,
  owner: &str,
  session: &Session,
  method: &str,
  params: Value,
) -> Result<Value, String> {
  if !session.alive.load(Ordering::SeqCst) {
    return Err("Applet session is closed.".into());
  }
  native(app, owner, method, params).await
}

fn idle_status() -> Value {
  json!({"recording_state":"idle", "elapsed_ms":0, "playing":false,
    "position_ms":0, "duration_ms":0})
}

fn watch_session(app: AppHandle, service: Arc<AudioService>, owner: String, session: Session) {
  let mut cancellation = session.cancellation.subscribe();
  tauri::async_runtime::spawn(async move {
    if !*cancellation.borrow() {
      let _ = cancellation.changed().await;
    }
    // This watcher is the only code that releases ownership. Reset before
    // waiting for the operation lock so a pending picker/permission can settle.
    // No new owner is admitted until that pending call and reset have finished.
    let _ = native(app.clone(), &owner, "reset", json!({})).await;
    let mut state = service.state.lock().await;
    if state.owner.as_deref() == Some(&owner) {
      let _ = native(app, &owner, "reset", json!({})).await;
      *state = AudioSession::default();
    }
  });
}

pub async fn dispatch(
  app: AppHandle,
  host: &Host,
  owner: &str,
  session: Session,
  method: &str,
  params: Value,
) -> Result<Value, String> {
  let mobile = cfg!(any(target_os = "ios", target_os = "android"));
  if method == "audio.capabilities" {
    return Ok(json!({"recording":mobile,"import":mobile,"sharing":mobile,"playback":mobile}));
  }
  if !mobile {
    return Err("Native audio is available on iOS and Android. Use the browser preview for desktop recording.".into());
  }
  let mut state = host.audio.state.lock().await;
  if !session.alive.load(Ordering::SeqCst) {
    return Err("Applet session is closed.".into());
  }
  if state.owner.as_deref().is_some_and(|value| value != owner) {
    return Err("Audio is being released by another view. Try again.".into());
  }
  if method == "audio.status" && state.owner.is_none() {
    return Ok(idle_status());
  }
  if state.owner.is_none() {
    state.owner = Some(owner.into());
    watch_session(
      app.clone(),
      host.audio.clone(),
      owner.into(),
      session.clone(),
    );
  }
  let applet_id = session.manifest.applet_id.clone();
  let files = host.files.clone();
  match method {
    "audio.start" | "audio.import" => {
      if state.recording_path.is_some() {
        return Err("Stop the current recording first.".into());
      }
      let importing = method == "audio.import";
      let relative = format!("recordings/{}.m4a", Uuid::new_v4());
      let target = relative.clone();
      let applet = applet_id.clone();
      let path = tauri::async_runtime::spawn_blocking(move || {
        files.dispatch(&applet, "fs.mkdir", json!({"path":"recordings"}))?;
        files.native_path(
          &applet,
          if importing { "recordings" } else { &target },
          !importing,
        )
      })
      .await
      .map_err(|_| "Could not prepare audio storage.")??;
      if importing {
        let imported =
          checked_native(app, owner, &session, "import", json!({"directory":path})).await?;
        if imported.is_null() {
          return Ok(Value::Null);
        }
        let full_path = std::path::Path::new(string(&imported, "path")?);
        let name = full_path
          .strip_prefix(&path)
          .ok()
          .filter(|value| value.components().count() == 1)
          .and_then(|value| value.to_str())
          .ok_or("The imported audio was not saved in private storage.")?;
        let relative = format!("recordings/{name}");
        let files = host.files.clone();
        let relative_check = relative.clone();
        let checked = tauri::async_runtime::spawn_blocking(move || {
          files.native_path(&applet_id, &relative_check, false)
        })
        .await
        .map_err(|_| "Could not inspect imported audio.")??;
        let size_bytes = tokio::fs::metadata(checked)
          .await
          .map_err(|_| "Could not inspect imported audio.")?
          .len();
        if size_bytes == 0 || size_bytes > 128 * 1024 * 1024 {
          return Err("Imported audio must contain between 1 byte and 128 MiB.".into());
        }
        Ok(json!({"path":relative,"file_name":imported["file_name"],
          "mime_type":imported["mime_type"],"duration_ms":imported["duration_ms"],"size_bytes":size_bytes}))
      } else {
        let source = host.view_grant_source(owner)?;
        let microphone = host.microphone.clone();
        let remembered = crate::commands::blocking({
          let applet_id = applet_id.clone();
          let source = source.clone();
          move || microphone.remembered(&applet_id, &source)
        })
        .await?;
        let started = checked_native(
          app.clone(),
          owner,
          &session,
          "start",
          json!({"path":path,"applet_name":session.manifest.name,"remembered":remembered}),
        )
        .await?;
        state.recording_path = Some(relative.clone());
        if started.get("remember").and_then(Value::as_bool) == Some(true) {
          let microphone = host.microphone.clone();
          let saved = crate::commands::blocking(move || {
            session.run_local(|| microphone.remember(&applet_id, &source))
          })
          .await;
          if saved.is_err() {
            if native(app, owner, "reset", json!({})).await.is_ok() {
              state.recording_path = None;
            }
            return Err(
              "Could not save microphone permission. Reload the applet and try again.".into(),
            );
          }
        }
        Ok(json!({"path":relative,"mime_type":"audio/mp4"}))
      }
    }
    "audio.stop" => {
      let path = state
        .recording_path
        .clone()
        .ok_or("No recording is active.")?;
      let stopped = match checked_native(app.clone(), owner, &session, "stop", json!({})).await {
        Ok(stopped) => stopped,
        Err(error) => {
          let status = checked_native(app, owner, &session, "status", json!({})).await;
          state.reconcile_failed_stop(status.as_ref().ok());
          return Err(error);
        }
      };
      state.recording_path = None;
      let checked_path = path.clone();
      let stat = tauri::async_runtime::spawn_blocking(move || {
        files.dispatch(&applet_id, "fs.stat", json!({"path":checked_path}))
      })
      .await
      .map_err(|_| "Could not inspect the recording.")??;
      Ok(
        json!({"path":path,"file_name":"Recording.m4a","mime_type":"audio/mp4",
        "duration_ms":stopped["duration_ms"],"size_bytes":stat["size_bytes"]}),
      )
    }
    "audio.pause" | "audio.resume" => {
      if state.recording_path.is_none() {
        return Err("No recording is active.".into());
      }
      checked_native(
        app,
        owner,
        &session,
        method.trim_start_matches("audio."),
        json!({}),
      )
      .await
    }
    "audio.play" | "audio.share" => {
      if state.recording_path.is_some() {
        return Err("Stop the current recording first.".into());
      }
      let relative = string(&params, "path")?.to_owned();
      let path = tauri::async_runtime::spawn_blocking(move || {
        let path = files.native_path(&applet_id, &relative, false)?;
        if !path.is_file() {
          return Err(String::from("A regular file is required."));
        }
        Ok(path)
      })
      .await
      .map_err(|_| "Could not read the audio file.")??;
      let args = if method == "audio.share" {
        let name = string(&params, "file_name")?;
        if !valid_share_name(name) {
          return Err("Use a simple filename for sharing.".into());
        }
        let mime_type = string(&params, "mime_type")?;
        if mime_type.len() > 100 || mime_type.chars().any(char::is_control) {
          return Err("Invalid media type.".into());
        }
        json!({"path":path,"file_name":name,"mime_type":mime_type})
      } else {
        json!({"path":path})
      };
      checked_native(
        app,
        owner,
        &session,
        method.trim_start_matches("audio."),
        args,
      )
      .await
    }
    "audio.seek" => {
      let position_ms = params
        .get("position_ms")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && (0.0..=86_400_000.0).contains(value))
        .map(|value| value.round() as u64)
        .ok_or("Invalid playback position.")?;
      checked_native(
        app,
        owner,
        &session,
        "seek",
        json!({"position_ms":position_ms}),
      )
      .await
    }
    "audio.playback_pause" | "audio.status" => {
      checked_native(
        app,
        owner,
        &session,
        method.trim_start_matches("audio."),
        json!({}),
      )
      .await
    }
    _ => Err("Unknown audio operation.".into()),
  }
}

fn valid_share_name(name: &str) -> bool {
  !name.is_empty()
    && name.len() <= 200
    && name != "."
    && name != ".."
    && !name
      .chars()
      .any(|ch| ch.is_control() || matches!(ch, '/' | '\\' | ':'))
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn failed_stop_releases_recording_only_after_native_confirms_idle() {
    let mut state = AudioSession {
      owner: Some("view".into()),
      recording_path: Some("recordings/short.m4a".into()),
    };
    state.reconcile_failed_stop(Some(&json!({"recording_state":"idle"})));
    assert!(state.recording_path.is_none());
    assert_eq!(state.owner.as_deref(), Some("view"));
  }

  #[test]
  fn failed_stop_preserves_recording_for_active_or_unavailable_native_state() {
    for status in [
      None,
      Some(json!({})),
      Some(json!({"recording_state":null})),
      Some(json!({"recording_state":"paused"})),
      Some(json!({"recording_state":"recording"})),
    ] {
      let mut state = AudioSession {
        owner: Some("view".into()),
        recording_path: Some("recordings/active.m4a".into()),
      };
      state.reconcile_failed_stop(status.as_ref());
      assert_eq!(
        state.recording_path.as_deref(),
        Some("recordings/active.m4a")
      );
    }
  }

  #[test]
  fn share_names_cannot_escape_the_native_export_directory() {
    for name in ["../secret", "a/b", "a\\b", "", ".", "..", "x\n.txt", "a:b"] {
      assert!(!valid_share_name(name));
    }
    assert!(valid_share_name("Meeting transcript.txt"));
    assert!(valid_share_name("录音.m4a"));
  }
}
