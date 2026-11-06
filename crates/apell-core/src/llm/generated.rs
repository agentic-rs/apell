//! Decode completed provider media before constructing the applet result.
use super::{media, validation, Failure, LlmAudioFormat, LlmImageFormat, MAX_OUTPUT_BYTES};
use serde_json::Value;
use tokn_convert::media::{audio_from_chat, image_from_responses};
use tokn_core::types::Bytes;
use tokn_sdk::Content;

const MAX_FILES: usize = 4;
const MAX_BYTES: usize = 8 * 1024 * 1024;

pub(super) struct Asset {
  pub bytes: Bytes,
  pub mime_type: &'static str,
  pub extension: &'static str,
  pub transcript: Option<String>,
}

pub(super) fn decode_image(
  item: &Value,
  format: LlmImageFormat,
  assets: &mut Vec<Asset>,
) -> Result<(), Failure> {
  if item["type"] != "image_generation_call"
    || item["status"] != "completed"
    || !item["id"].as_str().is_some_and(validation::valid_call_id)
  {
    return Err(invalid_media());
  }
  let remaining = remaining_bytes(&item["result"], assets)?;
  let Content::Image(image) = image_from_responses(item).map_err(|_| invalid_media())? else {
    return Err(invalid_media());
  };
  let (mime_type, extension) = match format {
    LlmImageFormat::Png => ("image/png", "png"),
    LlmImageFormat::Jpeg => ("image/jpeg", "jpg"),
    LlmImageFormat::Webp => ("image/webp", "webp"),
  };
  let bytes = image.resource.bytes().ok_or_else(invalid_media)?;
  validate_encoding(
    bytes,
    remaining,
    image.resource.media_type.as_deref(),
    media::image_type(bytes),
    mime_type,
  )?;
  assets.push(Asset {
    bytes: bytes.clone(),
    mime_type,
    extension,
    transcript: None,
  });
  Ok(())
}

pub(super) fn decode_audio(
  audio: &Value,
  format: LlmAudioFormat,
  assets: &mut Vec<Asset>,
) -> Result<(), Failure> {
  if let Some(transcript) = audio.get("transcript").filter(|value| !value.is_null()) {
    if transcript.as_str().ok_or_else(invalid_media)?.len() > MAX_OUTPUT_BYTES
      || serde_json::to_vec(transcript)
        .map_err(|_| invalid_media())?
        .len()
        > MAX_OUTPUT_BYTES
    {
      return Err(Failure::new(
        "response_limit",
        "The generated audio transcript exceeded the text output size limit.",
      ));
    }
  }
  let remaining = remaining_bytes(&audio["data"], assets)?;
  let Content::Audio(audio) = audio_from_chat(audio).map_err(|_| invalid_media())? else {
    return Err(invalid_media());
  };
  let (mime_type, extension) = match format {
    LlmAudioFormat::Wav => ("audio/wav", "wav"),
    LlmAudioFormat::Mp3 => ("audio/mpeg", "mp3"),
  };
  let bytes = audio.resource.bytes().ok_or_else(invalid_media)?;
  validate_encoding(
    bytes,
    remaining,
    audio.resource.media_type.as_deref(),
    media::audio_type(bytes),
    mime_type,
  )?;
  assets.push(Asset {
    bytes: bytes.clone(),
    mime_type,
    extension,
    transcript: audio.transcript.map(|transcript| transcript.text),
  });
  Ok(())
}

fn remaining_bytes(encoded: &Value, assets: &[Asset]) -> Result<usize, Failure> {
  if assets.len() >= MAX_FILES {
    return Err(media_limit());
  }
  let remaining = MAX_BYTES.saturating_sub(assets.iter().map(|asset| asset.bytes.len()).sum());
  let encoded = encoded.as_str().ok_or_else(invalid_media)?;
  // Reject oversized values before decoding and check the exact decoded
  // length afterwards. The final base64 quartet can hold up to three bytes.
  if encoded.len() > remaining.div_ceil(3) * 4 {
    return Err(media_limit());
  }
  Ok(remaining)
}

fn validate_encoding(
  bytes: &[u8],
  remaining: usize,
  reported: Option<&str>,
  detected: Option<&str>,
  expected: &str,
) -> Result<(), Failure> {
  if bytes.len() > remaining {
    return Err(media_limit());
  }
  if detected != Some(expected) || reported.is_some_and(|reported| reported != expected) {
    return Err(invalid_media());
  }
  Ok(())
}

fn invalid_media() -> Failure {
  Failure::new(
    "invalid_response",
    "The provider returned malformed generated media or a different format than requested.",
  )
}

fn media_limit() -> Failure {
  Failure::new(
    "response_limit",
    "An LLM response can contain up to 4 generated files and 8 MiB of generated media.",
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use base64::{engine::general_purpose::STANDARD, Engine};
  use serde_json::json;

  fn image(bytes: &[u8]) -> Value {
    json!({
      "type": "image_generation_call",
      "id": "ig_fixture",
      "status": "completed",
      "result": STANDARD.encode(bytes),
    })
  }

  #[test]
  fn decodes_expected_formats_when_provider_omits_format_metadata() {
    let png = b"\x89PNG\r\n\x1a\n";
    let wav = b"RIFF\x00\x00\x00\x00WAVE";
    let mut assets = Vec::new();
    decode_image(&image(png), LlmImageFormat::Png, &mut assets).unwrap();
    decode_audio(
      &json!({"data": STANDARD.encode(wav), "transcript": "Hello"}),
      LlmAudioFormat::Wav,
      &mut assets,
    )
    .unwrap();
    assert_eq!(assets[0].bytes.as_ref(), png);
    assert_eq!(assets[0].mime_type, "image/png");
    assert_eq!(assets[0].extension, "png");
    assert_eq!(assets[1].bytes.as_ref(), wav);
    assert_eq!(assets[1].mime_type, "audio/wav");
    assert_eq!(assets[1].transcript.as_deref(), Some("Hello"));
  }

  #[test]
  fn decodes_jpeg_webp_and_mp3_with_explicit_formats() {
    let mut assets = Vec::new();
    let mut jpeg = image(b"\xff\xd8\xff");
    jpeg["output_format"] = json!("jpeg");
    decode_image(&jpeg, LlmImageFormat::Jpeg, &mut assets).unwrap();
    let mut webp = image(b"RIFF\x00\x00\x00\x00WEBP");
    webp["output_format"] = json!("webp");
    decode_image(&webp, LlmImageFormat::Webp, &mut assets).unwrap();
    decode_audio(
      &json!({"data": STANDARD.encode(b"\xff\xfb\x90\x64"), "format": "mp3"}),
      LlmAudioFormat::Mp3,
      &mut assets,
    )
    .unwrap();
    assert_eq!(assets[0].mime_type, "image/jpeg");
    assert_eq!(assets[1].mime_type, "image/webp");
    assert_eq!(assets[2].mime_type, "audio/mpeg");
    assert!(assets[2].transcript.is_none());
  }

  #[test]
  fn rejects_wrong_formats_and_incomplete_images() {
    let mut assets = Vec::new();
    let mut item = image(b"\xff\xd8\xff");
    assert!(decode_image(&item, LlmImageFormat::Png, &mut assets).is_err());
    item["output_format"] = json!("png");
    assert!(decode_image(&item, LlmImageFormat::Jpeg, &mut assets).is_err());
    item["output_format"] = json!("jpeg");
    item["status"] = json!("in_progress");
    assert!(decode_image(&item, LlmImageFormat::Jpeg, &mut assets).is_err());
    assert!(decode_audio(
      &json!({"data": "not base64"}),
      LlmAudioFormat::Wav,
      &mut assets
    )
    .is_err());
    assert!(assets.is_empty());
  }

  #[test]
  fn bounds_file_count_and_exact_decoded_bytes() {
    let mut assets = Vec::new();
    let png = image(b"\x89PNG\r\n\x1a\n");
    for _ in 0..MAX_FILES {
      decode_image(&png, LlmImageFormat::Png, &mut assets).unwrap();
    }
    assert_eq!(
      decode_image(&png, LlmImageFormat::Png, &mut assets)
        .unwrap_err()
        .code,
      "response_limit"
    );
    assets.truncate(1);
    // Eight bytes pass the base64 precheck with seven bytes left because both
    // lengths fit into the same number of base64 quartets.
    assets[0].bytes = Bytes::from(vec![0; MAX_BYTES - 7]);
    assert_eq!(
      decode_image(&png, LlmImageFormat::Png, &mut assets)
        .unwrap_err()
        .code,
      "response_limit"
    );
    assert_eq!(assets.len(), 1);
  }

  #[test]
  fn bounds_the_serialized_transcript_before_publication() {
    let audio = json!({
      "data": STANDARD.encode(b"RIFF\x00\x00\x00\x00WAVE"),
      "transcript": "\0".repeat(MAX_OUTPUT_BYTES / 6 + 1),
    });
    let mut assets = Vec::new();
    assert_eq!(
      decode_audio(&audio, LlmAudioFormat::Wav, &mut assets)
        .unwrap_err()
        .code,
      "response_limit"
    );
    assert!(assets.is_empty());
  }
}
