//! macOS file import and view-owned playback. Native paths never reach applets.
use super::picker::{self, copy_file, Operation};
use objc2::{
  extern_class, extern_methods,
  rc::{Allocated, Retained},
  MainThreadMarker, MainThreadOnly,
};
use objc2_foundation::{NSError, NSObject, NSURL};
use serde_json::{json, Value};
use std::{
  cell::RefCell,
  fs::{self, File},
  io::Read,
  path::{Path, PathBuf},
  sync::mpsc,
};
use tauri::{AppHandle, Runtime};
use uuid::Uuid;

const IMPORT_OPERATION: &str = "audio-import";
const PLAYBACK_OPERATION: &str = "audio-playback";
const AUDIO_EXTENSIONS: &[&str] = &["wav", "mp3", "m4a", "aac", "aif", "aiff", "flac", "caf"];

// AVFoundation exports this player across the supported macOS versions. Bind
// only its small surface; AVAudioSession is an iOS-only audio policy API.
#[link(name = "AVFoundation", kind = "framework")]
extern "C" {}

extern_class!(
  #[unsafe(super(NSObject))]
  #[thread_kind = MainThreadOnly]
  struct AVAudioPlayer;
);

impl AVAudioPlayer {
  extern_methods!(
    #[unsafe(method(initWithContentsOfURL:error:_))]
    #[unsafe(method_family = init)]
    fn init_with_url(
      this: Allocated<Self>,
      url: &NSURL,
    ) -> Result<Retained<Self>, Retained<NSError>>;

    #[unsafe(method(play))]
    #[unsafe(method_family = none)]
    fn play(&self) -> bool;

    #[unsafe(method(pause))]
    #[unsafe(method_family = none)]
    fn pause(&self);

    #[unsafe(method(stop))]
    #[unsafe(method_family = none)]
    fn stop(&self);

    #[unsafe(method(isPlaying))]
    #[unsafe(method_family = none)]
    fn is_playing(&self) -> bool;

    #[unsafe(method(duration))]
    #[unsafe(method_family = none)]
    fn duration(&self) -> f64;

    #[unsafe(method(currentTime))]
    #[unsafe(method_family = none)]
    fn current_time(&self) -> f64;

    #[unsafe(method(setCurrentTime:))]
    #[unsafe(method_family = none)]
    fn set_current_time(&self, seconds: f64);

    #[unsafe(method(numberOfChannels))]
    #[unsafe(method_family = none)]
    fn channels(&self) -> usize;
  );
}

struct Playback {
  owner: String,
  path: PathBuf,
  player: Retained<AVAudioPlayer>,
}

thread_local! {
  // Accessed only through on_main; Retained<AVAudioPlayer> never crosses a thread.
  static PLAYBACK: RefCell<Option<Playback>> = const { RefCell::new(None) };
}

fn on_main<R: Runtime, T: Send + 'static>(
  app: &AppHandle<R>,
  action: impl FnOnce(MainThreadMarker) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
  if let Some(main) = MainThreadMarker::new() {
    return action(main);
  }
  let (sender, receiver) = mpsc::channel();
  app
    .run_on_main_thread(move || {
      let result = MainThreadMarker::new()
        .ok_or_else(|| "The audio service is unavailable.".to_string())
        .and_then(action);
      let _ = sender.send(result);
    })
    .map_err(|_| "The audio service is unavailable.")?;
  receiver
    .recv()
    .map_err(|_| "The audio operation was interrupted.".to_string())?
}

fn string<'a>(params: &'a Value, field: &str) -> Result<&'a str, String> {
  params
    .get(field)
    .and_then(Value::as_str)
    .filter(|value| !value.is_empty())
    .ok_or_else(|| "Invalid native audio request.".into())
}

fn duration_ms(duration: f64) -> Result<f64, String> {
  if !duration.is_finite() || duration <= 0.0 || duration > apell_speech::MAX_AUDIO_SECONDS as f64 {
    return Err("Choose a supported audio file no longer than 3 hours.".into());
  }
  Ok(duration * 1000.0)
}

fn load_player(path: &Path, main: MainThreadMarker) -> Result<Retained<AVAudioPlayer>, String> {
  let metadata = fs::metadata(path).map_err(|_| "The audio file is unavailable.")?;
  if !metadata.is_file() || metadata.len() == 0 || metadata.len() > apell_speech::MAX_AUDIO_BYTES {
    return Err("Choose a nonempty audio file no larger than 2 GiB.".into());
  }
  let url = NSURL::from_file_path(path).ok_or("The audio file is unavailable.")?;
  let player = AVAudioPlayer::init_with_url(AVAudioPlayer::alloc(main), &url)
    .map_err(|_| "This audio file could not be opened.")?;
  if player.channels() == 0 {
    return Err("This file does not contain supported audio.".into());
  }
  duration_ms(player.duration())?;
  Ok(player)
}

/// Determine the returned MIME from stored bytes; a filename is only a picker hint.
fn mime_type(header: &[u8]) -> Option<&'static str> {
  let word = |offset: usize, value: &[u8]| header.get(offset..offset + value.len()) == Some(value);
  if word(0, b"RIFF") && word(8, b"WAVE") {
    return Some("audio/wav");
  }
  if word(4, b"ftyp") {
    return Some("audio/mp4");
  }
  if word(0, b"fLaC") {
    return Some("audio/flac");
  }
  if word(0, b"FORM") && (word(8, b"AIFF") || word(8, b"AIFC")) {
    return Some("audio/aiff");
  }
  if word(0, b"caff") {
    return Some("audio/x-caf");
  }
  let id3 = word(0, b"ID3")
    && header.len() >= 10
    && (2..=4).contains(&header[3])
    && header[4] != 0xff
    && header[6..10].iter().all(|byte| *byte < 0x80);
  let mp3 = header.len() >= 4
    && header[0] == 0xff
    && header[1] & 0xe0 == 0xe0
    && header[1] & 0x18 != 0x08
    && header[1] & 0x06 == 0x02
    && header[2] & 0xf0 != 0xf0
    && header[2] & 0x0c != 0x0c;
  if id3 || mp3 {
    return Some("audio/mpeg");
  }
  if header.len() >= 2 && header[0] == 0xff && header[1] & 0xf6 == 0xf0 {
    return Some("audio/aac");
  }
  None
}

struct PendingFile(Option<PathBuf>);
impl Drop for PendingFile {
  fn drop(&mut self) {
    if let Some(path) = &self.0 {
      let _ = fs::remove_file(path);
    }
  }
}

fn import<R: Runtime>(app: &AppHandle<R>, owner: &str, params: &Value) -> Result<Value, String> {
  let operation = Operation::begin(owner, IMPORT_OPERATION)?;
  let directory = PathBuf::from(string(params, "directory")?);
  if !fs::metadata(&directory).is_ok_and(|metadata| metadata.is_dir()) {
    return Err("Private audio storage is unavailable.".into());
  }
  let Some(source) = picker::choose(app, &operation, "Choose an audio file", AUDIO_EXTENSIONS)?
  else {
    return Ok(Value::Null);
  };
  operation.ensure_active()?;
  let extension = source
    .extension()
    .and_then(|value| value.to_str())
    .filter(|value| {
      !value.is_empty()
        && value.len() <= 8
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
    })
    .map(str::to_ascii_lowercase)
    .unwrap_or_else(|| "audio".into());
  let path = directory.join(format!("{}.{}", Uuid::new_v4(), extension));
  let size_bytes = copy_file(&source, &path, apell_speech::MAX_AUDIO_BYTES, &operation)?;
  let mut pending = PendingFile(Some(path.clone()));
  let mut header = [0u8; 32];
  let length = size_bytes.min(header.len() as u64) as usize;
  File::open(&path)
    .and_then(|mut file| file.read_exact(&mut header[..length]))
    .map_err(|_| "Could not inspect the imported audio.")?;
  let mime_type = mime_type(&header[..length]).ok_or("Choose a supported audio file.")?;
  let duration_ms = on_main(app, {
    let path = path.clone();
    let operation = operation.clone();
    move |main| {
      operation.ensure_active()?;
      let player = load_player(&path, main)?;
      duration_ms(player.duration())
    }
  })?;
  let result = json!({
    "path":path.to_str().ok_or("Private audio storage is unavailable.")?,
    "file_name":source.file_name().map(|name| name.to_string_lossy()).unwrap_or_default(),
    "mime_type":mime_type,
    "duration_ms":duration_ms,
    "size_bytes":size_bytes,
  });
  // Reset can arrive while CoreAudio inspects the copied file. Do not publish it
  // after that session closes, and keep cleanup armed until the final check.
  operation.ensure_active()?;
  pending.0 = None;
  Ok(result)
}

fn status(playback: Option<&Playback>) -> Value {
  let player = playback.map(|playback| &playback.player);
  json!({
    "recording_state":"idle",
    "elapsed_ms":0,
    "playing":player.is_some_and(|player| player.is_playing()),
    "position_ms":player.map(|player| player.current_time() * 1000.0).unwrap_or(0.0),
    "duration_ms":player.map(|player| player.duration() * 1000.0).unwrap_or(0.0),
  })
}

pub fn call<R: Runtime>(app: &AppHandle<R>, method: &str, params: Value) -> Result<Value, String> {
  let owner = string(&params, "session_id")?.to_owned();
  if method == "reset" {
    // Close the shared registry first, including a reset that beats import/play
    // to the main queue. The picker must settle before host ownership is released.
    picker::cancel(app, &owner, None)?;
    return on_main(app, move |_| {
      PLAYBACK.with_borrow_mut(|loaded| {
        if loaded
          .as_ref()
          .is_some_and(|playback| playback.owner == owner)
        {
          if let Some(playback) = loaded.take() {
            playback.player.stop();
          }
        }
      });
      Ok(Value::Null)
    });
  }
  if method == "import" {
    return import(app, &owner, &params);
  }
  if !matches!(method, "play" | "playback_pause" | "seek" | "status") {
    return Err("This audio operation is unavailable on macOS.".into());
  }
  let operation = Operation::begin(&owner, PLAYBACK_OPERATION)?;
  let method = method.to_owned();
  on_main(app, move |main| {
    operation.ensure_active()?;
    PLAYBACK.with_borrow_mut(|loaded| {
      if loaded
        .as_ref()
        .is_some_and(|playback| playback.owner != owner)
      {
        return Err("Audio is owned by another applet view.".into());
      }
      match method.as_str() {
        "play" => {
          let path = PathBuf::from(string(&params, "path")?);
          if loaded.as_ref().is_none_or(|playback| playback.path != path) {
            let player = load_player(&path, main)?;
            operation.ensure_active()?;
            if let Some(previous) = loaded.take() {
              previous.player.stop();
            }
            *loaded = Some(Playback {
              owner,
              path,
              player,
            });
          }
          let player = &loaded
            .as_ref()
            .ok_or("The audio file is unavailable.")?
            .player;
          if player.current_time() >= player.duration() {
            player.set_current_time(0.0);
          }
          if !player.play() {
            return Err("This audio file could not be played.".into());
          }
          Ok(Value::Null)
        }
        "playback_pause" => {
          if let Some(playback) = loaded {
            playback.player.pause();
          }
          Ok(Value::Null)
        }
        "seek" => {
          let seconds = params
            .get("position_ms")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && (0.0..=86_400_000.0).contains(value))
            .ok_or("Invalid playback position.")?
            / 1000.0;
          let player = &loaded
            .as_ref()
            .ok_or("Load an audio file before seeking.")?
            .player;
          player.set_current_time(seconds.min(player.duration()));
          Ok(Value::Null)
        }
        "status" => Ok(status(loaded.as_ref())),
        _ => unreachable!(),
      }
    })
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn imported_audio_duration_must_be_finite_positive_and_bounded() {
    for duration in [0.0, -1.0, f64::NAN, f64::INFINITY, 10_800.001] {
      assert!(duration_ms(duration).is_err());
    }
    assert_eq!(duration_ms(0.125).unwrap(), 125.0);
    assert_eq!(duration_ms(10_800.0).unwrap(), 10_800_000.0);
  }

  #[test]
  fn imported_audio_mime_comes_from_content_instead_of_filename() {
    assert_eq!(mime_type(b"RIFF\0\0\0\0WAVE"), Some("audio/wav"));
    assert_eq!(mime_type(b"ID3\x04\0\0\0\0\0\0"), Some("audio/mpeg"));
    assert_eq!(mime_type(&[0xff, 0xfb, 0x90, 0]), Some("audio/mpeg"));
    assert_eq!(mime_type(&[0xff, 0xf1, 0x50, 0]), Some("audio/aac"));
    assert_eq!(mime_type(b"\0\0\0\x18ftypM4A "), Some("audio/mp4"));
    assert_eq!(mime_type(b"fLaC"), Some("audio/flac"));
    assert_eq!(mime_type(b"FORM\0\0\0\0AIFF"), Some("audio/aiff"));
    assert_eq!(mime_type(b"caff"), Some("audio/x-caf"));
    for bytes in [
      b"not audio".as_slice(),
      b"ID3\x05\0\0\0\0\0\0",
      &[0xff, 0xeb, 0xf0, 0],
      &[],
    ] {
      assert_eq!(mime_type(bytes), None);
    }
  }
}
