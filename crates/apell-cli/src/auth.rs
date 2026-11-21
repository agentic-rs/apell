mod args;
mod client;
mod output;

use args::{Args, ClientType};
use client::Api;
use output::OutputFile;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::ffi::OsString;

pub const USAGE: &str = "Account application registration (operator access):
  apell auth app register --name NAME --type web|native|spa --redirect-uri URI --output FILE
    [--redirect-uri URI ...] [--post-logout-redirect-uri URI ...] [--first-party] [--json]
Set ACCOUNT_MANAGEMENT_ENDPOINT and ACCOUNT_MANAGEMENT_TOKEN in the operator environment.
Registrations require user consent by default. Use --first-party for trusted services.
Client credentials are written to a new private file; terminal output contains public fields.";

#[derive(Deserialize)]
struct Discovery {
  issuer: String,
}

#[derive(Deserialize)]
struct Application {
  id: String,
  #[serde(rename = "type")]
  application_type: String,
  #[serde(rename = "isThirdParty")]
  is_third_party: bool,
  #[serde(rename = "oidcClientMetadata")]
  oidc_client_metadata: ClientMetadata,
}

#[derive(Deserialize)]
struct ClientMetadata {
  #[serde(rename = "redirectUris")]
  redirect_uris: Vec<String>,
  #[serde(rename = "postLogoutRedirectUris")]
  post_logout_redirect_uris: Vec<String>,
}

#[derive(Deserialize)]
struct ApplicationSecret {
  value: String,
  #[serde(default, rename = "expiresAt")]
  expires_at: Option<i64>,
}

#[derive(Serialize)]
struct Registration<'a> {
  issuer: &'a str,
  client_id: &'a str,
  client_type: ClientType,
  is_first_party: bool,
  redirect_uris: &'a [String],
  post_logout_redirect_uris: &'a [String],
  #[serde(skip_serializing_if = "Option::is_none")]
  client_secret: Option<String>,
}

pub fn run(arguments: &[OsString]) -> Result<(), String> {
  if arguments
    .last()
    .is_some_and(|value| value == "--help" || value == "-h")
  {
    println!("{USAGE}");
    return Ok(());
  }
  let args = Args::parse(arguments)?;
  let output = OutputFile::prepare(&args.output)?;
  let api = Api::from_environment()?;
  let discovery: Discovery = api.discover()?;
  client::secure_url(&discovery.issuer)?;
  let application: Application = api.json(
    Method::POST,
    "/api/applications",
    Some(&json!({
      "name": args.name,
      "type": args.client_type.provider_type(),
      "isThirdParty": !args.first_party,
      "oidcClientMetadata": {
        "redirectUris": args.redirect_uris,
        "postLogoutRedirectUris": args.post_logout_redirect_uris,
      },
    })),
  )?;
  if application.id.is_empty()
    || application.id.len() > 64
    || !application
      .id
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
  {
    return Err("Apell Account returned an invalid client ID. Inspect Account applications before retrying registration.".into());
  }
  let application_path = format!("/api/applications/{}", application.id);
  let result = configure(
    &api,
    &application_path,
    &application,
    &args,
    &discovery.issuer,
    output,
  );
  if let Err(error) = result {
    return match api.empty(Method::DELETE, &application_path, None) {
      Ok(()) => Err(format!("{error} The newly created application was removed.")),
      Err(_) => Err(format!("{error} Cleanup failed for client {}. Inspect this application in Account administration before retrying.", application.id)),
    };
  }
  let summary = json!({
    "issuer": discovery.issuer,
    "client_id": application.id,
    "client_type": args.client_type,
    "is_first_party": args.first_party,
    "redirect_uris": args.redirect_uris,
    "post_logout_redirect_uris": args.post_logout_redirect_uris,
    "output": args.output.to_string_lossy(),
  });
  let encoded = if args.json {
    serde_json::to_string(&summary)
  } else {
    serde_json::to_string_pretty(&summary)
  }
  .map_err(|_| "Could not format the registration summary.")?;
  println!("{encoded}");
  Ok(())
}

fn configure(
  api: &Api,
  application_path: &str,
  application: &Application,
  args: &Args,
  issuer: &str,
  output: OutputFile,
) -> Result<(), String> {
  if application.application_type != args.client_type.provider_type()
    || application.is_third_party == args.first_party
    || application.oidc_client_metadata.redirect_uris != args.redirect_uris
    || application.oidc_client_metadata.post_logout_redirect_uris != args.post_logout_redirect_uris
  {
    return Err("Apell Account returned a different registration than requested.".into());
  }
  let client_secret = if args.client_type == ClientType::Web {
    let secrets: Vec<ApplicationSecret> =
      api.json(Method::GET, &format!("{application_path}/secrets"), None)?;
    Some(
      secrets
        .into_iter()
        .find(|secret| secret.expires_at.is_none() && !secret.value.is_empty())
        .ok_or("Apell Account did not return a permanent web client secret.")?
        .value,
    )
  } else {
    None
  };
  if !args.first_party {
    api.empty(
      Method::POST,
      &format!("{application_path}/user-consent-scopes"),
      Some(&json!({
        "userScopes": ["profile", "email"],
      })),
    )?;
  }
  output.save(&Registration {
    issuer,
    client_id: &application.id,
    client_type: args.client_type,
    is_first_party: args.first_party,
    redirect_uris: &args.redirect_uris,
    post_logout_redirect_uris: &args.post_logout_redirect_uris,
    client_secret,
  })
}
