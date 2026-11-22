mod account;
mod cli;
mod config;
mod config_sync;
mod github;
mod routes;
mod sessions;
#[cfg(test)]
mod tests;

pub use config::{AccountConfig, OAuthConfig};
pub(crate) use config_sync::ConfigSyncAuth;
pub use config_sync::ConfigSyncConfig;
pub use github::RepositoryIdentity;
pub(crate) use routes::routes;
pub(crate) use sessions::migrate;

use crate::{Result, Store, StoreError};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct User {
  pub github_id: u64,
  pub github_login: String,
}

pub struct Auth {
  origin: String,
  sign_in: SignIn,
  store: Store,
  github: github::GitHub,
}

enum SignIn {
  Github(OAuthConfig),
  Account(account::Account),
}

#[derive(Copy, Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LoginProvider {
  Github,
  Account,
}

impl Auth {
  pub fn new(config: OAuthConfig, store: Store) -> Result<Self> {
    Self::with_provider(config.origin.clone(), SignIn::Github(config), store)
  }

  pub fn new_account(config: AccountConfig, store: Store) -> Result<Self> {
    let redirect_uri = config.callback();
    let account = account::Account::new(
      &config.issuer,
      config.client_id,
      config.client_secret,
      redirect_uri,
    )?;
    Self::with_provider(config.origin, SignIn::Account(account), store)
  }

  fn with_provider(origin: String, sign_in: SignIn, store: Store) -> Result<Self> {
    Ok(Self {
      origin,
      sign_in,
      store,
      github: github::GitHub::new()?,
    })
  }

  fn login_provider(&self) -> LoginProvider {
    match self.sign_in {
      SignIn::Github(_) => LoginProvider::Github,
      SignIn::Account(_) => LoginProvider::Account,
    }
  }

  pub fn user(&self, headers: &HeaderMap) -> Result<User> {
    if headers.contains_key("authorization") {
      return self.cli_user(headers);
    }
    self.browser_user(headers)
  }

  pub fn cli_user(&self, headers: &HeaderMap) -> Result<User> {
    Ok(self.store.cli_identity(&cli::bearer(headers)?, now())?.user)
  }

  pub fn browser_user(&self, headers: &HeaderMap) -> Result<User> {
    if headers.contains_key("authorization") {
      return Err(unauthorized());
    }
    let secret = sessions::cookie(headers, sessions::SESSION_COOKIE)?.ok_or_else(unauthorized)?;
    self.store.session_user(&secret, now())
  }

  pub fn require_mutation(&self, headers: &HeaderMap) -> Result<User> {
    if headers.contains_key("authorization") {
      return self.cli_user(headers);
    }
    self.require_browser_mutation(headers)
  }

  pub fn require_browser_mutation(&self, headers: &HeaderMap) -> Result<User> {
    let user = self.browser_user(headers)?;
    if headers.get_all("origin").iter().count() != 1
      || headers.get("origin").and_then(|value| value.to_str().ok()) != Some(self.origin.as_str())
      || headers.get_all("x-csrf-token").iter().count() != 1
    {
      return Err(forbidden());
    }
    let secret = sessions::cookie(headers, sessions::SESSION_COOKIE)?.ok_or_else(unauthorized)?;
    let supplied = headers
      .get("x-csrf-token")
      .and_then(|value| value.to_str().ok())
      .ok_or_else(forbidden)?;
    if !bool::from(csrf_token(&secret).as_bytes().ct_eq(supplied.as_bytes())) {
      return Err(forbidden());
    }
    Ok(user)
  }

  pub async fn lookup_user(&self, login: &str) -> Result<User> {
    self.github.lookup_user(login).await
  }

  pub async fn lookup_repository(&self, repository: &str) -> Result<RepositoryIdentity> {
    self.github.lookup_repository(repository).await
  }
}

fn now() -> i64 {
  chrono::Utc::now().timestamp()
}

fn random_secret() -> Result<String> {
  let mut bytes = [0; 32];
  getrandom::fill(&mut bytes).map_err(|_| StoreError::internal())?;
  Ok(apell_package::digest(&bytes))
}

fn hash(secret: &str) -> Vec<u8> {
  Sha256::digest(secret.as_bytes()).to_vec()
}

fn csrf_token(secret: &str) -> String {
  apell_package::digest(format!("apell-store-csrf\0{secret}").as_bytes())
}

fn unauthorized() -> StoreError {
  StoreError::new(
    StatusCode::UNAUTHORIZED,
    "sign_in_required",
    "Sign in to the Store to continue.",
  )
}

fn forbidden() -> StoreError {
  StoreError::new(
    StatusCode::FORBIDDEN,
    "invalid_session_request",
    "Refresh this page and try again.",
  )
}

fn unavailable() -> StoreError {
  StoreError::new(
    StatusCode::BAD_GATEWAY,
    "github_unavailable",
    "GitHub is unavailable. Please try again.",
  )
}
