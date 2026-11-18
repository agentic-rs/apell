//! OpenAI-compatible file transcription over a host-managed provider connection.
use super::{tokn_config::validate_account, validate_config, AccountConfig, LlmConfig};
use reqwest::{
  multipart::{Form, Part},
  redirect::Policy,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{path::Path, time::Duration};
use tokio::io::AsyncReadExt;

const MAX_FILE_BYTES: u64 = 25_000_000;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Deserialize)]
struct ApiTranscript {
  text: String,
  language: Option<String>,
  #[serde(default)]
  languages: Vec<DetectedLanguage>,
}

#[derive(Deserialize)]
struct DetectedLanguage {
  code: String,
}

pub fn supports(provider_id: &str) -> bool {
  matches!(provider_id, "openai" | "custom")
}

pub async fn transcribe(
  path: &Path,
  language: Option<&str>,
  config: &LlmConfig,
  account: Option<&AccountConfig>,
) -> Result<Value, String> {
  if !supports(&config.provider_id) {
    return Err("Choose an OpenAI or OpenAI-compatible transcription connection.".into());
  }
  validate_config(config)?;
  validate_account(config, account).map_err(|error| error.message)?;
  let file = tokio::fs::File::open(path)
    .await
    .map_err(|_| "Could not open the audio file.")?;
  let size = file
    .metadata()
    .await
    .map_err(|_| "Could not inspect the audio file.")?
    .len();
  let limit_error = "The transcription API accepts files up to 25 MB. Choose a shorter recording or an on-device recognizer.";
  if size > MAX_FILE_BYTES {
    return Err(limit_error.into());
  }
  let mut bytes = Vec::with_capacity(size as usize);
  file
    .take(MAX_FILE_BYTES + 1)
    .read_to_end(&mut bytes)
    .await
    .map_err(|_| "Could not read the audio file.")?;
  if bytes.len() as u64 > MAX_FILE_BYTES {
    return Err(limit_error.into());
  }
  if bytes.is_empty() {
    return Err("The audio file is empty.".into());
  }
  let extension = path
    .extension()
    .and_then(|v| v.to_str())
    .filter(|s| s.len() <= 12 && s.bytes().all(|b| b.is_ascii_alphanumeric()))
    .unwrap_or("audio");
  let file = Part::bytes(bytes)
    .file_name(format!("recording.{extension}"))
    .mime_str("application/octet-stream")
    .map_err(|_| "Could not prepare audio upload.")?;
  let mut form = Form::new()
    .part("file", file)
    .text("model", config.model_id.clone())
    .text("response_format", "json");
  if let Some(language) = language {
    // The newer multi-language model accepts repeated language hints.
    let field = if config.model_id == "gpt-transcribe" {
      "languages[]"
    } else {
      "language"
    };
    form = form.text(field, language.to_owned());
  }
  let client = crate::http::client_builder()?
    .redirect(Policy::none())
    .timeout(Duration::from_secs(600))
    .connect_timeout(Duration::from_secs(15))
    .build()
    .map_err(|_| "Could not initialize transcription networking.")?;
  let mut request = client
    .post(format!(
      "{}/audio/transcriptions",
      config.base_url.trim_end_matches('/')
    ))
    .multipart(form);
  if let Some(key) = account.and_then(|a| a.api_key.as_ref()) {
    request = request.bearer_auth(key.expose());
  }
  let mut response = request
    .send()
    .await
    .map_err(|_| "Could not reach the transcription API. Check the connection and try again.")?;
  if !response.status().is_success() {
    return Err(match response.status().as_u16() {
      401 | 403 => "Check the transcription connection’s credential in Apell Settings.".into(),
      404 => {
        "The transcription endpoint or model was not found. Check the connection and model ID."
          .into()
      }
      413 => limit_error.into(),
      429 => "The provider rate limit was reached. Try again later.".into(),
      code => {
        format!("The transcription API returned HTTP {code}. Check the model and audio format.")
      }
    });
  }
  let mut body = Vec::new();
  while let Some(chunk) = response
    .chunk()
    .await
    .map_err(|_| "Could not read the API transcript.")?
  {
    if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
      return Err("The transcription API response was too large.".into());
    }
    body.extend_from_slice(&chunk);
  }
  let transcript: ApiTranscript =
    serde_json::from_slice(&body).map_err(|_| "The API did not return a text transcript.")?;
  let reported_language = transcript
    .language
    .as_deref()
    .or_else(|| transcript.languages.first().map(|v| v.code.as_str()))
    .or(language)
    .unwrap_or("und");
  Ok(json!({"text":transcript.text,"language":reported_language,
    "segments":[], "model_id":config.model_id}))
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
  };

  #[tokio::test]
  async fn uploads_only_the_selected_file_to_the_saved_transcription_endpoint() {
    let listener = TcpListener::bind(("localhost", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let config = LlmConfig {
      base_url: format!("http://{address}/v1"),
      provider_id: "custom".into(),
      connection_id: "fixture".into(),
      model_id: "fixture-asr".into(),
    };
    let account =
      crate::providers::static_account("custom", "fixture", Some("fixture-key")).unwrap();
    let server = std::thread::spawn(move || {
      let (mut stream, _) = listener.accept().unwrap();
      stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
      let mut reader = BufReader::new(&mut stream);
      let mut headers = String::new();
      loop {
        let start = headers.len();
        assert!(reader.read_line(&mut headers).unwrap() > 0);
        if &headers[start..] == "\r\n" {
          break;
        }
      }
      let length: usize = headers
        .lines()
        .find_map(|s| {
          let (name, value) = s.split_once(':')?;
          name
            .eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse().unwrap())
        })
        .unwrap();
      let mut body = vec![0; length];
      reader.read_exact(&mut body).unwrap();
      let response = br#"{"text":"An API transcript"}"#;
      write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.len()
      )
      .unwrap();
      stream.write_all(response).unwrap();
      (headers, String::from_utf8(body).unwrap())
    });
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("private-audio-name.m4a");
    std::fs::write(&path, b"test-audio-bytes").unwrap();
    let result = transcribe(&path, Some("zh"), &config, Some(&account))
      .await
      .unwrap();
    assert_eq!(
      result,
      json!({"text":"An API transcript","model_id":"fixture-asr","segments":[],"language":"zh"})
    );
    let (headers, body) = server.join().unwrap();
    assert!(headers.starts_with("POST /v1/audio/transcriptions HTTP/1.1"));
    assert!(headers
      .to_lowercase()
      .contains("authorization: bearer fixture-key"));
    for field in [
      "name=\"file\"",
      "filename=\"recording.m4a\"",
      "test-audio-bytes",
      "fixture-asr",
      "name=\"language\"",
      "zh",
    ] {
      assert!(body.contains(field), "Missing multipart field {field}");
    }
    assert!(!body.contains("private-audio-name"));
  }
}
