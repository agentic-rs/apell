mod input;

use apell_core::bundles::{digest, parse, Manifest, MAX_BUNDLE_BYTES};
use input::{read_bounded, PublishArgs, PublishMetadata, MAX_ICON_BYTES, MAX_METADATA_BYTES};
use reqwest::{
  blocking::{multipart, Client},
  header::{HeaderValue, AUTHORIZATION},
  redirect::Policy,
  StatusCode,
};
use serde::Deserialize;
use std::{ffi::OsString, io::Read, time::Duration};

pub const USAGE: &str = "Usage: apell publish <file.apel> --store <https-base-url> --metadata <json-file> [--icon <png-file>]";
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

struct PreparedPublish {
  manifest: Manifest,
  sha256: String,
  package: Vec<u8>,
  metadata: Vec<u8>,
  icon: Option<Vec<u8>>,
}

impl PreparedPublish {
  fn load(args: &PublishArgs) -> Result<Self, String> {
    let package = read_bounded(&args.package, MAX_BUNDLE_BYTES, "Package")?;
    let bundle = parse(&package)?;
    let metadata = read_bounded(&args.metadata, MAX_METADATA_BYTES, "Metadata")?;
    let value: PublishMetadata =
      serde_json::from_slice(&metadata).map_err(|_| "Invalid publishing metadata JSON.")?;
    value.validate()?;
    let icon = args
      .icon
      .as_deref()
      .map(|file| {
        let bytes = read_bounded(file, MAX_ICON_BYTES, "Icon")?;
        input::validate_icon(&bytes)?;
        Ok::<_, String>(bytes)
      })
      .transpose()?;
    Ok(Self {
      manifest: bundle.manifest,
      sha256: digest(&package),
      package,
      metadata,
      icon,
    })
  }

  fn form(&self) -> Result<multipart::Form, String> {
    let part = |bytes: Vec<u8>, name: &str, mime: &str| {
      multipart::Part::bytes(bytes)
        .file_name(name.to_owned())
        .mime_str(mime)
        .map_err(|_| "Could not prepare package upload.".to_owned())
    };
    // Fixed upload names avoid disclosing local file names or paths.
    let mut form = multipart::Form::new()
      .part(
        "package",
        part(
          self.package.clone(),
          "applet.apel",
          "application/octet-stream",
        )?,
      )
      .part(
        "metadata",
        part(self.metadata.clone(), "metadata.json", "application/json")?,
      );
    if let Some(icon) = &self.icon {
      form = form.part("icon", part(icon.clone(), "icon.png", "image/png")?);
    }
    Ok(form)
  }
}

#[derive(Debug, Deserialize)]
struct PublishResult {
  applet_id: String,
  version: String,
  sha256: String,
  created: bool,
}

#[derive(Deserialize)]
struct ErrorResponse {
  error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
  code: String,
  message: String,
}

pub fn run(arguments: &[OsString]) -> Result<(), String> {
  let args = PublishArgs::parse(arguments)?;
  let prepared = PreparedPublish::load(&args)?;
  let token_file = std::env::var_os("APELL_STORE_PUBLISH_TOKEN_FILE")
    .filter(|value| !value.is_empty())
    .ok_or("Set APELL_STORE_PUBLISH_TOKEN_FILE to a file containing the publishing token.")?;
  let token = input::read_token(std::path::Path::new(&token_file))?;
  let published = send(&args, &prepared, &token)?;
  println!(
    "{} {} {}. SHA-256 {}",
    if published.created {
      "Published"
    } else {
      "Already published"
    },
    published.applet_id,
    published.version,
    published.sha256,
  );
  Ok(())
}

fn send(
  args: &PublishArgs,
  prepared: &PreparedPublish,
  token: &str,
) -> Result<PublishResult, String> {
  let mut client = Client::builder()
    .redirect(Policy::none())
    .connect_timeout(Duration::from_secs(15))
    .timeout(Duration::from_secs(120));
  if args.endpoint.scheme() == "http" {
    // Loopback development tokens must never leave the machine through a proxy.
    client = client.no_proxy();
  }
  let client = client
    .build()
    .map_err(|_| "Could not initialize the publishing client.")?;
  let mut authorization =
    HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| "Invalid publishing token.")?;
  authorization.set_sensitive(true);
  let response = client
    .post(args.endpoint.clone())
    .header(AUTHORIZATION, authorization)
    .multipart(prepared.form()?)
    .send()
    .map_err(|error| {
      if error.is_timeout() {
        "Publishing timed out. Check the store before retrying."
      } else {
        "Could not contact the store. Check the store address and connection."
      }
    })?;
  let status = response.status();
  if status.is_redirection() {
    return Err("The store returned a redirect. Publishing redirects are disabled.".into());
  }
  if response
    .content_length()
    .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
  {
    return Err("The store returned an oversized response.".into());
  }
  let mut bytes = Vec::new();
  response
    .take(MAX_RESPONSE_BYTES as u64 + 1)
    .read_to_end(&mut bytes)
    .map_err(|_| "Could not read the store response. Check the store before retrying.")?;
  if bytes.len() > MAX_RESPONSE_BYTES {
    return Err("The store returned an oversized response.".into());
  }
  decode_response(status, &bytes, prepared, token)
}

fn decode_response(
  status: StatusCode,
  bytes: &[u8],
  prepared: &PreparedPublish,
  token: &str,
) -> Result<PublishResult, String> {
  if !status.is_success() {
    if let Ok(error) = serde_json::from_slice::<ErrorResponse>(bytes) {
      let detail = error.error;
      if !detail.code.is_empty()
        && detail.code.len() <= 64
        && detail
          .code
          .bytes()
          .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        && !detail.message.is_empty()
        && detail.message.len() <= 2_000
        && !detail.message.chars().any(char::is_control)
      {
        return Err(
          format!("Store error [{}]: {}", detail.code, detail.message).replace(token, "[redacted]"),
        );
      }
    }
    return Err(format!(
      "The store rejected publishing (HTTP {}).",
      status.as_u16()
    ));
  }
  let published: PublishResult = serde_json::from_slice(bytes)
    .map_err(|_| "The store returned an invalid publishing result.")?;
  if published.applet_id != prepared.manifest.applet_id
    || published.version != prepared.manifest.version
    || published.sha256 != prepared.sha256
  {
    return Err("The store returned a different package identity or checksum.".into());
  }
  Ok(published)
}

#[cfg(test)]
mod tests;
