use crate::check_cancelled;
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
  let is_wave = format.format_info().format == FORMAT_ID_WAVE;
  // The WAV demuxer expresses its declared data length in sample frames.
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
  loop {
    check_cancelled(cancel)?;
    let packet = match format.next_packet() {
      Ok(Some(packet)) => packet,
      Ok(None) => break,
      // A WAV written to a pipe can explicitly omit its length. The WAV
      // reader signals EOF with an I/O error in that case.
      Err(Error::IoError(error)) if streaming_wave && error.kind() == ErrorKind::UnexpectedEof => {
        break;
      }
      Err(_) => return Err("The audio file is damaged or changes format midway.".into()),
    };
    if packet.track_id != track_id {
      continue;
    }
    // Do not silently omit damaged frames and return a misleading partial transcript.
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
      resampler.push(mono.clamp(-1.0, 1.0))?;
    }
  }
  check_cancelled(cancel)?;
  let resampler = resampler.ok_or("The audio file contains no samples.")?;
  if wave_frames.is_some_and(|frames| (resampler.input_count as u64) < frames) {
    return Err("The WAV file ends before its declared audio length.".into());
  }
  let samples = resampler.finish()?;
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
