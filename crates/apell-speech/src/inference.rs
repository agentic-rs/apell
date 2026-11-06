use crate::{
  check_cancelled,
  chunks::{AudioChunk, AudioChunks},
  decode::{self, SAMPLE_RATE},
  model::MODEL_ID,
  Transcript, TranscriptSegment, MAX_AUDIO_SECONDS,
};
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
  FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
  WhisperSysContext, WhisperSysState,
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

// A stable box owns callback state for the synchronous full() call. No callback
// may unwind over FFI, and cancellation stays live during each inference chunk.
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
  audio: &Path,
  language: Option<&str>,
  cancel: Arc<AtomicBool>,
  observer: F,
) -> Result<Transcript, String> {
  let mut engine = ChunkInference {
    model,
    state: None,
    language: language.map(str::to_owned),
    cancel: cancel.clone(),
    observer: Arc::new(Mutex::new(observer)),
    segments: Vec::new(),
    covered_until_ms: 0,
    processed_samples: 0,
  };
  let mut chunks = AudioChunks::default();
  let count = decode::stream(audio, &cancel, |block, estimate| {
    chunks.push(block, &mut |chunk| engine.process(chunk, estimate))
  })?;
  chunks.finish(&mut |chunk| engine.process(chunk, Some(count)))?;
  check_cancelled(&cancel)?;
  engine.report(100)?;
  let duration_ms = samples_to_ms(count);
  // Header estimates never determine output timing; decoded sample count does.
  for segment in &mut engine.segments {
    segment.start_ms = segment.start_ms.min(duration_ms);
    segment.end_ms = segment.end_ms.min(duration_ms).max(segment.start_ms);
  }
  Ok(Transcript {
    text: engine
      .segments
      .iter()
      .map(|segment| segment.text.as_str())
      .collect::<Vec<_>>()
      .join(" "),
    language: engine.language.unwrap_or_else(|| "und".into()),
    segments: engine.segments,
    duration_ms,
    model_id: MODEL_ID.into(),
  })
}

struct ChunkInference<'a, F> {
  model: &'a Path,
  // WhisperState retains its context via Arc. Load both once, lazily at the first
  // audible window; digital silence does not need to allocate the model at all.
  state: Option<WhisperState>,
  language: Option<String>,
  cancel: Arc<AtomicBool>,
  observer: Arc<Mutex<F>>,
  segments: Vec<TranscriptSegment>,
  covered_until_ms: u64,
  processed_samples: u64,
}

impl<F: Fn(u32) + Send + 'static> ChunkInference<'_, F> {
  fn report(&self, value: u32) -> Result<(), String> {
    if catch_unwind(AssertUnwindSafe(|| {
      let observer = self
        .observer
        .lock()
        .unwrap_or_else(|error| error.into_inner());
      observer(value);
    }))
    .is_err()
    {
      self.cancel.store(true, Ordering::Release);
    }
    check_cancelled(&self.cancel)
  }

  fn process(&mut self, chunk: AudioChunk<'_>, estimate: Option<u64>) -> Result<(), String> {
    check_cancelled(&self.cancel)?;
    let total = estimate
      .unwrap_or(MAX_AUDIO_SECONDS * SAMPLE_RATE as u64)
      .max(1);
    let previous = self.processed_samples;
    let span = chunk.boundary_sample.saturating_sub(previous);
    let Some(signal) = signal_bounds(chunk.samples) else {
      self.processed_samples = chunk.boundary_sample;
      return self.report(overall_progress(previous, span, 100, total));
    };
    // Auto detection reads the beginning of its input. Skip only leading
    // digital/inaudible silence, retaining half a second before the first signal.
    let trim = signal.start.saturating_sub(SAMPLE_RATE / 2);
    let audible_end = (signal.end + SAMPLE_RATE / 2).min(chunk.samples.len());
    let audible = &chunk.samples[trim..audible_end];
    if self.state.is_none() {
      let mut options = WhisperContextParameters::default();
      options.use_gpu(false);
      let context = WhisperContext::new_with_params(self.model, options)
        .map_err(|_| "Could not load the offline speech model.")?;
      self.state = Some(
        context
          .create_state()
          .map_err(|_| "Could not allocate memory for transcription.")?,
      );
    }
    check_cancelled(&self.cancel)?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(
      std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(2)
        .clamp(1, 4) as i32,
    );
    params.set_language(self.language.as_deref());
    params.set_translate(false);
    params.set_no_context(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);
    params.set_suppress_nst(true);
    // Short word-aligned segments make overlap reconciliation less dependent on
    // Whisper choosing exactly the same long sentence boundaries in two windows.
    params.set_token_timestamps(true);
    params.set_split_on_word(true);
    params.set_max_len(64);
    let observer = self.observer.clone();
    let mapped_progress = move |percent| {
      let observer = observer.lock().unwrap_or_else(|error| error.into_inner());
      observer(overall_progress(previous, span, percent, total));
    };
    let mut callbacks = Box::new(Callbacks {
      cancel: self.cancel.clone(),
      progress: Mutex::new(mapped_progress),
    });
    // Infer the callback's concrete closure type without erasing it.
    fn attach<'a, 'b, F: Fn(u32)>(params: &mut FullParams<'a, 'b>, callbacks: &mut Callbacks<F>) {
      let data = (callbacks as *mut Callbacks<F>).cast::<c_void>();
      unsafe {
        params.set_abort_callback(Some(abort::<F>));
        params.set_abort_callback_user_data(data);
        params.set_progress_callback(Some(progress::<F>));
        params.set_progress_callback_user_data(data);
      }
    }
    attach(&mut params, &mut callbacks);
    // Keep short input support without copying an ordinary five-minute window.
    let padded;
    let samples = if audible.len() < SAMPLE_RATE {
      padded = [audible, &vec![0.0; SAMPLE_RATE - audible.len()]].concat();
      padded.as_slice()
    } else {
      audible
    };
    let state = self.state.as_mut().unwrap();
    let result = state.full(params, samples);
    check_cancelled(&self.cancel)?;
    result.map_err(|_| "Local transcription failed. Try again or choose another audio file.")?;
    if self.language.is_none() {
      self.language = whisper_rs::get_lang_str(state.full_lang_id_from_state()).map(str::to_owned);
    }
    let offset_ms = samples_to_ms(chunk.start_sample + trim as u64);
    let end_ms = samples_to_ms(chunk.start_sample + audible_end as u64);
    let boundary_ms = samples_to_ms(chunk.boundary_sample);
    let mut incoming = Vec::new();
    for segment in state.as_iter() {
      let text = segment
        .to_str_lossy()
        .map_err(|_| "Could not read the transcript.")?
        .trim()
        .to_owned();
      if text.is_empty() {
        continue;
      }
      let start_ms = offset_ms
        .saturating_add(segment.start_timestamp().max(0) as u64 * 10)
        .min(end_ms);
      let end_ms = offset_ms
        .saturating_add(segment.end_timestamp().max(0) as u64 * 10)
        .min(end_ms)
        .max(start_ms);
      // The next window owns segments beginning after the cut. Keep a crossing
      // phrase intact, then reconcile its repeated prefix in the next window.
      if !chunk.last && start_ms >= boundary_ms {
        continue;
      }
      incoming.push(TranscriptSegment {
        start_ms,
        end_ms,
        text,
      });
    }
    append_segments(&mut self.segments, incoming, self.covered_until_ms);
    self.covered_until_ms = self
      .segments
      .last()
      .map(|segment| segment.end_ms)
      .unwrap_or(0);
    self.processed_samples = chunk.boundary_sample;
    self.report(overall_progress(previous, span, 100, total))
  }
}

fn samples_to_ms(samples: u64) -> u64 {
  samples * 1_000 / SAMPLE_RATE as u64
}

fn overall_progress(previous: u64, span: u64, local: u32, total: u64) -> u32 {
  ((previous + span.saturating_mul(local.min(100) as u64) / 100).saturating_mul(100) / total.max(1))
    .min(99) as u32
}

fn append_segments(
  output: &mut Vec<TranscriptSegment>,
  incoming: Vec<TranscriptSegment>,
  covered_until: u64,
) {
  for mut segment in incoming {
    if segment.end_ms <= covered_until {
      continue;
    }
    if segment.start_ms < covered_until {
      let previous = output
        .iter()
        .rev()
        .take(8)
        .rev()
        .map(|part| part.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
      segment.text = trim_repeated_prefix(&previous, &segment.text);
      segment.start_ms = covered_until.min(segment.end_ms);
    }
    if !segment.text.is_empty() {
      output.push(segment);
    }
  }
}

fn trim_repeated_prefix(previous: &str, incoming: &str) -> String {
  let left: Vec<char> = previous
    .chars()
    .filter(|ch| ch.is_alphanumeric())
    .flat_map(char::to_lowercase)
    .collect();
  let right: Vec<(usize, char)> = incoming
    .char_indices()
    .filter(|(_, ch)| ch.is_alphanumeric())
    .flat_map(|(offset, ch)| {
      ch.to_lowercase()
        .map(move |normalized| (offset + ch.len_utf8(), normalized))
    })
    .collect();
  let limit = left.len().min(right.len()).min(160);
  for count in (4..=limit).rev() {
    if left[left.len() - count..]
      .iter()
      .copied()
      .eq(right[..count].iter().map(|(_, ch)| *ch))
    {
      return incoming[right[count - 1].0..]
        .trim_start_matches(|ch: char| ch.is_whitespace() || ch.is_ascii_punctuation())
        .to_owned();
    }
  }
  incoming.to_owned()
}

fn signal_bounds(samples: &[f32]) -> Option<std::ops::Range<usize>> {
  let frame = SAMPLE_RATE / 10;
  let mut range: Option<std::ops::Range<usize>> = None;
  for (index, block) in samples.chunks(frame).enumerate() {
    let count = block.len() as f64;
    let mean = block.iter().map(|value| f64::from(*value)).sum::<f64>() / count;
    let variance = block
      .iter()
      .map(|value| (f64::from(*value) - mean).powi(2))
      .sum::<f64>()
      / count;
    if variance >= 1.0e-8 {
      let start = index * frame;
      let end = start + block.len();
      range.get_or_insert(start..end).end = end;
    }
  }
  range
}

#[cfg(test)]
fn first_signal(samples: &[f32]) -> Option<usize> {
  signal_bounds(samples).map(|range| range.start)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn overlap_keeps_continuations_once_with_absolute_times() {
    let mut output = vec![TranscriptSegment {
      start_ms: 299_000,
      end_ms: 301_000,
      text: "We should meet on Tuesday".into(),
    }];
    append_segments(
      &mut output,
      vec![
        TranscriptSegment {
          start_ms: 298_000,
          end_ms: 300_000,
          text: "We should meet".into(),
        },
        TranscriptSegment {
          start_ms: 300_000,
          end_ms: 303_000,
          text: "on Tuesday, and bring the report.".into(),
        },
        TranscriptSegment {
          start_ms: 303_000,
          end_ms: 305_000,
          text: "Then we can decide.".into(),
        },
      ],
      301_000,
    );
    assert_eq!(output.len(), 3);
    assert_eq!(output[1].text, "and bring the report.");
    assert_eq!(output[1].start_ms, 301_000);
    assert_eq!(output[2].end_ms, 305_000);
    assert_eq!(
      trim_repeated_prefix("我们明天讨论计划", "明天讨论计划，然后继续。"),
      "，然后继续。"
    );
    assert_eq!(
      trim_repeated_prefix("First statement", "Different continuation"),
      "Different continuation"
    );
  }

  #[test]
  fn chunk_progress_does_not_reset_or_finish_early() {
    assert_eq!(overall_progress(0, 300, 100, 1800), 16);
    assert_eq!(overall_progress(300, 300, 50, 1800), 25);
    assert_eq!(overall_progress(1500, 300, 100, 1800), 99);
  }

  #[test]
  fn short_silent_input_returns_exact_duration_without_loading_a_model() {
    let file = tempfile::Builder::new().suffix(".wav").tempfile().unwrap();
    let spec = hound::WavSpec {
      channels: 1,
      sample_rate: SAMPLE_RATE as u32,
      bits_per_sample: 16,
      sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(file.path(), spec).unwrap();
    for _ in 0..SAMPLE_RATE / 10 {
      writer.write_sample(0i16).unwrap();
    }
    writer.finalize().unwrap();
    let result = transcribe(
      Path::new("missing-model.bin"),
      file.path(),
      None,
      Arc::new(AtomicBool::new(false)),
      |_| {},
    )
    .unwrap();
    assert_eq!(result.duration_ms, 100);
    assert_eq!(result.language, "und");
    assert!(result.segments.is_empty());
    assert!(transcribe(
      Path::new("missing-model.bin"),
      file.path(),
      None,
      Arc::new(AtomicBool::new(true)),
      |_| {}
    )
    .unwrap_err()
    .contains("cancelled"));
  }

  #[test]
  fn silence_does_not_invent_a_language() {
    for sample in [0.0, 0.1] {
      assert_eq!(first_signal(&vec![sample; SAMPLE_RATE]), None);
    }
  }

  #[test]
  fn finds_short_speech_after_minutes_of_leading_silence() {
    let mut audio = vec![0.0; 5 * 60 * SAMPLE_RATE];
    let offset = 280 * SAMPLE_RATE;
    for (index, sample) in audio[offset..offset + SAMPLE_RATE / 10]
      .iter_mut()
      .enumerate()
    {
      *sample = if index % 2 == 0 { 0.01 } else { -0.01 };
    }
    assert_eq!(first_signal(&audio), Some(offset));
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
