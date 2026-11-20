use super::{unavailable, OAuthConfig, User};
use crate::{Result, StoreError};
use reqwest::{header, Client, Response};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::Semaphore;
use url::Url;

const MAX_RESPONSE_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepositoryIdentity {
  pub repository_id: u64,
  pub repository_owner_id: u64,
  pub repository: String,
}

pub(super) struct GitHub {
  client: Client,
  api: Url,
  token_url: Url,
  requests: Semaphore,
}

#[derive(Deserialize)]
struct Profile {
  id: u64,
  login: String,
  #[serde(rename = "type")]
  kind: String,
}

#[derive(Deserialize)]
struct Repository {
  id: u64,
  full_name: String,
  owner: RepositoryOwner,
}

#[derive(Deserialize)]
struct RepositoryOwner {
  id: u64,
}

#[derive(Deserialize)]
struct AccessToken {
  access_token: String,
  token_type: String,
}

impl GitHub {
  pub fn new() -> Result<Self> {
    Self::with_endpoints(
      Url::parse("https://api.github.com/").map_err(|_| StoreError::internal())?,
      Url::parse("https://github.com/login/oauth/access_token")
        .map_err(|_| StoreError::internal())?,
    )
  }

  fn with_endpoints(api: Url, token_url: Url) -> Result<Self> {
    let client = Client::builder()
      .user_agent("Apell-Store")
      .redirect(reqwest::redirect::Policy::none())
      .connect_timeout(Duration::from_secs(10))
      .timeout(Duration::from_secs(20))
      .build()
      .map_err(|_| StoreError::internal())?;
    Ok(Self {
      client,
      api,
      token_url,
      requests: Semaphore::new(4),
    })
  }

  #[cfg(test)]
  pub fn fixture(api: Url, token_url: Url) -> Self {
    Self::with_endpoints(api, token_url).unwrap()
  }

  pub async fn sign_in(&self, config: &OAuthConfig, code: &str, verifier: &str) -> Result<User> {
    let _permit = self
      .requests
      .try_acquire()
      .map_err(|_| StoreError::busy())?;
    if code.is_empty() || code.len() > 1024 || !code.bytes().all(|byte| byte.is_ascii_graphic()) {
      return Err(unavailable());
    }
    let body = url::form_urlencoded::Serializer::new(String::new())
      .extend_pairs([
        ("client_id", config.client_id.as_str()),
        ("client_secret", config.client_secret.as_str()),
        ("code", code),
        ("redirect_uri", config.callback().as_str()),
        ("code_verifier", verifier),
      ])
      .finish();
    let response = self
      .client
      .post(self.token_url.clone())
      .header(header::ACCEPT, "application/json")
      .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
      .body(body)
      .send()
      .await
      .map_err(|_| unavailable())?;
    let token: AccessToken = decode(response).await?;
    if !token.token_type.eq_ignore_ascii_case("bearer")
      || token.access_token.is_empty()
      || token.access_token.len() > 4096
      || !token
        .access_token
        .bytes()
        .all(|byte| byte.is_ascii_graphic())
    {
      return Err(unavailable());
    }
    let response = self
      .client
      .get(self.endpoint(&["user"])?)
      .bearer_auth(&token.access_token)
      .header(header::ACCEPT, "application/vnd.github+json")
      .header("X-GitHub-Api-Version", "2022-11-28")
      .send()
      .await
      .map_err(|_| unavailable())?;
    profile(decode(response).await?)
  }

  pub async fn lookup_user(&self, login: &str) -> Result<User> {
    if !valid_login(login) {
      return Err(StoreError::invalid("Enter a valid GitHub username."));
    }
    profile(self.get(&["users", login]).await?)
  }

  pub(super) async fn lookup_user_id(&self, id: u64) -> Result<User> {
    let user = profile(self.get(&["user", &id.to_string()]).await?)?;
    if user.github_id != id {
      return Err(unavailable());
    }
    Ok(user)
  }

  pub async fn lookup_repository(&self, name: &str) -> Result<RepositoryIdentity> {
    let Some((owner, repository)) = name.split_once('/') else {
      return Err(StoreError::invalid(
        "Enter a GitHub repository as owner/repository.",
      ));
    };
    if !valid_login(owner)
      || repository.is_empty()
      || repository.len() > 100
      || matches!(repository, "." | "..")
      || !repository
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
      return Err(StoreError::invalid(
        "Enter a GitHub repository as owner/repository.",
      ));
    }
    let result: Repository = self.get(&["repos", owner, repository]).await?;
    if result.id == 0 || result.owner.id == 0 || !result.full_name.eq_ignore_ascii_case(name) {
      return Err(unavailable());
    }
    Ok(RepositoryIdentity {
      repository_id: result.id,
      repository_owner_id: result.owner.id,
      repository: result.full_name,
    })
  }

  async fn get<T: DeserializeOwned>(&self, segments: &[&str]) -> Result<T> {
    let _permit = self
      .requests
      .try_acquire()
      .map_err(|_| StoreError::busy())?;
    let response = self
      .client
      .get(self.endpoint(segments)?)
      .header(header::ACCEPT, "application/vnd.github+json")
      .header("X-GitHub-Api-Version", "2022-11-28")
      .send()
      .await
      .map_err(|_| unavailable())?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
      return Err(StoreError::new(
        axum::http::StatusCode::NOT_FOUND,
        "github_not_found",
        "That public GitHub user or repository could not be found.",
      ));
    }
    decode(response).await
  }

  fn endpoint(&self, segments: &[&str]) -> Result<Url> {
    let mut url = self.api.clone();
    url
      .path_segments_mut()
      .map_err(|_| StoreError::internal())?
      .extend(segments);
    Ok(url)
  }
}

pub(super) fn valid_login(login: &str) -> bool {
  !login.is_empty()
    && login.len() <= 39
    && !login.starts_with('-')
    && !login.ends_with('-')
    && !login.contains("--")
    && login
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn profile(profile: Profile) -> Result<User> {
  if profile.id == 0
    || profile.id > i64::MAX as u64
    || profile.kind != "User"
    || !valid_login(&profile.login)
  {
    return Err(unavailable());
  }
  Ok(User {
    github_id: profile.id,
    github_login: profile.login,
  })
}

async fn decode<T: DeserializeOwned>(mut response: Response) -> Result<T> {
  if !response.status().is_success()
    || response
      .content_length()
      .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
  {
    return Err(unavailable());
  }
  let mut bytes = Vec::new();
  while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
    if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
      return Err(unavailable());
    }
    bytes.extend_from_slice(&chunk);
  }
  serde_json::from_slice(&bytes).map_err(|_| unavailable())
}
