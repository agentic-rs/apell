use super::{store_root, USAGE};
use serde::Serialize;
use std::{collections::HashSet, ffi::OsString};
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
  Login,
  Whoami,
  Logout,
  Packages,
  Register(String),
  Show(String),
  OidcShow(String),
  OidcSet(String, PublisherInput),
  OidcDisconnect(String),
  Members(String),
  Invite(String, String),
  Remove(String, String),
  Transfer(String, String),
  Invitations,
  Accept(String),
  Decline(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PublisherInput {
  pub repository: String,
  pub workflow: String,
  pub allowed_refs: Vec<String>,
  pub event_names: Vec<String>,
  pub environment: Option<String>,
}

pub struct Args {
  pub command: Command,
  pub store: Option<Url>,
  pub json: bool,
  pub no_browser: bool,
}

impl Args {
  pub fn parse(arguments: &[OsString]) -> Result<Self, String> {
    let words = arguments
      .iter()
      .map(|word| {
        word
          .to_str()
          .map(str::to_owned)
          .ok_or("Arguments must be valid UTF-8.")
      })
      .collect::<Result<Vec<_>, _>>()?;
    let mut store = None;
    let mut json = false;
    let mut no_browser = false;
    let mut remaining = Vec::new();
    let mut cursor = 0;
    while cursor < words.len() {
      match words[cursor].as_str() {
        "--store" if store.is_none() => {
          cursor += 1;
          store = Some(store_root(value(&words, cursor)?)?);
        }
        "--json" if !json => json = true,
        "--no-browser" if !no_browser => no_browser = true,
        "--store" | "--json" | "--no-browser" => return Err(USAGE.into()),
        _ => remaining.push(words[cursor].clone()),
      }
      cursor += 1;
    }
    let words: Vec<&str> = remaining.iter().map(String::as_str).collect();
    let command = match words.as_slice() {
      ["login"] => Command::Login,
      ["whoami"] => Command::Whoami,
      ["logout"] => Command::Logout,
      ["package", "list"] => Command::Packages,
      ["package", "register", id] => Command::Register(package_id(id)?),
      ["package", "show", id] => Command::Show(package_id(id)?),
      ["package", "oidc", "show", id] => Command::OidcShow(package_id(id)?),
      ["package", "oidc", "disconnect", id] => Command::OidcDisconnect(package_id(id)?),
      ["package", "oidc", "set", id, tail @ ..] => {
        Command::OidcSet(package_id(id)?, publisher(tail)?)
      }
      ["package", "members", "list", id] => Command::Members(package_id(id)?),
      ["package", "members", "invite", id, user] => {
        Command::Invite(package_id(id)?, username(user)?)
      }
      ["package", "members", "remove", id, user] => {
        Command::Remove(package_id(id)?, username(user)?)
      }
      ["package", "transfer", id, user] => Command::Transfer(package_id(id)?, username(user)?),
      ["invitations", "list"] => Command::Invitations,
      ["invitations", "accept", id] => Command::Accept(invitation_id(id)?),
      ["invitations", "decline", id] => Command::Decline(invitation_id(id)?),
      _ => return Err(USAGE.into()),
    };
    if no_browser && command != Command::Login {
      return Err("--no-browser only applies to login.".into());
    }
    Ok(Self {
      command,
      store,
      json,
      no_browser,
    })
  }
}

fn value(words: &[String], index: usize) -> Result<&str, String> {
  words
    .get(index)
    .map(String::as_str)
    .filter(|v| !v.is_empty() && !v.starts_with("--"))
    .ok_or_else(|| USAGE.into())
}

fn package_id(value: &str) -> Result<String, String> {
  if value.is_empty()
    || value.len() > 80
    || !value
      .bytes()
      .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
  {
    return Err("Package IDs use 1–80 lowercase letters, numbers, underscores, or hyphens.".into());
  }
  Ok(value.into())
}

fn username(value: &str) -> Result<String, String> {
  if value.is_empty()
    || value.len() > 100
    || !value
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || b == b'-')
  {
    return Err("Use a GitHub username or numeric user ID.".into());
  }
  Ok(value.into())
}

fn invitation_id(value: &str) -> Result<String, String> {
  if value.len() != 64
    || !value
      .bytes()
      .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
  {
    return Err("Use the invitation ID shown by apell invitations list.".into());
  }
  Ok(value.into())
}

fn publisher(words: &[&str]) -> Result<PublisherInput, String> {
  let mut repository = None;
  let mut workflow = None;
  let mut environment = None;
  let mut refs = Vec::new();
  let mut events = Vec::new();
  for pair in words.chunks(2) {
    let [flag, value] = pair else {
      return Err(USAGE.into());
    };
    if value.is_empty() || value.starts_with("--") || value.chars().any(char::is_control) {
      return Err(USAGE.into());
    }
    match *flag {
      "--repo" if repository.is_none() => repository = Some((*value).to_owned()),
      "--workflow" if workflow.is_none() => workflow = Some((*value).to_owned()),
      "--environment" if environment.is_none() => environment = Some((*value).to_owned()),
      "--ref" => refs.push((*value).to_owned()),
      "--event" if matches!(*value, "push" | "workflow_dispatch" | "release") => {
        events.push((*value).to_owned())
      }
      _ => return Err(USAGE.into()),
    }
  }
  if refs.is_empty() {
    refs.push("refs/heads/main".into());
  }
  if events.is_empty() {
    events = vec!["push".into(), "workflow_dispatch".into()];
  }
  if refs.len() > 16
    || events.len() > 3
    || refs.iter().collect::<HashSet<_>>().len() != refs.len()
    || events.iter().collect::<HashSet<_>>().len() != events.len()
  {
    return Err("Provide distinct refs and events within the Store's limits.".into());
  }
  Ok(PublisherInput {
    repository: repository.ok_or("OIDC setup requires --repo OWNER/REPO.")?,
    workflow: workflow.ok_or("OIDC setup requires --workflow .github/workflows/FILE.yml.")?,
    allowed_refs: refs,
    event_names: events,
    environment,
  })
}
