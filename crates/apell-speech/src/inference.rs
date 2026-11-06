use crate::{check_cancelled, decode::SAMPLE_RATE, model::MODEL_ID, Transcript, TranscriptSegment};
use std::{
  ffi::c_void,
  panic::{catch_unwind, AssertUnwindSafe},
  path::Path,
  sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
  },
};
use whisper_rs::{
  FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperSysContext,
  WhisperSysState,
};

pub(crate) fn normalize_language(language: Option<&str>) -> Result<Option<String>, String> {
  match language {
    None | Some("") | Some("auto") => Ok(None),
    Some(language) if language.len() <= 16 && !language.contains('\0') => {
      whisper_rs::get_lang_id(language)
        .and_then(whisper_rs::get_lang_str)
        .map(|code| Some(code.to_owned()))
        .ok_or_else(|| "Choose a supported transcription language or automatic detection.".into())
    }
    _ => Err("Choose a supported transcription language or automatic detection.".into()),
  }
}

struct Callbacks<F> {
  cancel: Arc<AtomicBool>,
  progress: Mutex<F>,
}

// These callbacks borrow a stable box owned for the entire synchronous `full`
// call. They never access Whisper's context/state and cannot unwind across FFI.
unsafe extern "C" fn abort<F>(data: *mut c_void) -> bool {
  let callbacks = unsafe { &*(data.cast::<Callbacks<F>>()) };
  callbacks.cancel.load(Ordering::Acquire)
}

unsafe extern "C" fn progress<F: Fn(u32)>(
  _: *mut WhisperSysContext,
  _: *mut WhisperSysState,
  percent: i32,
  data: *mut c_void,
) {
  let callbacks = unsafe { &*(data.cast::<Callbacks<F>>()) };
  if catch_unwind(AssertUnwindSafe(|| {
    let observer = callbacks
      .progress
      .lock()
      .unwrap_or_else(|error| error.into_inner());
    observer(percent.clamp(0, 100) as u32)
  }))
  .is_err()
  {
    callbacks.cancel.store(true, Ordering::Release);
  }
}

pub(crate) fn transcribe<F: Fn(u32) + Send + 'static>(
  model: &Path,
  mut samples: Vec<f32>,
  language: Option<&str>,
  cancel: Arc<AtomicBool>,
  observer: F,
) -> Result<Transcript, String> {
  check_cancelled(&cancel)?;
  let duration_ms = samples.len() as u64 * 1_000 / SAMPLE_RATE as u64;
  // Whisper can hallucinate words on digital silence. Skip inaudible input,
  // including constant DC offset, and do not invent a detected language.
  if !has_signal(&samples) {
    return Ok(Transcript {
      text: String::new(),
      language: language.unwrap_or("und").into(),
      segments: Vec::new(),
      duration_ms,
      model_id: MODEL_ID.into(),
    });
  }
  // Whisper needs at least one second for its mel input; report original duration.
  samples.resize(samples.len().max(SAMPLE_RATE), 0.0);
  let mut context_params = WhisperContextParameters::default();
  context_params.use_gpu(false);
  let context = WhisperContext::new_with_params(model, context_params)
    .map_err(|_| "Could not load the offline speech model.")?;
  check_cancelled(&cancel)?;
  let mut state = context
    .create_state()
    .map_err(|_| "Could not allocate memory for transcription.")?;
  let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
  let threads = std::thread::available_parallelism()
    .map(usize::from)
    .unwrap_or(2)
    .clamp(1, 4);
  params.set_n_threads(threads as i32);
  params.set_language(language);
  params.set_translate(false);
  params.set_no_context(true);
  params.set_print_special(false);
  params.set_print_progress(false);
  params.set_print_realtime(false);
  params.set_print_timestamps(false);
  params.set_suppress_blank(true);
  let mut callbacks = Box::new(Callbacks {
    cancel: cancel.clone(),
    progress: Mutex::new(observer),
  });
  let data = (&mut *callbacks as *mut Callbacks<F>).cast::<c_void>();
  // SAFETY: the box outlives `full`, which invokes callbacks synchronously.
  // Callback pointer types match their data exactly; no allocation is leaked.
  // Avoid whisper-rs 0.16's type-erased safe abort helper.
  unsafe {
    params.set_abort_callback(Some(abort::<F>));
    params.set_abort_callback_user_data(data);
    params.set_progress_callback(Some(progress::<F>));
    params.set_progress_callback_user_data(data);
  }
  let result = state.full(params, &samples);
  check_cancelled(&cancel)?;
  result.map_err(|_| "Local transcription failed. Try a shorter recording.")?;
  let language = whisper_rs::get_lang_str(state.full_lang_id_from_state())
    .unwrap_or("und")
    .to_owned();
  let mut segments = Vec::new();
  for segment in state.as_iter() {
    let text = segment
      .to_str_lossy()
      .map_err(|_| "Could not read the transcript.")?
      .trim()
      .to_owned();
    if text.is_empty() {
      continue;
    }
    let start_ms = (segment.start_timestamp().max(0) as u64 * 10).min(duration_ms);
    let end_ms = (segment.end_timestamp().max(0) as u64 * 10)
      .min(duration_ms)
      .max(start_ms);
    segments.push(TranscriptSegment {
      start_ms,
      end_ms,
      text,
    });
  }
  let text = segments
    .iter()
    .map(|s| s.text.as_str())
    .collect::<Vec<_>>()
    .join(" ");
  Ok(Transcript {
    text,
    language,
    segments,
    duration_ms,
    model_id: MODEL_ID.into(),
  })
}

fn has_signal(samples: &[f32]) -> bool {
  if samples.is_empty() {
    return false;
  }
  let count = samples.len() as f64;
  let mean = samples.iter().map(|value| f64::from(*value)).sum::<f64>() / count;
  let variance = samples
    .iter()
    .map(|value| (f64::from(*value) - mean).powi(2))
    .sum::<f64>()
    / count;
  variance >= 1.0e-8
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn silence_and_dc_offset_return_empty_without_loading_model() {
    for sample in [0.0, 0.1] {
      let result = transcribe(
        Path::new("missing-model.bin"),
        vec![sample; SAMPLE_RATE],
        None,
        Arc::new(AtomicBool::new(false)),
        |_| {},
      )
      .unwrap();
      assert!(result.text.is_empty());
      assert!(result.segments.is_empty());
      assert_eq!(result.language, "und");
      assert_eq!(result.duration_ms, 1_000);
    }
  }

  #[test]
  fn rejects_unrecognized_or_nul_language() {
    assert_eq!(normalize_language(None).unwrap(), None);
    assert_eq!(normalize_language(Some("auto")).unwrap(), None);
    assert_eq!(
      normalize_language(Some("en")).unwrap().as_deref(),
      Some("en")
    );
    assert!(normalize_language(Some("not-a-language")).is_err());
    assert!(normalize_language(Some("en\0")).is_err());
  }

  #[test]
  fn callbacks_cancel_and_contain_observer_panics() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut state = Box::new(Callbacks {
      cancel: cancelled.clone(),
      progress: Mutex::new(|_: u32| panic!("observer failed")),
    });
    fn test_callbacks<F: Fn(u32)>(state: &mut Callbacks<F>) {
      let data = (state as *mut Callbacks<F>).cast::<c_void>();
      unsafe {
        assert!(!abort::<F>(data));
        progress::<F>(std::ptr::null_mut(), std::ptr::null_mut(), 50, data);
        assert!(abort::<F>(data));
      }
    }
    test_callbacks(&mut state);
    assert!(cancelled.load(Ordering::Acquire));
  }
}
