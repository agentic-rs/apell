//! Materialize applet file references for the provider transport.
use super::Failure;
use crate::filesystem::{BoundedReadError, FileStore};
use serde_json::Value;
use std::collections::BTreeMap;
use tokn_sdk::{AudioContent, Content, DocumentContent, ImageContent, MediaResource};

const MAX_ATTACHMENTS: usize = 8;
const MAX_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;

pub(super) type Attachments = BTreeMap<(usize, usize), Content>;

pub(super) struct FileAccess<'a> {
  pub files: &'a FileStore,
  pub applet_id: &'a str,
}

#[derive(Clone, Copy)]
enum FileKind {
  Image,
  Document,
  Audio,
}

enum Part<'a> {
  Text(&'a str),
  File(FileKind, &'a str),
}

pub(super) fn parts(message: &Value) -> Option<&[Value]> {
  message.get("content")?.as_array().map(Vec::as_slice)
}

pub(super) fn validate_parts(parts: &[Value]) -> Result<bool, Failure> {
  if parts.is_empty() {
    return Err(invalid_part());
  }
  let mut meaningful = false;
  for part in parts {
    meaningful |= match parse_part(part)? {
      Part::Text(text) => !text.is_empty(),
      Part::File(..) => true,
    };
  }
  Ok(meaningful)
}

fn parse_part(part: &Value) -> Result<Part<'_>, Failure> {
  let object = part.as_object().filter(|object| object.len() == 2);
  let object = object.ok_or_else(invalid_part)?;
  let kind = match object.get("type").and_then(Value::as_str) {
    Some("text") => {
      return object
        .get("text")
        .and_then(Value::as_str)
        .map(Part::Text)
        .ok_or_else(invalid_part)
    }
    Some("image") => FileKind::Image,
    Some("document") => FileKind::Document,
    Some("audio") => FileKind::Audio,
    _ => return Err(invalid_part()),
  };
  let path = object
    .get("path")
    .and_then(Value::as_str)
    .filter(|path| !path.is_empty())
    .ok_or_else(invalid_part)?;
  Ok(Part::File(kind, path))
}

pub(super) fn resolve(
  messages: &[Value],
  access: Option<FileAccess<'_>>,
) -> Result<Attachments, Failure> {
  let mut references = Vec::new();
  for (message_index, message) in messages.iter().enumerate() {
    if let Some(parts) = parts(message) {
      for (part_index, part) in parts.iter().enumerate() {
        if let Part::File(kind, path) = parse_part(part)? {
          if references.len() == MAX_ATTACHMENTS {
            return Err(Failure::new(
              "attachment_limit",
              "An LLM request can include up to 8 file attachments.",
            ));
          }
          references.push(((message_index, part_index), kind, path));
        }
      }
    }
  }
  if references.is_empty() {
    return Ok(Attachments::new());
  }
  let access = access.ok_or_else(|| {
    Failure::new(
      "attachment_permission",
      "The applet must declare file access to send attachments.",
    )
  })?;

  let mut attachments = Attachments::new();
  let mut remaining = MAX_ATTACHMENT_BYTES;
  for (position, kind, path) in references {
    let bytes = access
      .files
      .read_bounded(access.applet_id, path, remaining)
      .map_err(read_error)?;
    remaining -= bytes.len();
    attachments.insert(position, kind.content(bytes)?);
  }
  Ok(attachments)
}

impl FileKind {
  fn content(self, bytes: Vec<u8>) -> Result<Content, Failure> {
    let resource = match self {
      Self::Image => MediaResource::inline(
        image_type(&bytes).ok_or_else(|| invalid_format("Use a JPEG, PNG, GIF, or WebP image."))?,
        bytes,
      ),
      Self::Document => {
        if !bytes.starts_with(b"%PDF-") {
          return Err(invalid_format("Use a PDF document."));
        }
        MediaResource::inline("application/pdf", bytes)
      }
      Self::Audio => MediaResource::inline(
        audio_type(&bytes).ok_or_else(|| invalid_format("Use a WAV or MP3 audio file."))?,
        bytes,
      ),
    };
    Ok(match self {
      Self::Image => ImageContent::new(resource).into(),
      Self::Document => DocumentContent::new(resource)
        .with_filename("attachment.pdf")
        .into(),
      Self::Audio => AudioContent::new(resource).into(),
    })
  }
}

pub(super) fn image_type(bytes: &[u8]) -> Option<&'static str> {
  if bytes.starts_with(b"\xff\xd8\xff") {
    Some("image/jpeg")
  } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
    Some("image/png")
  } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
    Some("image/gif")
  } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
    Some("image/webp")
  } else {
    None
  }
}

pub(super) fn audio_type(bytes: &[u8]) -> Option<&'static str> {
  if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
    return Some("audio/wav");
  }
  // MP3 may start with an ID3v2 tag or directly with an MPEG Layer III frame.
  let mp3 = match bytes {
    [b'I', b'D', b'3', 2..=4, revision, _, size @ ..] if size.len() >= 4 => {
      *revision != 0xff && size[..4].iter().all(|byte| byte & 0x80 == 0)
    }
    [0xff, format, rate, _, ..] => {
      format & 0xe0 == 0xe0
        && format & 0x18 != 0x08 // reserved MPEG version
        && format & 0x06 == 0x02 // Layer III
        && rate & 0xf0 != 0xf0 // reserved bitrate index
        && rate & 0x0c != 0x0c // reserved sample-rate index
    }
    _ => false,
  };
  mp3.then_some("audio/mpeg")
}

fn invalid_part() -> Failure {
  Failure::new(
    "invalid_request",
    "Message content parts require text or an image, document, or audio file path.",
  )
}

fn invalid_format(message: &str) -> Failure {
  Failure::new("invalid_attachment", message)
}

fn read_error(error: BoundedReadError) -> Failure {
  match error {
    BoundedReadError::InvalidPath => {
      invalid_format("An attachment must use a relative applet file path.")
    }
    BoundedReadError::Unavailable => Failure::new(
      "attachment_unavailable",
      "An attachment could not be read from the applet's files.",
    ),
    BoundedReadError::TooLarge => Failure::new(
      "attachment_limit",
      "LLM attachments can contain up to 8 MiB of file data per request.",
    ),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  fn request(kind: &str, path: &str) -> Vec<Value> {
    vec![json!({"role":"user","content":[{"type":kind,"path":path}]})]
  }

  fn write(files: &FileStore, applet_id: &str, path: &str, bytes: &[u8]) {
    files
      .dispatch(applet_id, "fs.write", json!({"path":path,"bytes":bytes}))
      .unwrap();
  }

  #[test]
  fn rejects_unreadable_foreign_and_mismatched_files() {
    let root = tempfile::tempdir().unwrap();
    let files = FileStore::new(root.path().join("files"));
    write(&files, "other", "doc.pdf", b"%PDF-1.4\n");
    write(&files, "first", "fake.pdf", b"not a PDF");
    let access = || {
      Some(FileAccess {
        files: &files,
        applet_id: "first",
      })
    };
    for (path, code) in [
      ("doc.pdf", "attachment_unavailable"),
      ("../other/doc.pdf", "invalid_attachment"),
      ("https://example.invalid/doc.pdf", "invalid_attachment"),
      ("fake.pdf", "invalid_attachment"),
    ] {
      let failure = resolve(&request("document", path), access()).unwrap_err();
      assert_eq!(failure.code, code);
      assert!(!failure.message.contains(path));
    }
    assert_eq!(
      resolve(&request("document", "doc.pdf"), None)
        .unwrap_err()
        .code,
      "attachment_permission"
    );
  }

  #[cfg(unix)]
  #[test]
  fn rejects_symlinks_even_when_the_target_is_a_valid_attachment() {
    let root = tempfile::tempdir().unwrap();
    let files = FileStore::new(root.path().join("files"));
    write(&files, "first", "doc.pdf", b"%PDF-1.4\n");
    let target = files.native_path("first", "doc.pdf", false).unwrap();
    let link = files.native_path("first", "link.pdf", true).unwrap();
    std::os::unix::fs::symlink(target, link).unwrap();
    assert_eq!(
      resolve(
        &request("document", "link.pdf"),
        Some(FileAccess {
          files: &files,
          applet_id: "first"
        }),
      )
      .unwrap_err()
      .code,
      "attachment_unavailable"
    );
  }

  #[test]
  fn attachment_limits_apply_across_messages_before_provider_encoding() {
    let root = tempfile::tempdir().unwrap();
    let files = FileStore::new(root.path().join("files"));
    let access = || {
      Some(FileAccess {
        files: &files,
        applet_id: "first",
      })
    };
    let mut messages = vec![request("document", "missing.pdf").remove(0); MAX_ATTACHMENTS + 1];
    assert_eq!(
      resolve(&messages, access()).unwrap_err().code,
      "attachment_limit"
    );

    write(&files, "first", "doc.pdf", b"%PDF-1.4\n");
    let path = files.native_path("first", "doc.pdf", false).unwrap();
    std::fs::OpenOptions::new()
      .write(true)
      .open(path)
      .unwrap()
      .set_len((MAX_ATTACHMENT_BYTES / 2 + 1) as u64)
      .unwrap();
    messages = vec![request("document", "doc.pdf").remove(0); 2];
    assert_eq!(
      resolve(&messages, access()).unwrap_err().code,
      "attachment_limit"
    );
  }
}
