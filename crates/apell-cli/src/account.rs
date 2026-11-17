mod args;
mod commands;
mod http;
mod login;
mod storage;
#[cfg(test)]
mod tests;

pub(crate) use storage::{CredentialStore, SavedCredential};

use serde::{Deserialize, Serialize};
use std::{
  ffi::OsString,
  time::{SystemTime, UNIX_EPOCH},
};
use url::Url;

pub const USAGE: &str = "Account commands (all accept --store URL and --json):
  apell login [--store URL] [--no-browser]
  apell whoami | logout
  apell package list | register ID | show ID
  apell package oidc show ID | disconnect ID
  apell package oidc set ID --repo OWNER/REPO --workflow FILE [--ref REF] [--event EVENT] [--environment NAME]
  apell package members list ID | invite ID USER | remove ID USER
  apell package transfer ID USER
  apell invitations list | accept ID | decline ID
Login remembers the default Store. OIDC set replaces the full connection; default ref is refs/heads/main and events are push,workflow_dispatch.";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct User {
  pub github_id: u64,
  pub github_login: String,
}

pub(crate) fn valid_user(user: &User) -> bool {
  user.github_id > 0
    && user.github_id <= i64::MAX as u64
    && !user.github_login.is_empty()
    && user.github_login.len() <= 100
    && user
      .github_login
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

pub(crate) fn now() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |value| value.as_secs())
}

pub(crate) fn store_root(value: &str) -> Result<Url, String> {
  let mut url = crate::publish::input::store_endpoint(value)?;
  url.set_path("/");
  Ok(url)
}

pub fn run(arguments: &[OsString]) -> Result<(), String> {
  let args = args::Args::parse(arguments)?;
  let credentials = CredentialStore::open()?;
  let store = match &args.store {
    Some(store) => store.clone(),
    None => credentials
      .default_store()?
      .ok_or("Choose a Store with --store URL, or run apell login --store URL first.")?,
  };
  if args.command == args::Command::Login {
    return login::run(&store, &credentials, args.no_browser, args.json);
  }
  commands::run(&args.command, &store, &credentials, args.json)
}

pub(crate) fn default_store() -> Result<Option<Url>, String> {
  CredentialStore::open()?.default_store()
}

pub(crate) fn emit(value: &serde_json::Value, json: bool) -> Result<(), String> {
  let encoded = if json {
    serde_json::to_string(value)
  } else {
    serde_json::to_string_pretty(value)
  }
  .map_err(|_| "Could not format the Store response.")?;
  println!("{encoded}");
  Ok(())
}
