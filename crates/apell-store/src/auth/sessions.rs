use super::{github::valid_login, hash, random_secret, unauthorized, User};
use crate::{Result, Store, StoreError};
use axum::http::{HeaderMap, StatusCode};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

pub(super) const SESSION_COOKIE: &str = "__Host-apell_session";
pub(super) const FLOW_COOKIE: &str = "__Host-apell_oauth";
pub(super) const SESSION_SECONDS: i64 = 7 * 24 * 60 * 60;

pub(super) struct AuthFlow {
  pub(super) verifier: String,
  pub(super) nonce: Option<String>,
  pub(super) cli_code: Option<String>,
}

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
    );
    CREATE TABLE IF NOT EXISTS auth_account_flows (
      state_hash BLOB PRIMARY KEY REFERENCES auth_flows(state_hash) ON DELETE CASCADE,
      nonce TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS auth_account_links (
      issuer TEXT NOT NULL,
      subject TEXT NOT NULL,
      github_id INTEGER NOT NULL REFERENCES users(github_id),
      PRIMARY KEY (issuer, subject),
      UNIQUE (issuer, github_id)
    );",
    )
    .map_err(|_| StoreError::internal())?;
  super::cli::migrate(&transaction)?;
  transaction.commit().map_err(|_| StoreError::internal())
}

fn save_user(connection: &Connection, user: &User) -> Result<()> {
  if user.github_id == 0 || user.github_id > i64::MAX as u64 || !valid_login(&user.github_login) {
    return Err(StoreError::invalid("Invalid GitHub user."));
  }
  connection
    .execute(
      "INSERT INTO users (github_id, github_login) VALUES (?1, ?2)
       ON CONFLICT(github_id) DO UPDATE SET github_login=excluded.github_login",
      params![user.github_id as i64, user.github_login],
    )
    .map_err(|_| StoreError::internal())?;
  Ok(())
}

impl Store {
  pub fn upsert_user(&self, user: &User) -> Result<()> {
    save_user(&self.connection()?, user)
  }

  pub(super) fn bind_account(&self, issuer: &str, subject: &str, user: &User) -> Result<()> {
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    save_user(&transaction, user)?;
    transaction
      .execute(
        "INSERT INTO auth_account_links (issuer, subject, github_id) VALUES (?1, ?2, ?3)
         ON CONFLICT DO NOTHING",
        params![issuer, subject, user.github_id as i64],
      )
      .map_err(|_| StoreError::internal())?;
    let existing: Option<i64> = transaction
      .query_row(
        "SELECT github_id FROM auth_account_links WHERE issuer=?1 AND subject=?2",
        params![issuer, subject],
        |row| row.get(0),
      )
      .optional()
      .map_err(|_| StoreError::internal())?;
    if existing != Some(user.github_id as i64) {
      return Err(StoreError::new(
        StatusCode::CONFLICT,
        "account_link_conflict",
        "This GitHub identity conflicts with an existing Apell Account link.",
      ));
    }
    transaction.commit().map_err(|_| StoreError::internal())
  }

  pub(super) fn account_user(&self, issuer: &str, subject: &str) -> Result<Option<User>> {
    self
      .connection()?
      .query_row(
        "SELECT users.github_id, users.github_login FROM auth_account_links JOIN users USING(github_id)
         WHERE issuer=?1 AND subject=?2",
        params![issuer, subject],
        |row| {
          Ok(User {
            github_id: row.get(0)?,
            github_login: row.get(1)?,
          })
        },
      )
      .optional()
      .map_err(|_| StoreError::internal())
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
    self.create_auth_flow(state, browser, verifier, None, None, now)
  }

  #[cfg(test)]
  pub(super) fn create_flow_for_cli(
    &self,
    state: &str,
    browser: &str,
    verifier: &str,
    cli_code: Option<&str>,
    now: i64,
  ) -> Result<()> {
    self.create_auth_flow(state, browser, verifier, None, cli_code, now)
  }

  pub(super) fn create_auth_flow(
    &self,
    state: &str,
    browser: &str,
    verifier: &str,
    nonce: Option<&str>,
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
    if let Some(nonce) = nonce {
      transaction
        .execute(
          "INSERT INTO auth_account_flows(state_hash,nonce) VALUES (?1,?2)",
          params![hash(state), nonce],
        )
        .map_err(|_| StoreError::internal())?;
    }
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
    Ok(self.consume_auth_flow(state, browser, false, now)?.verifier)
  }

  #[cfg(test)]
  pub(super) fn consume_flow_for_cli(
    &self,
    state: &str,
    browser: &str,
    now: i64,
  ) -> Result<(String, Option<String>)> {
    let flow = self.consume_auth_flow(state, browser, false, now)?;
    Ok((flow.verifier, flow.cli_code))
  }

  pub(super) fn consume_auth_flow(
    &self,
    state: &str,
    browser: &str,
    expect_account: bool,
    now: i64,
  ) -> Result<AuthFlow> {
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    let flow = transaction
      .query_row(
        "SELECT verifier,nonce,user_code FROM auth_flows
         LEFT JOIN auth_account_flows USING(state_hash)
         LEFT JOIN auth_cli_resumes USING(state_hash)
         WHERE state_hash=?1 AND browser_hash=?2 AND expires_at>?3 AND (nonce IS NOT NULL)=?4",
        params![hash(state), hash(browser), now, expect_account],
        |row| {
          Ok(AuthFlow {
            verifier: row.get(0)?,
            nonce: row.get(1)?,
            cli_code: row.get(2)?,
          })
        },
      )
      .optional()
      .map_err(|_| StoreError::internal())?
      .ok_or_else(unauthorized)?;
    let cli_code = flow
      .cli_code
      .as_deref()
      .map(super::cli::user_code)
      .transpose()?;
    transaction
      .execute("DELETE FROM auth_flows WHERE state_hash=?1", [hash(state)])
      .map_err(|_| StoreError::internal())?;
    transaction.commit().map_err(|_| StoreError::internal())?;
    Ok(AuthFlow { cli_code, ..flow })
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn account_binding_is_idempotent_and_conflicts_leave_existing_users_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path()).unwrap();
    let issuer = "https://account.example.invalid/oidc";
    let original = User {
      github_id: 42,
      github_login: "original-login".into(),
    };
    store.upsert_user(&original).unwrap();
    store
      .bind_account(issuer, "account-one", &original)
      .unwrap();
    store
      .bind_account(issuer, "account-one", &original)
      .unwrap();

    let renamed = User {
      github_login: "renamed-login".into(),
      ..original.clone()
    };
    assert_eq!(
      store
        .bind_account(issuer, "account-two", &renamed)
        .unwrap_err()
        .code,
      "account_link_conflict"
    );
    assert_eq!(
      store.user_by_id(original.github_id).unwrap(),
      Some(original.clone())
    );
    assert_eq!(
      store.account_user(issuer, "account-one").unwrap(),
      Some(original)
    );
    assert_eq!(store.account_user(issuer, "account-two").unwrap(), None);
    assert_eq!(
      store
        .account_user("https://other.example.invalid/oidc", "account-one")
        .unwrap(),
      None
    );
    let other_user = User {
      github_id: 43,
      github_login: "other-login".into(),
    };
    assert_eq!(
      store
        .bind_account(issuer, "account-one", &other_user)
        .unwrap_err()
        .code,
      "account_link_conflict"
    );
    assert_eq!(store.user_by_id(other_user.github_id).unwrap(), None);

    store.bind_account(issuer, "account-one", &renamed).unwrap();
    assert_eq!(store.user_by_id(renamed.github_id).unwrap(), Some(renamed));
  }

  #[test]
  fn account_flow_is_bound_to_provider_and_browser_and_consumes_nonce_once() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path()).unwrap();
    let cli = store.start_cli_authorization(1000).unwrap();
    store
      .create_auth_flow(
        "state",
        "browser",
        "verifier",
        Some("nonce"),
        Some(&cli.user_code),
        1000,
      )
      .unwrap();
    assert!(store
      .consume_auth_flow("state", "browser", false, 1001)
      .is_err());
    assert!(store
      .consume_auth_flow("state", "other-browser", true, 1001)
      .is_err());
    let flow = store
      .consume_auth_flow("state", "browser", true, 1001)
      .unwrap();
    assert_eq!(flow.verifier, "verifier");
    assert_eq!(flow.nonce.as_deref(), Some("nonce"));
    assert_eq!(flow.cli_code, Some(cli.user_code));
    assert!(store
      .consume_auth_flow("state", "browser", true, 1001)
      .is_err());

    store
      .create_auth_flow("state", "browser", "verifier", None, None, 1002)
      .unwrap();
    assert!(store
      .consume_auth_flow("state", "browser", true, 1003)
      .is_err());
    let legacy = store
      .consume_auth_flow("state", "browser", false, 1003)
      .unwrap();
    assert_eq!(legacy.verifier, "verifier");
    assert!(legacy.nonce.is_none());
  }
}
