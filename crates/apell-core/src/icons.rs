use base64::{engine::general_purpose::STANDARD, Engine};
use std::io::Cursor;

pub(crate) const MAX_ICON_BYTES: usize = 256 * 1024;
const MAX_DECODED_BYTES: usize = 8 * 1024 * 1024;

/// Only bounded, fully decoded still PNGs cross the native-to-shell boundary.
pub(crate) fn png_data_url(bytes: &[u8]) -> Result<String, String> {
  if bytes.is_empty() || bytes.len() > MAX_ICON_BYTES {
    return Err("Icon must be a PNG of at most 256 KiB.".into());
  }
  let mut decoder = png::Decoder::new(Cursor::new(bytes));
  decoder.set_limits(png::Limits {
    bytes: MAX_DECODED_BYTES,
  });
  let mut reader = decoder
    .read_info()
    .map_err(|_| "Icon is not a valid PNG.")?;
  let info = reader.info();
  if info.width == 0
    || info.height == 0
    || info.width > 1024
    || info.height > 1024
    || info.animation_control.is_some()
  {
    return Err("Icon must be a still PNG no larger than 1024 × 1024.".into());
  }
  let size = reader.output_buffer_size();
  if size > MAX_DECODED_BYTES {
    return Err("Icon decoded content is too large.".into());
  }
  reader
    .next_frame(&mut vec![0; size])
    .map_err(|_| "Icon is not a valid PNG.")?;
  reader.finish().map_err(|_| "Icon is not a valid PNG.")?;
  Ok(format!("data:image/png;base64,{}", STANDARD.encode(bytes)))
}

#[cfg(test)]
pub(crate) mod tests;
