use apell_guest::HostError;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Conversation {
  pub conversation_id: String,
  pub title: String,
  pub updated_at: String,
}

#[derive(Serialize, Deserialize)]
pub struct Message {
  pub message_id: String,
  pub role: String,
  pub content: String,
  pub status: String,
  #[serde(default)]
  pub media: Vec<Media>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Media {
  #[serde(rename = "type")]
  pub kind: MediaKind,
  pub path: String,
  pub mime_type: String,
  pub size_bytes: usize,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub file_name: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub transcript: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
  Image,
  Audio,
}

impl Media {
  pub fn validate(&self) -> Result<(), HostError> {
    let valid_path = !self.path.is_empty()
      && self.path.len() <= 4096
      && !self.path.contains(['\\', ':'])
      && !self.path.chars().any(char::is_control)
      && self
        .path
        .split('/')
        .all(|part| !matches!(part, "" | "." | ".."));
    let valid_type = match self.kind {
      MediaKind::Image => matches!(
        self.mime_type.as_str(),
        "image/jpeg" | "image/png" | "image/gif" | "image/webp"
      ),
      MediaKind::Audio => matches!(self.mime_type.as_str(), "audio/wav" | "audio/mpeg"),
    };
    let valid_name = self.file_name.as_ref().is_none_or(|name| {
      !name.is_empty() && name.len() <= 512 && !name.chars().any(char::is_control)
    });
    if !valid_path
      || !valid_type
      || !valid_name
      || self.size_bytes == 0
      || self.size_bytes > 8 * 1024 * 1024
    {
      return Err(HostError::new(
        "Use a private JPEG, PNG, GIF, WebP, WAV, or MP3 file no larger than 8 MiB.",
      ));
    }
    Ok(())
  }

  pub fn title(&self) -> &str {
    self.file_name.as_deref().unwrap_or(match self.kind {
      MediaKind::Image => "Image conversation",
      MediaKind::Audio => "Audio conversation",
    })
  }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MediaOutput {
  Image {
    #[serde(default)]
    format: ImageFormat,
  },
  Audio {
    voice: String,
    #[serde(default)]
    format: AudioFormat,
  },
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
  #[default]
  Png,
  Jpeg,
  Webp,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioFormat {
  #[default]
  Wav,
  Mp3,
}

impl MediaOutput {
  pub fn validate(&self) -> Result<(), HostError> {
    if let Self::Audio { voice, .. } = self {
      if voice.is_empty()
        || voice.len() > 128
        || !voice
          .bytes()
          .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
      {
        return Err(HostError::new(
          "Enter a valid voice ID for the selected provider.",
        ));
      }
    }
    Ok(())
  }
}

pub fn title_from_message(content: &str) -> String {
  let single_line = content.split_whitespace().collect::<Vec<_>>().join(" ");
  let mut title: String = single_line.chars().take(52).collect();
  if single_line.chars().count() > 52 {
    title.push('…');
  }
  title
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn titles_are_single_line_and_do_not_split_unicode() {
    assert_eq!(title_from_message("hello\n world"), "hello world");
    assert_eq!(
      title_from_message(&"読".repeat(53)),
      format!("{}…", "読".repeat(52))
    );
  }
}
