use super::{
  emit, http::Api, now, storage::valid_token, valid_user, CredentialStore, SavedCredential,
};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::json;
#[cfg(unix)]
use std::process::{Command, Stdio};
use std::{
  thread,
  time::{Duration, Instant},
};
use url::Url;

#[derive(Deserialize)]
pub(super) struct Challenge {
  pub device_code: String,
  pub user_code: String,
  pub verification_uri: String,
  pub verification_uri_complete: String,
  pub expires_at: u64,
  pub interval: u64,
}

impl Challenge {
  pub fn validate(&self, store: &Url, time: u64) -> Result<(), String> {
    let alphabet = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let chars = self.user_code.as_bytes();
    let mut base = store.clone();
    base.set_path("/account/cli");
    let mut complete = base.clone();
    complete
      .query_pairs_mut()
      .append_pair("code", &self.user_code);
    if self.device_code.len() != 64
      || !self
        .device_code
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
      || chars.len() != 11
      || chars[5] != b'-'
      || !chars
        .iter()
        .enumerate()
        .all(|(i, c)| i == 5 || alphabet.contains(c))
      || self.verification_uri != base.as_str()
      || self.verification_uri_complete != complete.as_str()
      || self.expires_at <= time
      || self.expires_at > time.saturating_add(660)
      || !(1..=30).contains(&self.interval)
    {
      return Err("The Store returned an invalid browser authorization challenge.".into());
    }
    Ok(())
  }
}

pub fn run(
  store: &Url,
  credentials: &CredentialStore,
  no_browser: bool,
  json_output: bool,
) -> Result<(), String> {
  let api = Api::new(store, None)?;
  let value = api.call(Method::POST, "/api/v1/auth/cli/start", Some(&json!({})))?;
  let challenge: Challenge = serde_json::from_value(value)
    .map_err(|_| "The Store returned an invalid authorization challenge.")?;
  challenge.validate(store, now())?;
  eprintln!(
    "Open {}\nVerify this code before approving: {}",
    challenge.verification_uri_complete, challenge.user_code
  );
  if !no_browser && !open_browser(&challenge.verification_uri_complete) {
    eprintln!("Could not open the default browser. Open the address above manually.");
  }
  let credential = poll(&api, &challenge, thread::sleep)?;
  if let Err(error) = credentials.save(store, &credential) {
    // Never leave an untracked active credential when secure persistence failed.
    if let Ok(authenticated) = Api::new(store, Some(&credential.token)) {
      let _ = authenticated.call(Method::POST, "/api/v1/auth/cli/logout", Some(&json!({})));
    }
    return Err(error);
  }
  emit(
    &json!({"store": store.as_str(), "user": credential.user, "expires_at": credential.expires_at}),
    json_output,
  )
}

pub(super) fn poll(
  api: &Api,
  challenge: &Challenge,
  mut sleep: impl FnMut(Duration),
) -> Result<SavedCredential, String> {
  let started = Instant::now();
  let duration = Duration::from_secs(challenge.expires_at.saturating_sub(now()));
  let mut interval = challenge.interval;
  loop {
    if now().saturating_add(interval) >= challenge.expires_at
      || started
        .elapsed()
        .saturating_add(Duration::from_secs(interval))
        >= duration
    {
      return Err("The browser authorization expired. Run apell login again.".into());
    }
    sleep(Duration::from_secs(interval));
    let reply = api.request(
      Method::POST,
      "/api/v1/auth/cli/poll",
      Some(&json!({"device_code":challenge.device_code})),
    )?;
    match reply.status {
      StatusCode::OK => {
        let credential: SavedCredential = serde_json::from_value(reply.value)
          .map_err(|_| "The Store returned an invalid account session.")?;
        if !valid_token(&credential.token)
          || !valid_user(&credential.user)
          || credential.expires_at <= now()
          || credential.expires_at > now().saturating_add(7 * 24 * 3600 + 60)
        {
          return Err("The Store returned an invalid or expired account session.".into());
        }
        return Ok(credential);
      }
      StatusCode::ACCEPTED
        if reply.value.get("status").and_then(|v| v.as_str()) == Some("pending") =>
      {
        let next = reply
          .value
          .get("interval")
          .and_then(|v| v.as_u64())
          .ok_or("Invalid login polling interval.")?;
        if !(1..=30).contains(&next) {
          return Err("Invalid login polling interval.".into());
        }
        interval = interval.max(next);
      }
      StatusCode::TOO_MANY_REQUESTS if reply.code() == Some("slow_down") => {
        interval = interval.saturating_add(5).min(60)
      }
      _ => return Err(reply.error()),
    }
  }
}

#[cfg(unix)]
fn open_browser(url: &str) -> bool {
  #[cfg(target_os = "macos")]
  let mut command = Command::new("open");
  #[cfg(all(unix, not(target_os = "macos")))]
  let mut command = Command::new("xdg-open");
  let child = command
    .arg(url)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn();
  match child {
    Ok(mut child) => {
      // Some Linux openers remain alive with the browser. Do not block polling.
      thread::spawn(move || {
        let _ = child.wait();
      });
      true
    }
    Err(_) => false,
  }
}

#[cfg(not(unix))]
fn open_browser(_: &str) -> bool {
  false
}
