use super::*;

pub(crate) fn png(width: u32, height: u32, color: u8) -> Vec<u8> {
  let mut bytes = Vec::new();
  {
    let mut encoder = png::Encoder::new(&mut bytes, width, height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer
      .write_image_data(&vec![color; (width * height) as usize])
      .unwrap();
  }
  bytes
}

#[test]
fn encodes_valid_still_png_and_accepts_dimension_limit() {
  for bytes in [png(1, 1, 0), png(1024, 1024, 255)] {
    assert_eq!(
      png_data_url(&bytes).unwrap(),
      format!("data:image/png;base64,{}", STANDARD.encode(&bytes))
    );
  }
}

#[test]
fn rejects_invalid_pixels_truncation_and_oversized_icons() {
  let valid = png(1, 1, 0);
  let mut corrupted = valid.clone();
  // A valid IHDR alone is insufficient: corrupt the IDAT content after it.
  let idat = corrupted
    .windows(4)
    .position(|part| part == b"IDAT")
    .unwrap();
  corrupted[idat + 4] ^= 0xff;
  let mut oversized = valid.clone();
  oversized.resize(MAX_ICON_BYTES + 1, 0);
  for invalid in [
    Vec::new(),
    b"not a PNG".to_vec(),
    valid[..33].to_vec(),
    valid[..valid.len() - 1].to_vec(),
    corrupted,
    oversized,
    png(1025, 1, 0),
    png(1, 1025, 0),
  ] {
    assert!(png_data_url(&invalid).is_err());
  }
}
