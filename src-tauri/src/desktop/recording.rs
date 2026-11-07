//! View-owned microphone recording. Applet approval and OS authorization are
//! separate, and neither a closed view nor a late callback can begin capture.
use super::{audio, picker::Operation};
use block2::{DynBlock, RcBlock};
use objc2::{
  extern_class, extern_methods,
  rc::{Allocated, Retained},
  runtime::{AnyObject, Bool},
  MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
  NSAlert, NSAlertFirstButtonReturn, NSAlertSecondButtonReturn, NSAlertThirdButtonReturn,
  NSApplication, NSWindow,
};
use objc2_foundation::{NSBundle, NSDictionary, NSError, NSNumber, NSObject, NSString, NSURL};
use serde_json::{json, Value};
use std::{
  cell::RefCell,
  fs::{self, OpenOptions},
  path::PathBuf,
  sync::mpsc,
};
use tauri::{AppHandle, Manager, Runtime};
use uuid::Uuid;

const START_OPERATION: &str = "recording-start";
const CONTROL_OPERATION: &str = "recording-control";
const CANCELLED: &str = "Microphone recording was cancelled.";
const DENIED: &str =
  "Microphone access is denied. Enable Apell in System Settings → Privacy & Security → Microphone.";
const VISIBLE: &str = "Show the Apell window to start or resume recording.";
const MAX_SECONDS: f64 = apell_speech::MAX_AUDIO_SECONDS as f64;

#[link(name = "AVFoundation", kind = "framework")]
extern "C" {
  static AVMediaTypeAudio: &'static NSString;
  static AVFormatIDKey: &'static NSString;
  static AVSampleRateKey: &'static NSString;
  static AVNumberOfChannelsKey: &'static NSString;
  static AVEncoderBitRateKey: &'static NSString;
  static AVEncoderAudioQualityKey: &'static NSString;
}

extern_class!(
  #[unsafe(super(NSObject))]
  #[thread_kind = MainThreadOnly]
  struct AVAudioRecorder;
);

impl AVAudioRecorder {
  extern_methods!(
    #[unsafe(method(initWithURL:settings:error:_))]
    #[unsafe(method_family = init)]
    fn init_with_url(
      this: Allocated<Self>,
      url: &NSURL,
      settings: &NSDictionary<NSString, AnyObject>,
    ) -> Result<Retained<Self>, Retained<NSError>>;

    #[unsafe(method(prepareToRecord))]
    #[unsafe(method_family = none)]
    fn prepare(&self) -> bool;

    #[unsafe(method(recordForDuration:))]
    #[unsafe(method_family = none)]
    fn record_for_duration(&self, duration: f64) -> bool;

    #[unsafe(method(pause))]
    #[unsafe(method_family = none)]
    fn pause(&self);

    #[unsafe(method(stop))]
    #[unsafe(method_family = none)]
    fn stop(&self);

    #[unsafe(method(isRecording))]
    #[unsafe(method_family = none)]
    fn is_recording(&self) -> bool;

    #[unsafe(method(currentTime))]
    #[unsafe(method_family = none)]
    fn current_time(&self) -> f64;
  );
}

extern_class!(
  #[unsafe(super(NSObject))]
  #[thread_kind = MainThreadOnly]
  struct AVCaptureDevice;
);

impl AVCaptureDevice {
  extern_methods!(
    #[unsafe(method(authorizationStatusForMediaType:))]
    #[unsafe(method_family = none)]
    fn authorization_status(media: &NSString) -> isize;

    #[unsafe(method(requestAccessForMediaType:completionHandler:))]
    #[unsafe(method_family = none)]
    fn request_access(media: &NSString, completion: &DynBlock<dyn Fn(Bool)>);
  );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
  Recording,
  Paused,
}

struct Capture {
  owner: String,
  path: PathBuf,
  recorder: Retained<AVAudioRecorder>,
  phase: Phase,
  elapsed_ms: f64,
  resume_ready: bool,
  duration_finalized: bool,
}

impl Capture {
  fn update(&mut self, main: MainThreadMarker) {
    if self.phase == Phase::Recording {
      if self.recorder.is_recording() {
        self.elapsed_ms = elapsed_ms(self.elapsed_ms, self.recorder.current_time());
      } else {
        // An input-device failure or the duration ceiling can stop CoreAudio.
        // currentTime is no longer valid, so use the finalized file below.
        self.phase = Phase::Paused;
        // An automatic stop closes the file. Unlike an explicit pause, calling
        // record again could replace it instead of resuming the same content.
        self.resume_ready = false;
      }
    }
    if !self.resume_ready && !self.duration_finalized {
      if let Ok(duration) = audio::file_duration_ms(&self.path, main) {
        self.elapsed_ms = duration;
        self.duration_finalized = true;
      }
    }
  }
}

type Reply = mpsc::SyncSender<Result<Value, String>>;

struct PendingStart {
  owner: String,
  token: Uuid,
  operation: Operation,
  path: PathBuf,
  remember: bool,
  reply: Reply,
  alert: Option<(Retained<NSAlert>, Retained<NSWindow>)>,
}

#[derive(Default)]
struct RecorderState {
  pending: Option<PendingStart>,
  capture: Option<Capture>,
  finished: Option<(String, f64)>,
}

impl RecorderState {
  fn take_pending(&mut self, owner: &str, token: Uuid) -> Option<PendingStart> {
    if self
      .pending
      .as_ref()
      .is_some_and(|pending| pending_matches(pending, owner, token))
    {
      self.pending.take()
    } else {
      None
    }
  }
}

thread_local! {
  // Every access is on the main thread; no native object is Send or Sync.
  static STATE: RefCell<RecorderState> = RefCell::new(RecorderState::default());
}

fn elapsed_ms(previous: f64, current_seconds: f64) -> f64 {
  if current_seconds.is_finite() && current_seconds >= 0.0 {
    previous.max(current_seconds.min(MAX_SECONDS) * 1000.0)
  } else {
    previous
  }
}

fn remaining_seconds(elapsed: f64) -> Result<f64, String> {
  let remaining = MAX_SECONDS - elapsed / 1000.0;
  if !remaining.is_finite() || remaining <= 0.0 {
    Err("This recording reached its 3-hour limit. Stop it and start a new recording.".into())
  } else {
    Ok(remaining)
  }
}

fn pending_matches(pending: &PendingStart, owner: &str, token: Uuid) -> bool {
  pending.owner == owner && pending.token == token
}

fn finish(owner: &str, token: Uuid, result: Result<Value, String>) {
  let pending = STATE.with_borrow_mut(|state| state.take_pending(owner, token));
  if let Some(pending) = pending {
    let _ = pending.reply.send(result);
  }
}

fn context(owner: &str, token: Uuid) -> Option<Result<(Operation, PathBuf, bool), String>> {
  STATE.with_borrow(|state| {
    let pending = state
      .pending
      .as_ref()
      .filter(|pending| pending_matches(pending, owner, token))?;
    Some(pending.operation.ensure_active().map(|()| {
      (
        pending.operation.clone(),
        pending.path.clone(),
        pending.remember,
      )
    }))
  })
}

fn visible<R: Runtime>(app: &AppHandle<R>, main: MainThreadMarker) -> Result<(), String> {
  let window = app.get_webview_window("main").ok_or(VISIBLE)?;
  if NSApplication::sharedApplication(main).isHidden()
    || !window.is_visible().unwrap_or(false)
    || window.is_minimized().unwrap_or(true)
  {
    return Err(VISIBLE.into());
  }
  Ok(())
}

fn microphone_description() -> Result<(), String> {
  let present = NSBundle::mainBundle()
    .objectForInfoDictionaryKey(&NSString::from_str("NSMicrophoneUsageDescription"))
    .and_then(|value| value.downcast::<NSString>().ok())
    .is_some_and(|description| !description.is_empty());
  if present {
    Ok(())
  } else {
    Err("Use the installed Apell app to record audio; its microphone privacy description is required.".into())
  }
}

fn settings() -> Retained<NSDictionary<NSString, AnyObject>> {
  let format = NSNumber::new_u32(u32::from_be_bytes(*b"aac "));
  let rate = NSNumber::new_f64(44_100.0);
  let channels = NSNumber::new_u32(1);
  let bitrate = NSNumber::new_u32(64_000);
  // AVAudioQualityHigh
  let quality = NSNumber::new_u32(96);
  // SAFETY: AVFoundation exports immutable NSString constants for these keys.
  let keys = unsafe {
    [
      AVFormatIDKey,
      AVSampleRateKey,
      AVNumberOfChannelsKey,
      AVEncoderBitRateKey,
      AVEncoderAudioQualityKey,
    ]
  };
  NSDictionary::from_slices(&keys, &[&*format, &*rate, &*channels, &*bitrate, &*quality])
}

struct NewRecordingFile(Option<PathBuf>);
impl Drop for NewRecordingFile {
  fn drop(&mut self) {
    if let Some(path) = &self.0 {
      let _ = fs::remove_file(path);
    }
  }
}

fn start_capture<R: Runtime>(app: &AppHandle<R>, owner: &str, token: Uuid, main: MainThreadMarker) {
  let Some(current) = context(owner, token) else {
    return;
  };
  let result = (|| {
    let (operation, path, remember) = current?;
    visible(app, main)?;
    operation.ensure_active()?;
    // Reserving the unique host path prevents overwriting an existing file.
    // AVAudioRecorder may replace only this newly created placeholder.
    OpenOptions::new()
      .create_new(true)
      .write(true)
      .open(&path)
      .map_err(|_| "Could not create the private microphone recording.")?;
    let mut cleanup = NewRecordingFile(Some(path.clone()));
    let url = NSURL::from_file_path(&path).ok_or("Private audio storage is unavailable.")?;
    let recorder = AVAudioRecorder::init_with_url(AVAudioRecorder::alloc(main), &url, &settings())
      .map_err(|_| {
        "Could not prepare the microphone recording. Check the selected input device."
      })?;
    if !recorder.prepare() {
      return Err(
        "Could not prepare the microphone recording. Check the selected input device.".into(),
      );
    }
    operation.ensure_active()?;
    audio::stop_playback(owner)?;
    if !recorder.record_for_duration(MAX_SECONDS) {
      return Err(
        "Could not start the microphone recording. Check the selected input device.".into(),
      );
    }
    // A view reset may cancel on a worker while CoreAudio initializes. Stop the
    // actual device before settling that request; no late grant is persisted.
    cleanup.0 = None;
    if let Err(error) = operation.ensure_active() {
      recorder.stop();
      return Err(error);
    }
    STATE.with_borrow_mut(|state| {
      state.finished = None;
      state.capture = Some(Capture {
        owner: owner.into(),
        path,
        recorder,
        phase: Phase::Recording,
        elapsed_ms: 0.0,
        resume_ready: true,
        duration_finalized: false,
      });
    });
    Ok(json!({"remember":remember}))
  })();
  finish(owner, token, result);
}

fn authorize<R: Runtime>(app: &AppHandle<R>, owner: &str, token: Uuid, main: MainThreadMarker) {
  let Some(current) = context(owner, token) else {
    return;
  };
  if let Err(error) = current {
    finish(owner, token, Err(error));
    return;
  }
  // SAFETY: This is the immutable AVFoundation audio media-type constant.
  let media = unsafe { AVMediaTypeAudio };
  match AVCaptureDevice::authorization_status(media) {
    3 => start_capture(app, owner, token, main),
    0 => {
      let handle = app.clone();
      let callback_owner = owner.to_owned();
      // AVFoundation invokes this block on an arbitrary queue. Capture no native
      // objects, and return to the main thread before inspecting recorder state.
      let completion = RcBlock::new(move |granted: Bool| {
        let callback_handle = handle.clone();
        let owner = callback_owner.clone();
        let _ = handle.run_on_main_thread(move || {
          let Some(main) = MainThreadMarker::new() else {
            finish(
              &owner,
              token,
              Err("The microphone service is unavailable.".into()),
            );
            return;
          };
          if granted.as_bool() {
            start_capture(&callback_handle, &owner, token, main);
          } else {
            finish(&owner, token, Err(DENIED.into()));
          }
        });
      });
      AVCaptureDevice::request_access(media, &completion);
    }
    _ => finish(owner, token, Err(DENIED.into())),
  }
}

fn present_start<R: Runtime>(
  app: AppHandle<R>,
  owner: String,
  params: Value,
  operation: Operation,
  reply: Reply,
  main: MainThreadMarker,
) {
  let prepare = (|| {
    operation.ensure_active()?;
    visible(&app, main)?;
    microphone_description()?;
    let path = params
      .get("path")
      .and_then(Value::as_str)
      .filter(|value| !value.is_empty())
      .map(PathBuf::from)
      .ok_or("Invalid microphone recording request.")?;
    let remembered = params
      .get("remembered")
      .and_then(Value::as_bool)
      .ok_or("Invalid microphone recording request.")?;
    let applet_name = params
      .get("applet_name")
      .and_then(Value::as_str)
      .filter(|value| !value.is_empty())
      .ok_or("Invalid microphone recording request.")?;
    let busy = STATE.with_borrow(|state| state.pending.is_some() || state.capture.is_some());
    if busy {
      return Err("A microphone recording is already active.".to_string());
    }
    let token = Uuid::new_v4();
    let presentation = if remembered {
      None
    } else {
      let application = NSApplication::sharedApplication(main);
      let parent = application
        .mainWindow()
        .or_else(|| application.keyWindow())
        .ok_or("Microphone confirmation is unavailable.")?;
      if parent.attachedSheet().is_some() {
        return Err("Finish the current native prompt before recording.".into());
      }
      let alert = NSAlert::new(main);
      alert.setMessageText(&NSString::from_str("Allow microphone recording?"));
      alert.setInformativeText(&NSString::from_str(&format!(
        "{applet_name} wants to start recording audio."
      )));
      alert.addButtonWithTitle(&NSString::from_str("Allow Once"));
      alert.addButtonWithTitle(&NSString::from_str("Remember for This Applet"));
      alert.addButtonWithTitle(&NSString::from_str("Cancel"));
      Some((alert, parent))
    };
    Ok((path, token, presentation))
  })();
  let (path, token, presentation) = match prepare {
    Ok(prepared) => prepared,
    Err(error) => {
      let _ = reply.send(Err(error));
      return;
    }
  };
  STATE.with_borrow_mut(|state| {
    state.pending = Some(PendingStart {
      owner: owner.clone(),
      token,
      operation,
      path,
      remember: false,
      reply,
      alert: presentation.clone(),
    });
  });
  if let Some((alert, parent)) = presentation {
    let handle = app.clone();
    let callback_owner = owner.clone();
    let completion = RcBlock::new(move |response| {
      let accepted = if response == NSAlertFirstButtonReturn {
        Some(false)
      } else if response == NSAlertSecondButtonReturn {
        Some(true)
      } else {
        None
      };
      let Some(remember) = accepted else {
        finish(&callback_owner, token, Err(CANCELLED.into()));
        return;
      };
      let current = STATE.with_borrow_mut(|state| {
        let pending = state
          .pending
          .as_mut()
          .filter(|pending| pending_matches(pending, &callback_owner, token));
        if let Some(pending) = pending {
          pending.alert = None;
          pending.remember = remember;
          true
        } else {
          false
        }
      });
      if current {
        if let Some(main) = MainThreadMarker::new() {
          authorize(&handle, &callback_owner, token, main);
        }
      }
    });
    alert.beginSheetModalForWindow_completionHandler(&parent, Some(&completion));
  } else {
    authorize(&app, &owner, token, main);
  }
}

pub(super) fn snapshot(owner: &str, main: MainThreadMarker) -> Result<Value, String> {
  STATE.with_borrow_mut(|state| {
    if let Some(capture) = state
      .capture
      .as_mut()
      .filter(|capture| capture.owner == owner)
    {
      capture.update(main);
      return Ok(json!({
        "recording_state":if capture.phase == Phase::Recording {"recording"} else {"paused"},
        "elapsed_ms":capture.elapsed_ms,
      }));
    }
    let elapsed = state
      .finished
      .as_ref()
      .filter(|(previous_owner, _)| previous_owner == owner)
      .map(|(_, elapsed)| *elapsed)
      .unwrap_or(0.0);
    Ok(json!({"recording_state":"idle","elapsed_ms":elapsed}))
  })
}

fn stop_owned(owner: Option<&str>, main: MainThreadMarker) {
  let pending = STATE.with_borrow_mut(|state| {
    if state
      .pending
      .as_ref()
      .is_some_and(|pending| owner.is_none_or(|owner| pending.owner == owner))
    {
      state.pending.take()
    } else {
      None
    }
  });
  if let Some(pending) = pending {
    // Drop the state borrow before native sheet dismissal can reenter AppKit.
    if let Some((alert, parent)) = &pending.alert {
      parent.endSheet_returnCode(&alert.window(), NSAlertThirdButtonReturn);
    }
    let _ = pending.reply.send(Err(CANCELLED.into()));
  }
  let capture = STATE.with_borrow_mut(|state| {
    if state
      .capture
      .as_ref()
      .is_some_and(|capture| owner.is_none_or(|owner| capture.owner == owner))
    {
      state.capture.take()
    } else {
      None
    }
  });
  if let Some(mut capture) = capture {
    capture.update(main);
    capture.recorder.stop();
    if let Ok(duration) = audio::file_duration_ms(&capture.path, main) {
      capture.elapsed_ms = duration;
    }
    STATE.with_borrow_mut(|state| {
      state.finished = Some((capture.owner, capture.elapsed_ms));
    });
  }
}

/// Actual process exit finalizes M4A data; hiding the window does not call this.
pub(super) fn shutdown(main: MainThreadMarker) {
  let owner =
    STATE.with_borrow(|state| state.pending.as_ref().map(|pending| pending.owner.clone()));
  if let Some(owner) = owner {
    let _ = super::picker::cancel_active(&owner, START_OPERATION);
  }
  stop_owned(None, main);
}

pub fn call<R: Runtime>(
  app: &AppHandle<R>,
  owner: &str,
  method: &str,
  params: Value,
) -> Result<Value, String> {
  if method == "start" {
    let operation = Operation::begin(owner, START_OPERATION)?;
    let (reply, receiver) = mpsc::sync_channel(1);
    let handle = app.clone();
    let owner = owner.to_owned();
    app
      .run_on_main_thread(move || {
        let Some(main) = MainThreadMarker::new() else {
          let _ = reply.send(Err("The microphone service is unavailable.".into()));
          return;
        };
        present_start(handle, owner, params, operation, reply, main);
      })
      .map_err(|_| "The microphone service is unavailable.")?;
    return receiver
      .recv()
      .map_err(|_| "The microphone request was interrupted.".to_string())?;
  }
  let operation = if method == "reset" {
    super::picker::cancel_active(owner, START_OPERATION)?;
    super::picker::cancel_active(owner, CONTROL_OPERATION)?;
    None
  } else {
    Some(Operation::begin(owner, CONTROL_OPERATION)?)
  };
  let owner = owner.to_owned();
  let method = method.to_owned();
  let handle = app.clone();
  audio::on_main(app, move |main| {
    if let Some(operation) = &operation {
      operation.ensure_active()?;
    }
    if method == "status" {
      return snapshot(&owner, main);
    }
    if method == "reset" {
      // The audio adapter retires the shared operation registry first when this
      // is a real view close. Rollback resets intentionally keep that owner open.
      stop_owned(Some(&owner), main);
      return Ok(Value::Null);
    }
    if method == "resume" {
      visible(&handle, main)?;
      // SAFETY: Read the immutable AVFoundation audio media-type constant.
      if AVCaptureDevice::authorization_status(unsafe { AVMediaTypeAudio }) != 3 {
        return Err(DENIED.into());
      }
    }
    STATE.with_borrow_mut(|state| {
      let capture = state
        .capture
        .as_mut()
        .filter(|capture| capture.owner == owner)
        .ok_or("No microphone recording is active.")?;
      capture.update(main);
      match method.as_str() {
        "pause" => {
          if capture.phase != Phase::Recording {
            return Err("No microphone recording is running.".into());
          }
          capture.recorder.pause();
          capture.phase = Phase::Paused;
          Ok(Value::Null)
        }
        "resume" => {
          if capture.phase != Phase::Paused {
            return Err("No microphone recording is paused.".into());
          }
          let remaining = remaining_seconds(capture.elapsed_ms)?;
          if !capture.resume_ready {
            return Err("The input device stopped recording. Stop and save this recording before starting another.".into());
          }
          let operation = operation.as_ref().ok_or(CANCELLED)?;
          operation.ensure_active()?;
          if !capture.recorder.record_for_duration(remaining) {
            return Err(
              "Could not resume the microphone recording. Check the selected input device.".into(),
            );
          }
          capture.phase = Phase::Recording;
          if let Err(error) = operation.ensure_active() {
            // Cancellation can arrive from a worker while CoreAudio resumes.
            // Release the microphone immediately and preserve its finalized
            // partial file for the retiring view's reset/recovery path.
            capture.update(main);
            capture.recorder.stop();
            capture.phase = Phase::Paused;
            capture.resume_ready = false;
            capture.duration_finalized = false;
            capture.update(main);
            return Err(error);
          }
          Ok(Value::Null)
        }
        "stop" => {
          capture.recorder.stop();
          capture.phase = Phase::Paused;
          capture.resume_ready = false;
          // Final file metadata remains authoritative even if the frontend was
          // hidden throughout the capture or CoreAudio stopped between polls.
          let elapsed = audio::file_duration_ms(&capture.path, main)?;
          state.capture = None;
          state.finished = Some((owner, elapsed));
          Ok(json!({"duration_ms":elapsed}))
        }
        _ => Err("Unknown microphone operation.".into()),
      }
    })
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn recording_clock_does_not_rewind_on_pause_device_failure_or_invalid_native_time() {
    assert_eq!(elapsed_ms(5_000.0, 6.25), 6_250.0);
    for native in [0.0, -1.0, f64::NAN, f64::INFINITY] {
      assert_eq!(elapsed_ms(5_000.0, native), 5_000.0);
    }
    assert_eq!(elapsed_ms(0.0, MAX_SECONDS + 100.0), MAX_SECONDS * 1000.0);
  }

  #[test]
  fn resuming_uses_only_the_remaining_capture_budget() {
    assert_eq!(remaining_seconds(1_500.0).unwrap(), MAX_SECONDS - 1.5);
    assert!(remaining_seconds(MAX_SECONDS * 1000.0).is_err());
    assert!(remaining_seconds(f64::NAN).is_err());
  }

  #[test]
  fn a_late_permission_reply_cannot_complete_a_replacement_start() {
    let owner = Uuid::new_v4().to_string();
    let old_token = Uuid::new_v4();
    let replacement_token = Uuid::new_v4();
    let (reply, _receiver) = mpsc::sync_channel(1);
    let mut state = RecorderState {
      pending: Some(PendingStart {
        owner: owner.clone(),
        token: replacement_token,
        operation: Operation::begin(&owner, START_OPERATION).unwrap(),
        path: PathBuf::from("recording.m4a"),
        remember: false,
        reply,
        alert: None,
      }),
      ..Default::default()
    };
    assert!(state.take_pending(&owner, old_token).is_none());
    assert!(state
      .take_pending("another-view", replacement_token)
      .is_none());
    assert!(state.pending.is_some());
    assert!(state.take_pending(&owner, replacement_token).is_some());
    assert!(state.pending.is_none());
  }
}
