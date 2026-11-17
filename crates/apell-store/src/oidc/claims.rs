use super::{
  config::{valid_ref, TrustedPublisher},
  unauthorized,
};
use crate::Result;
use serde::Deserialize;

pub(super) const ISSUER: &str = "https://token.actions.githubusercontent.com";

#[derive(Clone, Deserialize)]
pub(super) struct Claims {
  pub iss: String,
  pub aud: String,
  pub sub: String,
  pub exp: u64,
  pub iat: u64,
  pub nbf: u64,
  pub jti: String,
  pub repository_id: String,
  pub repository_owner_id: String,
  pub repository: String,
  pub workflow_ref: String,
  #[serde(rename = "ref")]
  pub git_ref: String,
  pub event_name: String,
  pub runner_environment: String,
  pub environment: Option<String>,
  pub job_workflow_ref: Option<String>,
}

impl Claims {
  pub fn validate(&self, audience: &str, now: u64) -> Result<()> {
    if self.iss != ISSUER
      || self.aud != audience
      || self.sub.is_empty()
      || self.sub.len() > 1024
      || self.sub.chars().any(char::is_control)
      || self.jti.is_empty()
      || self.jti.len() > 256
      || !self.jti.bytes().all(|c| c.is_ascii_graphic())
      || self.exp <= now
      || self.exp <= self.iat
      || self.exp - self.iat > 600
      || self.iat > now.saturating_add(30)
      || self.nbf > now.saturating_add(30)
      || self.nbf >= self.exp
      || self.runner_environment != "github-hosted"
      || self.job_workflow_ref.is_some()
      || !matches!(
        self.event_name.as_str(),
        "push" | "workflow_dispatch" | "release"
      )
      || !valid_ref(&self.git_ref)
      || !valid_repository(&self.repository)
      || self
        .repository_id
        .parse::<u64>()
        .ok()
        .is_none_or(|id| id == 0)
      || self
        .repository_owner_id
        .parse::<u64>()
        .ok()
        .is_none_or(|id| id == 0)
    {
      return Err(unauthorized());
    }
    Ok(())
  }

  pub fn matches(&self, publisher: &TrustedPublisher) -> bool {
    self.repository_id == publisher.repository_id.to_string()
      && self.repository_owner_id == publisher.repository_owner_id.to_string()
      && publisher.allowed_refs.contains(&self.git_ref)
      && publisher.event_names.contains(&self.event_name)
      && publisher
        .environment
        .as_ref()
        .is_none_or(|name| self.environment.as_ref() == Some(name))
      && self.workflow_ref
        == format!(
          "{}/{}@{}",
          self.repository, publisher.workflow, self.git_ref
        )
  }
}

fn valid_repository(value: &str) -> bool {
  if value.len() > 201 {
    return false;
  }
  let Some((owner, name)) = value.split_once('/') else {
    return false;
  };
  !owner.is_empty()
    && !name.is_empty()
    && owner
      .bytes()
      .all(|c| c.is_ascii_alphanumeric() || c == b'-')
    && name
      .bytes()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
    && !matches!(name, "." | "..")
}
