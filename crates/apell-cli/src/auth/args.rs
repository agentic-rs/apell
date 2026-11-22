use serde::Serialize;
use std::{ffi::OsString, path::PathBuf};
use url::Url;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ClientType {
  Web,
  Native,
  Spa,
}

impl ClientType {
  pub fn provider_type(self) -> &'static str {
    match self {
      Self::Web => "Traditional",
      Self::Native => "Native",
      Self::Spa => "SPA",
    }
  }
}

pub(super) struct Args {
  pub name: String,
  pub client_type: ClientType,
  pub redirect_uris: Vec<String>,
  pub post_logout_redirect_uris: Vec<String>,
  pub output: PathBuf,
  pub first_party: bool,
  pub device_flow: bool,
  pub json: bool,
}

impl Args {
  pub fn parse(arguments: &[OsString]) -> Result<Self, String> {
    if arguments.first().is_none_or(|value| value != "app")
      || arguments.get(1).is_none_or(|value| value != "register")
    {
      return Err(super::USAGE.into());
    }
    let mut name = None;
    let mut client_type = None;
    let mut redirect_uris = Vec::new();
    let mut post_logout_redirect_uris = Vec::new();
    let mut output = None;
    let mut first_party = false;
    let mut device_flow = false;
    let mut json = false;
    let mut options = arguments[2..].iter();
    while let Some(option) = options.next() {
      match option.to_str() {
        Some("--first-party") if !first_party => first_party = true,
        Some("--device-flow") if !device_flow => device_flow = true,
        Some("--json") if !json => json = true,
        Some("--output") if output.is_none() => {
          output = Some(PathBuf::from(
            options
              .next()
              .filter(|value| !value.is_empty() && !value.to_string_lossy().starts_with("--"))
              .ok_or("--output requires a file path.")?,
          ));
        }
        Some("--name" | "--type" | "--redirect-uri" | "--post-logout-redirect-uri") => {
          let value = options
            .next()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty() && !value.starts_with("--"))
            .ok_or("Each registration option requires a value.")?;
          match option.to_str() {
            Some("--name") if name.is_none() => name = Some(value.to_owned()),
            Some("--type") if client_type.is_none() => {
              client_type = Some(match value {
                "web" => ClientType::Web,
                "native" => ClientType::Native,
                "spa" => ClientType::Spa,
                _ => return Err("Choose --type web, native, or spa.".into()),
              });
            }
            Some("--redirect-uri") => redirect_uris.push(value.to_owned()),
            Some("--post-logout-redirect-uri") => {
              post_logout_redirect_uris.push(value.to_owned());
            }
            _ => return Err("Specify --name and --type once each.".into()),
          }
        }
        _ => return Err(super::USAGE.into()),
      }
    }
    let name = name.ok_or("Specify --name for the application.")?;
    if name.trim() != name || name.chars().count() > 256 || name.chars().any(char::is_control) {
      return Err("Use an application name of 1–256 characters without surrounding whitespace or control characters.".into());
    }
    let client_type = client_type.ok_or("Choose --type web, native, or spa.")?;
    if device_flow {
      if client_type != ClientType::Native {
        return Err("--device-flow requires --type native.".into());
      }
      if !redirect_uris.is_empty() || !post_logout_redirect_uris.is_empty() {
        return Err("Device flow does not use redirect callbacks.".into());
      }
    } else if redirect_uris.is_empty() {
      return Err("Specify at least one --redirect-uri callback.".into());
    }
    for value in redirect_uris.iter().chain(&post_logout_redirect_uris) {
      validate_callback(value, client_type)?;
    }
    let output = output
      .filter(|path| path.file_name().is_some())
      .ok_or("Specify --output for the private registration file.")?;
    Ok(Self {
      name,
      client_type,
      redirect_uris,
      post_logout_redirect_uris,
      output,
      first_party,
      device_flow,
      json,
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn registration(options: &[&str]) -> Result<Args, String> {
    let arguments = [
      "app",
      "register",
      "--name",
      "Fixture",
      "--output",
      ".env.fixture.json",
    ]
    .into_iter()
    .chain(options.iter().copied())
    .map(OsString::from)
    .collect::<Vec<_>>();
    Args::parse(&arguments)
  }

  #[test]
  fn device_flow_is_a_native_registration_without_callbacks() {
    let registration = registration(&["--type", "native", "--device-flow"]).unwrap();
    assert!(registration.device_flow);
    assert!(registration.redirect_uris.is_empty());
  }

  #[test]
  fn rejects_device_flow_with_incompatible_types_and_callback_options() {
    for client_type in ["web", "spa"] {
      assert!(registration(&["--type", client_type, "--device-flow"]).is_err());
    }
    for callback in ["--redirect-uri", "--post-logout-redirect-uri"] {
      assert!(registration(&[
        "--type",
        "native",
        "--device-flow",
        callback,
        "https://example.invalid/callback"
      ])
      .is_err());
    }
    assert!(registration(&["--type", "native"]).is_err());
  }
}

pub(super) fn is_loopback(url: &Url) -> bool {
  url.host_str().is_some_and(|host| match host {
    "localhost" | "[::1]" => true,
    _ => host
      .parse::<std::net::Ipv4Addr>()
      .is_ok_and(|address| address.is_loopback()),
  })
}

fn validate_callback(value: &str, client_type: ClientType) -> Result<(), String> {
  let invalid = || {
    "Use exact callback URLs without credentials, fragments, whitespace, backslashes, or wildcards. Web and SPA callbacks require HTTPS or loopback HTTP; native clients also support private app schemes.".to_owned()
  };
  if value.len() > 2048
    || value.chars().any(|character| {
      character.is_control() || character.is_whitespace() || matches!(character, '*' | '\\')
    })
  {
    return Err(invalid());
  }
  let url = Url::parse(value).map_err(|_| invalid())?;
  if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
    return Err(invalid());
  }
  match url.scheme() {
    "https" if url.host_str().is_some() => Ok(()),
    "http" if is_loopback(&url) => Ok(()),
    "http" | "https" => Err(invalid()),
    "file" | "data" | "javascript" | "vbscript" | "about" | "blob" | "ftp" | "mailto" | "tel"
    | "ws" | "wss" => Err(invalid()),
    _ if client_type == ClientType::Native && !url.cannot_be_a_base() => Ok(()),
    _ => Err(invalid()),
  }
}
