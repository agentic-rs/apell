use super::{
  config::{valid_ref, TrustedPublisher},
  diagnostics::{reject, Rejection},
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
    let reason = if self.iss != ISSUER {
      Some(Rejection::ClaimIssuer)
    } else if self.aud != audience {
      Some(Rejection::ClaimAudience)
    } else if self.sub.is_empty() || self.sub.len() > 1024 || self.sub.chars().any(char::is_control)
    {
      Some(Rejection::ClaimSubject)
    } else if self.jti.is_empty()
      || self.jti.len() > 256
      || !self.jti.bytes().all(|c| c.is_ascii_graphic())
    {
      Some(Rejection::ClaimTokenId)
    } else if self.exp <= now {
      Some(Rejection::ClaimExpired)
    } else if self.exp <= self.iat || self.exp - self.iat > 600 {
      Some(Rejection::ClaimLifetime)
    } else if self.iat > now.saturating_add(30) {
      Some(Rejection::ClaimIssuedInFuture)
    } else if self.nbf > now.saturating_add(30) {
      Some(Rejection::ClaimNotYetValid)
    } else if self.nbf >= self.exp {
      Some(Rejection::ClaimTimeOrder)
    } else if self.runner_environment != "github-hosted" {
      Some(Rejection::ClaimRunner)
    } else if self.job_workflow_ref.is_some() {
      Some(Rejection::ClaimReusableWorkflow)
    } else if !matches!(
      self.event_name.as_str(),
      "push" | "workflow_dispatch" | "release"
    ) {
      Some(Rejection::ClaimEvent)
    } else if !valid_ref(&self.git_ref) {
      Some(Rejection::ClaimRef)
    } else if !valid_repository(&self.repository) {
      Some(Rejection::ClaimRepository)
    } else if self
      .repository_id
      .parse::<u64>()
      .ok()
      .is_none_or(|id| id == 0)
    {
      Some(Rejection::ClaimRepositoryId)
    } else if self
      .repository_owner_id
      .parse::<u64>()
      .ok()
      .is_none_or(|id| id == 0)
    {
      Some(Rejection::ClaimOwnerId)
    } else {
      None
    };
    reason.map_or(Ok(()), |reason| Err(reject(reason)))
  }

  pub fn policy_failure(
    &self,
    publishers: &[TrustedPublisher],
    applet_id: Option<&str>,
  ) -> Rejection {
    let mut candidates: Vec<_> = publishers
      .iter()
      .filter(|publisher| applet_id.is_none_or(|id| publisher.applet_id == id))
      .collect();
    if candidates.is_empty() {
      return Rejection::PolicyApplet;
    }
    candidates.retain(|publisher| self.repository_id == publisher.repository_id.to_string());
    if candidates.is_empty() {
      return Rejection::PolicyRepositoryId;
    }
    candidates
      .retain(|publisher| self.repository_owner_id == publisher.repository_owner_id.to_string());
    if candidates.is_empty() {
      return Rejection::PolicyOwnerId;
    }
    candidates.retain(|publisher| {
      self.workflow_ref
        == format!(
          "{}/{}@{}",
          self.repository, publisher.workflow, self.git_ref
        )
    });
    if candidates.is_empty() {
      return Rejection::PolicyWorkflow;
    }
    candidates.retain(|publisher| publisher.allowed_refs.contains(&self.git_ref));
    if candidates.is_empty() {
      return Rejection::PolicyRef;
    }
    candidates.retain(|publisher| publisher.event_names.contains(&self.event_name));
    if candidates.is_empty() {
      return Rejection::PolicyEvent;
    }
    candidates.retain(|publisher| {
      publisher
        .environment
        .as_ref()
        .is_none_or(|name| self.environment.as_ref() == Some(name))
    });
    if candidates.is_empty() {
      return Rejection::PolicyEnvironment;
    }
    Rejection::TokenInvalid
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
