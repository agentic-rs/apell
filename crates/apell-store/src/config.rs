use crate::{
  auth::{AccountConfig, OAuthConfig, User},
  OidcConfig,
};
use std::{net::SocketAddr, path::PathBuf};

pub struct Config {
  pub listen: SocketAddr,
  pub data_dir: PathBuf,
  pub web_dir: PathBuf,
  pub publish_token: String,
  pub oidc: Option<OidcConfig>,
  pub oauth: Option<OAuthConfig>,
  pub account: Option<AccountConfig>,
  pub bootstrap_owner: Option<User>,
}

impl Config {
  pub fn from_env() -> std::result::Result<Self, String> {
    let token_path = std::env::var_os("APELL_STORE_PUBLISH_TOKEN_FILE")
      .ok_or("Set APELL_STORE_PUBLISH_TOKEN_FILE before starting the store.")?;
    let file =
      std::fs::File::open(token_path).map_err(|_| "Could not open the publishing token file.")?;
    let metadata = file
      .metadata()
      .map_err(|_| "Could not inspect the publishing token file.")?;
    if !metadata.is_file() || metadata.len() > 514 {
      return Err("Publishing token must be a regular file of at most 514 bytes.".into());
    }
    let mut token = String::new();
    use std::io::Read;
    file
      .take(515)
      .read_to_string(&mut token)
      .map_err(|_| "Could not read the publishing token file.")?;
    if token.len() > 514 {
      return Err("Publishing token file is too large.".into());
    }
    let token = token.trim_end_matches(['\n', '\r']).to_owned();
    validate_token(&token)?;
    let listen = std::env::var("APELL_STORE_LISTEN")
      .unwrap_or_else(|_| "127.0.0.1:8788".into())
      .parse()
      .map_err(|_| "APELL_STORE_LISTEN must be an IP address and port.")?;
    let account = AccountConfig::from_env()?;
    let oauth = if account.is_none() {
      OAuthConfig::from_env()?
    } else {
      None
    };
    Ok(Self {
      listen,
      data_dir: std::env::var_os("APELL_STORE_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| "store-data".into()),
      web_dir: std::env::var_os("APELL_STORE_WEB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| "store/web/dist".into()),
      publish_token: token,
      oidc: OidcConfig::from_env()?,
      oauth,
      account,
      bootstrap_owner: bootstrap_owner()?,
    })
  }
}

pub(crate) fn validate_token(token: &str) -> std::result::Result<(), String> {
  if !(32..=512).contains(&token.len()) || !token.bytes().all(|byte| byte.is_ascii_graphic()) {
    return Err(
      "Publishing token must contain 32–512 printable ASCII bytes without spaces.".into(),
    );
  }
  Ok(())
}

fn bootstrap_owner() -> Result<Option<User>, String> {
  let Some(path) = std::env::var_os("APELL_STORE_BOOTSTRAP_OWNER_FILE") else {
    return Ok(None);
  };
  let file = std::fs::File::open(path).map_err(|_| "Could not open the bootstrap owner file.")?;
  let metadata = file
    .metadata()
    .map_err(|_| "Could not inspect the bootstrap owner file.")?;
  if !metadata.is_file() || metadata.len() > 1024 {
    return Err("Invalid bootstrap owner file.".into());
  }
  use std::io::Read;
  let mut bytes = Vec::new();
  file
    .take(1025)
    .read_to_end(&mut bytes)
    .map_err(|_| "Could not read the bootstrap owner file.")?;
  if bytes.len() > 1024 {
    return Err("Invalid bootstrap owner file.".into());
  }
  let user: User = serde_json::from_slice(&bytes).map_err(|_| "Invalid bootstrap owner file.")?;
  if user.github_id == 0
    || user.github_id > i64::MAX as u64
    || user.github_login.is_empty()
    || user.github_login.len() > 100
    || !user
      .github_login
      .bytes()
      .all(|c| c.is_ascii_alphanumeric() || c == b'-')
  {
    return Err("Invalid bootstrap owner identity.".into());
  }
  Ok(Some(user))
}
