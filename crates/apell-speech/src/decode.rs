use crate::check_cancelled;
use rubato::{FftFixedInOut, Resampler};
use std::{fs::File, io::ErrorKind, path::Path, sync::atomic::AtomicBool};
use symphonia::core::{
  audio::SampleBuffer,
  codecs::{DecoderOptions, CODEC_TYPE_NULL},
  errors::Error,
  formats::FormatOptions,
  io::MediaSourceStream,
  meta::MetadataOptions,
  probe::Hint,
};

pub(crate) const SAMPLE_RATE: usize = 16_000;
const MAX_SECONDS: usize = 30 * 60;
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

pub(crate) fn decode(path: &Path, cancel: &AtomicBool) -> Result<Vec<f32>, String> {
  check_cancelled(cancel)?;
  let file = File::open(path).map_err(|_| "Could not open the audio recording.")?;
  let metadata = file
    .metadata()
    .map_err(|_| "Could not inspect the audio recording.")?;
  if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_FILE_BYTES {
    return Err("Choose a nonempty audio file smaller than 256 MiB.".into());
  }
  let source = MediaSourceStream::new(Box::new(file), Default::default());
  let mut hint = Hint::new();
  if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
    hint.with_extension(extension);
  }
  let mut format = symphonia::default::get_probe()
    .format(
      &hint,
      source,
      &FormatOptions {
        enable_gapless: true,
        ..Default::default()
      },
      &MetadataOptions::default(),
    )
    .map_err(|_| "Unsupported audio. Use WAV, MP3, M4A (AAC/ALAC), FLAC, or Ogg Vorbis.")?
    .format;
  let track = format
    .default_track()
    .filter(|track| track.codec_params.codec != CODEC_TYPE_NULL)
    .or_else(|| {
      format
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
    })
    .ok_or("The file does not contain a supported audio track.")?;
  let track_id = track.id;
  let mut decoder = symphonia::default::get_codecs()
    .make(&track.codec_params, &DecoderOptions::default())
    .map_err(|_| "This audio codec is not supported. Use WAV, MP3, AAC, ALAC, FLAC, or Vorbis.")?;
  let mut resampler: Option<MonoResampler> = None;
  loop {
    check_cancelled(cancel)?;
    let packet = match format.next_packet() {
      Ok(packet) => packet,
      Err(Error::IoError(error)) if error.kind() == ErrorKind::UnexpectedEof => break,
      Err(_) => return Err("The audio file is damaged or changes format midway.".into()),
    };
    if packet.track_id() != track_id {
      continue;
    }
    // Do not silently omit damaged frames and return a misleading partial transcript.
    let decoded = decoder
      .decode(&packet)
      .map_err(|_| "The audio contains damaged or unsupported frames.")?;
    let spec = *decoded.spec();
    let channels = spec.channels.count();
    if channels == 0 || channels > 8 || !(8_000..=192_000).contains(&spec.rate) {
      return Err("Audio must have 1–8 channels and a sample rate between 8 and 192 kHz.".into());
    }
    if decoded.frames() > spec.rate as usize * 10 {
      return Err("The audio decoder returned an oversized frame.".into());
    }
    if resampler.is_none() {
      resampler = Some(MonoResampler::new(spec.rate as usize)?);
    }
    let resampler = resampler.as_mut().unwrap();
    if resampler.input_rate != spec.rate as usize {
      return Err("Audio that changes sample rate midway is not supported.".into());
    }
    let mut samples = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
    samples.copy_interleaved_ref(decoded);
    for frame in samples.samples().chunks_exact(channels) {
      let mono = frame.iter().copied().sum::<f32>() / channels as f32;
      if !mono.is_finite() {
        return Err("The audio contains invalid samples.".into());
      }
      resampler.push(mono.clamp(-1.0, 1.0))?;
    }
  }
  check_cancelled(cancel)?;
  let samples = resampler
    .ok_or("The audio file contains no samples.")?
    .finish()?;
  if samples.len() < SAMPLE_RATE / 10 {
    return Err("Record at least a tenth of a second before transcribing.".into());
  }
  Ok(samples)
}

struct MonoResampler {
  input_rate: usize,
  input_count: usize,
  pending: Vec<f32>,
  output: Vec<f32>,
  filter: Option<FftFixedInOut<f32>>,
}

impl MonoResampler {
  fn new(input_rate: usize) -> Result<Self, String> {
    let filter = if input_rate == SAMPLE_RATE {
      None
    } else {
      Some(
        FftFixedInOut::new(input_rate, SAMPLE_RATE, 1024, 1)
          .map_err(|_| "Could not initialize audio resampling.")?,
      )
    };
    Ok(Self {
      input_rate,
      input_count: 0,
      pending: Vec::new(),
      output: Vec::new(),
      filter,
    })
  }

  fn push(&mut self, sample: f32) -> Result<(), String> {
    self.input_count += 1;
    if self.input_count > self.input_rate * MAX_SECONDS {
      return Err("Split recordings longer than 30 minutes before transcribing.".into());
    }
    let Some(filter) = self.filter.as_mut() else {
      self.output.push(sample);
      return Ok(());
    };
    self.pending.push(sample);
    if self.pending.len() == filter.input_frames_next() {
      let block = filter
        .process(&[self.pending.as_slice()], None)
        .map_err(|_| "Could not resample audio.")?;
      self.output.extend_from_slice(&block[0]);
      self.pending.clear();
    }
    Ok(())
  }

  fn finish(mut self) -> Result<Vec<f32>, String> {
    if let Some(filter) = self.filter.as_mut() {
      let wanted = (self.input_count as u64 * SAMPLE_RATE as u64 / self.input_rate as u64) as usize;
      let delay = filter.output_delay();
      if !self.pending.is_empty() {
        let block = filter
          .process_partial(Some(&[self.pending.as_slice()]), None)
          .map_err(|_| "Could not finish audio resampling.")?;
        self.output.extend_from_slice(&block[0]);
      }
      while self.output.len() < wanted + delay {
        let block = filter
          .process_partial::<&[f32]>(None, None)
          .map_err(|_| "Could not finish audio resampling.")?;
        self.output.extend_from_slice(&block[0]);
      }
      // Remove the filter's delay and padding, preserving the original duration.
      self.output.drain(..delay);
      self.output.truncate(wanted);
    }
    Ok(self.output)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn wav(rate: u32, channels: u16, seconds: f32, frequency: f32) -> tempfile::NamedTempFile {
    let file = tempfile::Builder::new().suffix(".wav").tempfile().unwrap();
    let spec = hound::WavSpec {
      channels,
      sample_rate: rate,
      bits_per_sample: 16,
      sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(file.path(), spec).unwrap();
    for frame in 0..(rate as f32 * seconds) as usize {
      let sample =
        ((frame as f32 * frequency * std::f32::consts::TAU / rate as f32).sin() * 12_000.0) as i16;
      for _ in 0..channels {
        writer.write_sample(sample).unwrap();
      }
    }
    writer.finalize().unwrap();
    file
  }

  #[test]
  fn stereo_48khz_preserves_duration_and_frequency() {
    let file = wav(48_000, 2, 1.0, 440.0);
    let samples = decode(file.path(), &AtomicBool::new(false)).unwrap();
    assert_eq!(samples.len(), SAMPLE_RATE);
    let crossings = samples
      .windows(2)
      .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
      .count();
    assert!((438..=442).contains(&crossings), "crossings: {crossings}");
    assert!(samples[100..15_900]
      .iter()
      .all(|sample| sample.abs() < 0.38));
  }

  #[test]
  fn downsampling_suppresses_aliases() {
    let file = wav(48_000, 1, 1.0, 12_000.0);
    let samples = decode(file.path(), &AtomicBool::new(false)).unwrap();
    let rms = (samples[500..15_500].iter().map(|s| s * s).sum::<f32>() / 15_000.0).sqrt();
    assert!(rms < 0.005, "alias RMS: {rms}");
  }

  #[test]
  fn fractional_resampling_keeps_short_tail() {
    let file = wav(44_100, 1, 0.333, 440.0);
    let samples = decode(file.path(), &AtomicBool::new(false)).unwrap();
    assert!((samples.len() as i64 - 5_328).abs() <= 1);
    assert!(samples[samples.len() - 100..].iter().any(|v| v.abs() > 0.1));
  }

  #[test]
  fn invalid_and_pre_cancelled_audio_fail() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), b"not audio").unwrap();
    assert!(decode(file.path(), &AtomicBool::new(false)).is_err());
    assert!(decode(file.path(), &AtomicBool::new(true))
      .unwrap_err()
      .contains("cancelled"));
  }
}
