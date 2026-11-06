use crate::{check_cancelled, MAX_AUDIO_BYTES, MAX_AUDIO_SECONDS};
use rubato::{FftFixedInOut, Resampler};
use std::{fs::File, io::ErrorKind, path::Path, sync::atomic::AtomicBool};
use symphonia::core::{
  codecs::audio::AudioDecoderOptions,
  errors::Error,
  formats::{probe::Hint, well_known::FORMAT_ID_WAVE, FormatOptions, TrackType},
  io::MediaSourceStream,
  meta::MetadataOptions,
};

pub(crate) const SAMPLE_RATE: usize = 16_000;

/// Decode one packet at a time. Neither decoded PCM nor resampler output grows
/// with the recording duration. The consumer can perform inference before the
/// next packet is read, providing backpressure without another audio queue.
pub(crate) fn stream(
  path: &Path,
  cancel: &AtomicBool,
  mut consume: impl FnMut(&[f32], Option<u64>) -> Result<(), String>,
) -> Result<u64, String> {
  check_cancelled(cancel)?;
  let file = File::open(path).map_err(|_| "Could not open the audio recording.")?;
  let metadata = file
    .metadata()
    .map_err(|_| "Could not inspect the audio recording.")?;
  if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_AUDIO_BYTES {
    return Err("Choose a nonempty audio file no larger than 2 GiB.".into());
  }
  let source = MediaSourceStream::new(Box::new(file), Default::default());
  let mut hint = Hint::new();
  if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
    hint.with_extension(extension);
  }
  let mut format = symphonia::default::get_probe()
    .probe(
      &hint,
      source,
      FormatOptions::default(),
      MetadataOptions::default(),
    )
    .map_err(|_| "Unsupported audio. Use WAV, MP3, M4A (AAC/ALAC), FLAC, or Ogg Vorbis.")?;
  let track = format
    .default_track(TrackType::Audio)
    .ok_or("The file does not contain a supported audio track.")?;
  let codec = track
    .codec_params
    .as_ref()
    .and_then(|params| params.audio())
    .ok_or("The file does not contain a supported audio track.")?;
  let estimated_samples = track
    .time_base
    .zip(track.duration)
    .and_then(|(base, duration)| base.calc_duration(duration))
    .map(|time| (time.as_secs_f64() * SAMPLE_RATE as f64).round() as u64)
    .filter(|samples| *samples > 0);
  if estimated_samples.is_some_and(|samples| samples > MAX_AUDIO_SECONDS * SAMPLE_RATE as u64) {
    return Err("Recordings longer than 3 hours are not supported.".into());
  }
  let is_wave = format.format_info().format == FORMAT_ID_WAVE;
  let wave_frames = if is_wave {
    track.duration.map(|duration| duration.get())
  } else {
    None
  };
  let streaming_wave = is_wave && wave_frames.is_none();
  let track_id = track.id;
  let mut decoder = symphonia::default::get_codecs()
    .make_audio_decoder(codec, &AudioDecoderOptions::default())
    .map_err(|_| "This audio codec is not supported. Use WAV, MP3, AAC, ALAC, FLAC, or Vorbis.")?;
  let mut resampler: Option<MonoResampler> = None;
  let mut samples = Vec::<f32>::new();
  let mut emit = |block: &[f32]| {
    check_cancelled(cancel)?;
    consume(block, estimated_samples)
  };
  loop {
    check_cancelled(cancel)?;
    let packet = match format.next_packet() {
      Ok(Some(packet)) => packet,
      Ok(None) => break,
      Err(Error::IoError(error)) if streaming_wave && error.kind() == ErrorKind::UnexpectedEof => {
        break
      }
      Err(_) => return Err("The audio file is damaged or changes format midway.".into()),
    };
    if packet.track_id != track_id {
      continue;
    }
    let decoded = decoder
      .decode(&packet)
      .map_err(|_| "The audio contains damaged or unsupported frames.")?;
    let channels = decoded.spec().channels().count();
    let rate = decoded.spec().rate() as usize;
    if channels == 0 || channels > 8 || !(8_000..=192_000).contains(&rate) {
      return Err("Audio must have 1–8 channels and a sample rate between 8 and 192 kHz.".into());
    }
    if decoded.frames() > rate * 10 {
      return Err("The audio decoder returned an oversized frame.".into());
    }
    if resampler.is_none() {
      resampler = Some(MonoResampler::new(rate)?);
    }
    let resampler = resampler.as_mut().unwrap();
    if resampler.input_rate != rate {
      return Err("Audio that changes sample rate midway is not supported.".into());
    }
    decoded.copy_to_vec_interleaved(&mut samples);
    for frame in samples.chunks_exact(channels) {
      let mono = frame.iter().copied().sum::<f32>() / channels as f32;
      if !mono.is_finite() {
        return Err("The audio contains invalid samples.".into());
      }
      resampler.push(mono.clamp(-1.0, 1.0), &mut emit)?;
    }
  }
  check_cancelled(cancel)?;
  let resampler = resampler.ok_or("The audio file contains no samples.")?;
  if wave_frames.is_some_and(|frames| resampler.input_count < frames) {
    return Err("The WAV file ends before its declared audio length.".into());
  }
  let count = resampler.finish(&mut emit)?;
  if count < SAMPLE_RATE as u64 / 10 {
    return Err("Record at least a tenth of a second before transcribing.".into());
  }
  Ok(count)
}

struct MonoResampler {
  input_rate: usize,
  input_count: u64,
  emitted_count: u64,
  delay_remaining: usize,
  pending: Vec<f32>,
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
    let delay_remaining = filter.as_ref().map(Resampler::output_delay).unwrap_or(0);
    Ok(Self {
      input_rate,
      input_count: 0,
      emitted_count: 0,
      delay_remaining,
      pending: Vec::new(),
      filter,
    })
  }

  fn push(
    &mut self,
    sample: f32,
    consume: &mut impl FnMut(&[f32]) -> Result<(), String>,
  ) -> Result<(), String> {
    self.input_count += 1;
    if self.input_count > self.input_rate as u64 * MAX_AUDIO_SECONDS {
      return Err("Recordings longer than 3 hours are not supported.".into());
    }
    self.pending.push(sample);
    let block_size = self
      .filter
      .as_ref()
      .map(Resampler::input_frames_next)
      .unwrap_or(1024);
    if self.pending.len() == block_size {
      if let Some(filter) = self.filter.as_mut() {
        let block = filter
          .process(&[self.pending.as_slice()], None)
          .map_err(|_| "Could not resample audio.")?;
        self.emit(&block[0], consume)?;
      } else {
        let mut block = std::mem::take(&mut self.pending);
        self.emit(&block, consume)?;
        block.clear();
        self.pending = block;
      }
      self.pending.clear();
    }
    Ok(())
  }

  fn emit(
    &mut self,
    block: &[f32],
    consume: &mut impl FnMut(&[f32]) -> Result<(), String>,
  ) -> Result<(), String> {
    let skip = self.delay_remaining.min(block.len());
    self.delay_remaining -= skip;
    let wanted = self.input_count * SAMPLE_RATE as u64 / self.input_rate as u64;
    let count = (wanted - self.emitted_count).min((block.len() - skip) as u64) as usize;
    if count > 0 {
      consume(&block[skip..skip + count])?;
      self.emitted_count += count as u64;
    }
    Ok(())
  }

  fn finish(
    mut self,
    consume: &mut impl FnMut(&[f32]) -> Result<(), String>,
  ) -> Result<u64, String> {
    let wanted = self.input_count * SAMPLE_RATE as u64 / self.input_rate as u64;
    if let Some(filter) = self.filter.as_mut() {
      if !self.pending.is_empty() {
        let block = filter
          .process_partial(Some(&[self.pending.as_slice()]), None)
          .map_err(|_| "Could not finish audio resampling.")?;
        self.emit(&block[0], consume)?;
      }
      while self.emitted_count < wanted {
        let block = self
          .filter
          .as_mut()
          .unwrap()
          .process_partial::<&[f32]>(None, None)
          .map_err(|_| "Could not finish audio resampling.")?;
        self.emit(&block[0], consume)?;
      }
    } else {
      let block = std::mem::take(&mut self.pending);
      self.emit(&block, consume)?;
    }
    Ok(self.emitted_count)
  }
}

#[cfg(test)]
fn decode(path: &Path, cancel: &AtomicBool) -> Result<Vec<f32>, String> {
  let mut samples = Vec::new();
  stream(path, cancel, |block, _| {
    samples.extend_from_slice(block);
    Ok(())
  })?;
  Ok(samples)
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

  fn sparse_wave(frames: u64) -> tempfile::NamedTempFile {
    let file = wav(16_000, 1, 0.0, 0.0);
    let mut header = std::fs::read(file.path()).unwrap();
    let data_bytes = u32::try_from(frames * 2).unwrap();
    header[4..8].copy_from_slice(&(36 + data_bytes).to_le_bytes());
    header[40..44].copy_from_slice(&data_bytes.to_le_bytes());
    std::fs::write(file.path(), header).unwrap();
    file.as_file().set_len(44 + data_bytes as u64).unwrap();
    file
  }

  #[test]
  fn streams_past_old_thirty_minute_limit_without_accumulating_pcm() {
    let frames = 31 * 60 * SAMPLE_RATE as u64;
    let file = sparse_wave(frames);
    let mut received = 0;
    let mut largest_block = 0;
    let count = stream(file.path(), &AtomicBool::new(false), |block, estimate| {
      received += block.len() as u64;
      largest_block = largest_block.max(block.len());
      assert_eq!(estimate, Some(frames));
      Ok(())
    })
    .unwrap();
    assert_eq!(count, frames);
    assert_eq!(received, frames);
    assert!(largest_block <= 1024);
  }

  #[test]
  fn accepts_three_hour_header_and_rejects_one_sample_over_it() {
    let maximum = MAX_AUDIO_SECONDS * SAMPLE_RATE as u64;
    let file = sparse_wave(maximum);
    let cancelled = AtomicBool::new(false);
    let mut emitted = false;
    let result = stream(file.path(), &cancelled, |_, _| {
      emitted = true;
      cancelled.store(true, std::sync::atomic::Ordering::Release);
      Ok(())
    });
    assert!(emitted, "the three-hour boundary must be admitted");
    assert!(result.unwrap_err().contains("cancelled"));
    let file = sparse_wave(maximum + 1);
    assert!(stream(file.path(), &AtomicBool::new(false), |_, _| panic!(
      "oversized header must fail before PCM"
    ))
    .unwrap_err()
    .contains("3 hours"));
    let mut resampler = MonoResampler::new(SAMPLE_RATE).unwrap();
    resampler.input_count = maximum - 1;
    resampler.emitted_count = maximum - 1;
    resampler.push(0.0, &mut |_| Ok(())).unwrap();
    assert!(resampler
      .push(0.0, &mut |_| Ok(()))
      .unwrap_err()
      .contains("3 hours"));
  }

  #[test]
  fn bounds_source_files_and_short_input_before_returning_a_transcript() {
    let file = tempfile::NamedTempFile::new().unwrap();
    file.as_file().set_len(MAX_AUDIO_BYTES + 1).unwrap();
    assert!(stream(file.path(), &AtomicBool::new(false), |_, _| Ok(()))
      .unwrap_err()
      .contains("2 GiB"));
    let file = sparse_wave((SAMPLE_RATE / 10 - 1) as u64);
    assert!(stream(file.path(), &AtomicBool::new(false), |_, _| Ok(()))
      .unwrap_err()
      .contains("tenth"));
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

  #[test]
  fn rejects_truncated_wave_without_returning_partial_audio() {
    for bytes_missing in [1, 2, 16_000] {
      let file = wav(16_000, 1, 1.0, 440.0);
      let length = file.as_file().metadata().unwrap().len();
      file.as_file().set_len(length - bytes_missing).unwrap();
      assert!(decode(file.path(), &AtomicBool::new(false)).is_err());
    }
  }

  #[test]
  fn accepts_wave_explicitly_written_without_a_known_length() {
    let file = wav(16_000, 1, 1.0, 440.0);
    let mut bytes = std::fs::read(file.path()).unwrap();
    assert_eq!(&bytes[36..40], b"data");
    bytes[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    bytes[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    std::fs::write(file.path(), bytes).unwrap();
    assert_eq!(
      decode(file.path(), &AtomicBool::new(false)).unwrap().len(),
      SAMPLE_RATE
    );
  }

  #[test]
  fn rejects_mp4_handler_whose_size_exceeds_the_file() {
    fn atom(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
      [(8 + data.len() as u32).to_be_bytes().as_slice(), kind, data].concat()
    }

    // A short file whose track handler advertises a huge extended atom size.
    let handler = [
      1u32.to_be_bytes().as_slice(),
      b"hdlr",
      &(1u64 << 60).to_be_bytes(),
      &[0; 8],
      b"soun",
      &[0; 12],
    ]
    .concat();
    let bytes = [
      atom(b"ftyp", b"M4A \0\0\0\0M4A "),
      atom(b"moov", &atom(b"trak", &atom(b"mdia", &handler))),
    ]
    .concat();
    let file = tempfile::Builder::new().suffix(".m4a").tempfile().unwrap();
    std::fs::write(file.path(), bytes).unwrap();
    assert!(decode(file.path(), &AtomicBool::new(false)).is_err());
  }
}
