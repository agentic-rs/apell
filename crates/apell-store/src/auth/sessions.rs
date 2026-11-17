use super::{github::valid_login, hash, random_secret, unauthorized, User};
use crate::{Result, Store, StoreError};
use axum::http::HeaderMap;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

pub(super) const SESSION_COOKIE: &str = "__Host-apell_session";
pub(super) const FLOW_COOKIE: &str = "__Host-apell_oauth";
pub(super) const SESSION_SECONDS: i64 = 7 * 24 * 60 * 60;

pub(crate) fn migrate(connection: &Connection) -> Result<()> {
  let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
    .map_err(|_| StoreError::internal())?;
  transaction
    .execute_batch(
      "CREATE TABLE IF NOT EXISTS users (
      github_id INTEGER PRIMARY KEY CHECK(github_id > 0),
      github_login TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS auth_sessions (
      token_hash BLOB PRIMARY KEY,
      github_id INTEGER NOT NULL REFERENCES users(github_id),
      expires_at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS auth_session_users ON auth_sessions(github_id);
    CREATE INDEX IF NOT EXISTS auth_session_expiry ON auth_sessions(expires_at);
    CREATE TABLE IF NOT EXISTS auth_flows (
      state_hash BLOB PRIMARY KEY,
      browser_hash BLOB NOT NULL,
      verifier TEXT NOT NULL,
      expires_at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS auth_flow_expiry ON auth_flows(expires_at);
    CREATE TABLE IF NOT EXISTS auth_cli_resumes (
      state_hash BLOB PRIMARY KEY REFERENCES auth_flows(state_hash) ON DELETE CASCADE,
      user_code TEXT NOT NULL
    );",
    )
    .map_err(|_| StoreError::internal())?;
  super::cli::migrate(&transaction)?;
  transaction.commit().map_err(|_| StoreError::internal())
}

impl Store {
  pub fn upsert_user(&self, user: &User) -> Result<()> {
    if user.github_id == 0 || user.github_id > i64::MAX as u64 || !valid_login(&user.github_login) {
      return Err(StoreError::invalid("Invalid GitHub user."));
    }
    self
      .connection()?
      .execute(
        "INSERT INTO users (github_id, github_login) VALUES (?1, ?2)
       ON CONFLICT(github_id) DO UPDATE SET github_login=excluded.github_login",
        params![user.github_id as i64, user.github_login],
      )
      .map_err(|_| StoreError::internal())?;
    Ok(())
  }

  pub fn user_by_id(&self, github_id: u64) -> Result<Option<User>> {
    if github_id == 0 || github_id > i64::MAX as u64 {
      return Ok(None);
    }
    self
      .connection()?
      .query_row(
        "SELECT github_id, github_login FROM users WHERE github_id=?1",
        [github_id as i64],
        |row| {
          Ok(User {
            github_id: row.get::<_, i64>(0)? as u64,
            github_login: row.get(1)?,
          })
        },
      )
      .optional()
      .map_err(|_| StoreError::internal())
  }

  pub(super) fn session_user(&self, secret: &str, now: i64) -> Result<User> {
    self
      .connection()?
      .query_row(
        "SELECT users.github_id, users.github_login FROM auth_sessions JOIN users USING(github_id)
       WHERE token_hash=?1 AND expires_at>?2",
        params![hash(secret), now],
        |row| {
          Ok(User {
            github_id: row.get::<_, i64>(0)? as u64,
            github_login: row.get(1)?,
          })
        },
      )
      .optional()
      .map_err(|_| StoreError::internal())?
      .ok_or_else(unauthorized)
  }

  pub(super) fn create_session(
    &self,
    user: &User,
    old_secret: Option<&str>,
    now: i64,
  ) -> Result<String> {
    let secret = random_secret()?;
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    transaction
      .execute("DELETE FROM auth_sessions WHERE expires_at<=?1", [now])
      .map_err(|_| StoreError::internal())?;
    if let Some(old) = old_secret {
      transaction
        .execute("DELETE FROM auth_sessions WHERE token_hash=?1", [hash(old)])
        .map_err(|_| StoreError::internal())?;
    }
    let count: i64 = transaction
      .query_row("SELECT COUNT(*) FROM auth_sessions", [], |row| row.get(0))
      .map_err(|_| StoreError::internal())?;
    if count >= 4096 {
      return Err(StoreError::busy());
    }
    transaction.execute(
      "DELETE FROM auth_sessions WHERE token_hash IN
       (SELECT token_hash FROM auth_sessions WHERE github_id=?1 ORDER BY expires_at DESC LIMIT -1 OFFSET 15)",
      [user.github_id as i64],
    ).map_err(|_| StoreError::internal())?;
    transaction
      .execute(
        "INSERT INTO auth_sessions VALUES (?1,?2,?3)",
        params![hash(&secret), user.github_id as i64, now + SESSION_SECONDS],
      )
      .map_err(|_| StoreError::internal())?;
    transaction.commit().map_err(|_| StoreError::internal())?;
    Ok(secret)
  }

  pub(super) fn delete_session(&self, secret: &str) -> Result<()> {
    self
      .connection()?
      .execute(
        "DELETE FROM auth_sessions WHERE token_hash=?1",
        [hash(secret)],
      )
      .map_err(|_| StoreError::internal())?;
    Ok(())
  }

  #[cfg(test)]
  pub(super) fn create_flow(
    &self,
    state: &str,
    browser: &str,
    verifier: &str,
    now: i64,
  ) -> Result<()> {
    self.create_flow_for_cli(state, browser, verifier, None, now)
  }

  pub(super) fn create_flow_for_cli(
    &self,
    state: &str,
    browser: &str,
    verifier: &str,
    cli_code: Option<&str>,
    now: i64,
  ) -> Result<()> {
    let cli_code = cli_code.map(super::cli::user_code).transpose()?;
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    transaction
      .execute("DELETE FROM auth_flows WHERE expires_at<=?1", [now])
      .map_err(|_| StoreError::internal())?;
    let count: i64 = transaction
      .query_row("SELECT COUNT(*) FROM auth_flows", [], |row| row.get(0))
      .map_err(|_| StoreError::internal())?;
    if count >= 256 {
      return Err(StoreError::busy());
    }
    transaction
      .execute(
        "INSERT INTO auth_flows(state_hash,browser_hash,verifier,expires_at) VALUES (?1,?2,?3,?4)",
        params![hash(state), hash(browser), verifier, now + 600],
      )
      .map_err(|_| StoreError::internal())?;
    if let Some(cli_code) = cli_code {
      transaction
        .execute(
          "INSERT INTO auth_cli_resumes(state_hash,user_code) VALUES (?1,?2)",
          params![hash(state), cli_code],
        )
        .map_err(|_| StoreError::internal())?;
    }
    transaction.commit().map_err(|_| StoreError::internal())
  }

  #[cfg(test)]
  pub(super) fn consume_flow(&self, state: &str, browser: &str, now: i64) -> Result<String> {
    Ok(self.consume_flow_for_cli(state, browser, now)?.0)
  }

  pub(super) fn consume_flow_for_cli(
    &self,
    state: &str,
    browser: &str,
    now: i64,
  ) -> Result<(String, Option<String>)> {
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    let verifier: Option<(String, Option<String>)> = transaction
      .query_row(
        "SELECT verifier,user_code FROM auth_flows LEFT JOIN auth_cli_resumes USING(state_hash)
         WHERE state_hash=?1 AND browser_hash=?2 AND expires_at>?3",
        params![hash(state), hash(browser), now],
        |row| Ok((row.get(0)?, row.get(1)?)),
      )
      .optional()
      .map_err(|_| StoreError::internal())?;
    let verifier = verifier.ok_or_else(unauthorized)?;
    let cli_code = verifier
      .1
      .as_deref()
      .map(super::cli::user_code)
      .transpose()?;
    transaction
      .execute("DELETE FROM auth_flows WHERE state_hash=?1", [hash(state)])
      .map_err(|_| StoreError::internal())?;
    transaction.commit().map_err(|_| StoreError::internal())?;
    Ok((verifier.0, cli_code))
  }
}

pub(super) fn cookie(headers: &HeaderMap, name: &str) -> Result<Option<String>> {
  let mut result = None;
  let mut length = 0;
  for header in headers.get_all("cookie") {
    let value = header.to_str().map_err(|_| unauthorized())?;
    length += value.len();
    if length > 8192 {
      return Err(unauthorized());
    }
    for item in value.split(';') {
      let Some((key, value)) = item.trim().split_once('=') else {
        continue;
      };
      if key != name {
        continue;
      }
      if result.is_some()
        || value.len() != 64
        || !value
          .bytes()
          .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
      {
        return Err(unauthorized());
      }
      result = Some(value.to_owned());
    }
  }
  Ok(result)
}
