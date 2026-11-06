use crate::decode::SAMPLE_RATE;

// Five minutes of working audio, plus five seconds of context on each side.
// At 16 kHz this holds about 20 MiB, independent of total recording duration.
pub(crate) const CHUNK_SAMPLES: usize = 5 * 60 * SAMPLE_RATE;
pub(crate) const OVERLAP_SAMPLES: usize = 5 * SAMPLE_RATE;
const SEARCH_SAMPLES: usize = 30 * SAMPLE_RATE;

pub(crate) struct AudioChunk<'a> {
  pub samples: &'a [f32],
  pub start_sample: u64,
  pub boundary_sample: u64,
  pub last: bool,
}

pub(crate) struct AudioChunks {
  pending: Vec<f32>,
  start_sample: u64,
  boundary_sample: u64,
}

impl Default for AudioChunks {
  fn default() -> Self {
    Self {
      pending: Vec::with_capacity(CHUNK_SAMPLES + OVERLAP_SAMPLES),
      start_sample: 0,
      boundary_sample: 0,
    }
  }
}

impl AudioChunks {
  pub fn push(
    &mut self,
    mut samples: &[f32],
    consume: &mut impl FnMut(AudioChunk<'_>) -> Result<(), String>,
  ) -> Result<(), String> {
    while !samples.is_empty() {
      let limit = CHUNK_SAMPLES + OVERLAP_SAMPLES;
      let count = samples.len().min(limit - self.pending.len());
      self.pending.extend_from_slice(&samples[..count]);
      samples = &samples[count..];
      if self.pending.len() == limit {
        let cut = quiet_boundary(&self.pending[..CHUNK_SAMPLES]);
        let boundary_sample = self.start_sample + cut as u64;
        consume(AudioChunk {
          samples: &self.pending[..cut + OVERLAP_SAMPLES],
          start_sample: self.start_sample,
          boundary_sample,
          last: false,
        })?;
        let retained_from = cut - OVERLAP_SAMPLES;
        self.pending.drain(..retained_from);
        self.start_sample += retained_from as u64;
        self.boundary_sample = boundary_sample;
      }
    }
    Ok(())
  }

  pub fn finish(
    self,
    consume: &mut impl FnMut(AudioChunk<'_>) -> Result<(), String>,
  ) -> Result<(), String> {
    if self.start_sample + self.pending.len() as u64 > self.boundary_sample {
      consume(AudioChunk {
        samples: &self.pending,
        start_sample: self.start_sample,
        boundary_sample: self.start_sample + self.pending.len() as u64,
        last: true,
      })?;
    }
    Ok(())
  }
}

fn quiet_boundary(samples: &[f32]) -> usize {
  // Prefer the midpoint of the last >=300 ms quiet stretch in the last 30 s.
  // Continuous speech still has overlapping context rather than a hard cut.
  let frame = SAMPLE_RATE / 50;
  let start = samples.len().saturating_sub(SEARCH_SAMPLES);
  let mut quiet_start = None;
  let mut best = samples.len();
  for (index, block) in samples[start..].chunks_exact(frame).enumerate() {
    let position = start + index * frame;
    let energy = block.iter().map(|sample| sample * sample).sum::<f32>() / frame as f32;
    if energy < 1.0e-5 {
      let from = *quiet_start.get_or_insert(position);
      if position + frame - from >= SAMPLE_RATE * 3 / 10 {
        best = (from + position + frame) / 2;
      }
    } else {
      quiet_start = None;
    }
  }
  best
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn overlapping_windows_cover_every_sample_and_keep_bounded_memory() {
    let mut chunks = AudioChunks::default();
    let mut windows = Vec::new();
    let block = vec![0.25; 16_000];
    let mut receive = |chunk: AudioChunk<'_>| {
      assert!(chunk.samples.len() <= CHUNK_SAMPLES + OVERLAP_SAMPLES);
      windows.push((
        chunk.start_sample,
        chunk.start_sample + chunk.samples.len() as u64,
        chunk.boundary_sample,
      ));
      Ok(())
    };
    for _ in 0..(31 * 60) {
      chunks.push(&block, &mut receive).unwrap();
    }
    chunks.finish(&mut receive).unwrap();
    assert!(windows.len() > 6);
    assert_eq!(windows[0].0, 0);
    assert_eq!(windows.last().unwrap().1, (31 * 60 * SAMPLE_RATE) as u64);
    for pair in windows.windows(2) {
      assert_eq!(pair[0].1 - pair[1].0, (2 * OVERLAP_SAMPLES) as u64);
      assert!(pair[1].2 > pair[0].2);
    }
  }

  #[test]
  fn chooses_a_pause_instead_of_cutting_a_word() {
    let mut samples = vec![0.25; CHUNK_SAMPLES];
    let pause = CHUNK_SAMPLES - 4 * SAMPLE_RATE;
    samples[pause..pause + SAMPLE_RATE].fill(0.0);
    assert_eq!(quiet_boundary(&samples), pause + SAMPLE_RATE / 2);
  }

  #[test]
  fn callback_failure_stops_processing_and_short_tail_survives() {
    let mut chunks = AudioChunks::default();
    let result = chunks.push(&vec![0.1; CHUNK_SAMPLES + OVERLAP_SAMPLES], &mut |_| {
      Err("cancelled".into())
    });
    assert_eq!(result.unwrap_err(), "cancelled");
    let mut chunks = AudioChunks::default();
    chunks
      .push(&[0.5; 17], &mut |_| panic!("too early"))
      .unwrap();
    chunks
      .finish(&mut |chunk| {
        assert!(chunk.last);
        assert_eq!(chunk.samples, &[0.5; 17]);
        Ok(())
      })
      .unwrap();
  }
}
