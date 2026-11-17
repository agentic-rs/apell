use super::{
  denied, expired, random_code, slow_down, user_code, valid_secret, Authorized, CredentialDetails,
  Identity, Polled, RequestDetails, Started, CREDENTIAL_SECONDS, FLOW_SECONDS, INTERVAL,
};
use crate::{
  auth::{hash, random_secret, unauthorized, User},
  Result, Store, StoreError,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

pub(in crate::auth) fn migrate(connection: &Connection) -> Result<()> {
  connection
    .execute_batch(
      "CREATE TABLE IF NOT EXISTS cli_authorizations (
      device_hash BLOB PRIMARY KEY,
      user_code TEXT NOT NULL UNIQUE,
      status TEXT NOT NULL CHECK(status IN ('pending','approved','denied')),
      github_id INTEGER REFERENCES users(github_id),
      expires_at INTEGER NOT NULL,
      poll_after INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS cli_authorization_expiry ON cli_authorizations(expires_at);
    CREATE TABLE IF NOT EXISTS cli_credentials (
      id TEXT PRIMARY KEY,
      token_hash BLOB NOT NULL UNIQUE,
      github_id INTEGER NOT NULL REFERENCES users(github_id),
      created_at INTEGER NOT NULL,
      expires_at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS cli_credential_users ON cli_credentials(github_id);
    CREATE INDEX IF NOT EXISTS cli_credential_expiry ON cli_credentials(expires_at);",
    )
    .map_err(|_| StoreError::internal())
}

impl Store {
  pub(in crate::auth) fn start_cli_authorization(&self, now: i64) -> Result<Started> {
    let device_code = random_secret()?;
    let user_code = random_code()?;
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    transaction
      .execute("DELETE FROM cli_authorizations WHERE expires_at<=?1", [now])
      .map_err(|_| StoreError::internal())?;
    let count: i64 = transaction
      .query_row("SELECT COUNT(*) FROM cli_authorizations", [], |row| {
        row.get(0)
      })
      .map_err(|_| StoreError::internal())?;
    if count >= 256 {
      return Err(StoreError::busy());
    }
    transaction.execute(
      "INSERT INTO cli_authorizations(device_hash,user_code,status,expires_at,poll_after) VALUES(?1,?2,'pending',?3,?4)",
      params![hash(&device_code),user_code,now+FLOW_SECONDS,now],
    ).map_err(|_| StoreError::internal())?;
    transaction.commit().map_err(|_| StoreError::internal())?;
    Ok(Started {
      device_code,
      user_code,
      expires_at: now + FLOW_SECONDS,
      interval: INTERVAL,
    })
  }

  pub(in crate::auth) fn cli_authorization(&self, code: &str, now: i64) -> Result<RequestDetails> {
    let code = user_code(code)?;
    self.connection()?.query_row(
      "SELECT user_code,expires_at,status FROM cli_authorizations WHERE user_code=?1 AND expires_at>?2",
      params![code,now], |row| Ok(RequestDetails { user_code: row.get(0)?, expires_at: row.get(1)?, status: row.get(2)? }),
    ).optional().map_err(|_| StoreError::internal())?.ok_or_else(expired)
  }

  pub(in crate::auth) fn decide_cli_authorization(
    &self,
    code: &str,
    actor_id: u64,
    approve: bool,
    now: i64,
  ) -> Result<()> {
    let code = user_code(code)?;
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    let user_exists: bool = transaction
      .query_row(
        "SELECT EXISTS(SELECT 1 FROM users WHERE github_id=?1)",
        [actor_id],
        |row| row.get(0),
      )
      .map_err(|_| StoreError::internal())?;
    if !user_exists {
      return Err(unauthorized());
    }
    let changed = transaction.execute(
      "UPDATE cli_authorizations SET status=?2,github_id=?3 WHERE user_code=?1 AND status='pending' AND expires_at>?4",
      params![code,if approve {"approved"} else {"denied"},if approve {Some(actor_id)} else {None},now],
    ).map_err(|_| StoreError::internal())?;
    if changed != 1 {
      return Err(expired());
    }
    transaction.commit().map_err(|_| StoreError::internal())
  }

  pub(in crate::auth) fn poll_cli_authorization(
    &self,
    device_code: &str,
    now: i64,
  ) -> Result<Polled> {
    if !valid_secret(device_code) {
      return Err(expired());
    }
    let device_hash = hash(device_code);
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    let entry: Option<(String, Option<u64>, i64)> = transaction.query_row(
      "SELECT status,github_id,poll_after FROM cli_authorizations WHERE device_hash=?1 AND expires_at>?2",
      params![device_hash,now], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    ).optional().map_err(|_| StoreError::internal())?;
    let (status, github_id, poll_after) = entry.ok_or_else(expired)?;
    if poll_after > now {
      return Err(slow_down());
    }
    match status.as_str() {
      "pending" => {
        transaction
          .execute(
            "UPDATE cli_authorizations SET poll_after=?2 WHERE device_hash=?1",
            params![device_hash, now + INTERVAL],
          )
          .map_err(|_| StoreError::internal())?;
        transaction.commit().map_err(|_| StoreError::internal())?;
        Ok(Polled::Pending)
      }
      "denied" => Err(denied()),
      "approved" => {
        let github_id = github_id.ok_or_else(StoreError::internal)?;
        let user = transaction
          .query_row(
            "SELECT github_id,github_login FROM users WHERE github_id=?1",
            [github_id],
            |row| {
              Ok(User {
                github_id: row.get(0)?,
                github_login: row.get(1)?,
              })
            },
          )
          .map_err(|_| StoreError::internal())?;
        transaction
          .execute("DELETE FROM cli_credentials WHERE expires_at<=?1", [now])
          .map_err(|_| StoreError::internal())?;
        let (total, personal): (i64, i64) = transaction
          .query_row(
            "SELECT COUNT(*),COUNT(CASE WHEN github_id=?1 THEN 1 END) FROM cli_credentials",
            [github_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
          )
          .map_err(|_| StoreError::internal())?;
        if total >= 4096 || personal >= 16 {
          return Err(StoreError::busy());
        }
        let token = format!("apel_user_{}", random_secret()?);
        let id = random_secret()?;
        let expires_at = now + CREDENTIAL_SECONDS;
        transaction.execute("INSERT INTO cli_credentials(id,token_hash,github_id,created_at,expires_at) VALUES(?1,?2,?3,?4,?5)", params![id,hash(&token),github_id,now,expires_at]).map_err(|_| StoreError::internal())?;
        transaction
          .execute(
            "DELETE FROM cli_authorizations WHERE device_hash=?1",
            [device_hash],
          )
          .map_err(|_| StoreError::internal())?;
        transaction.commit().map_err(|_| StoreError::internal())?;
        Ok(Polled::Authorized(Authorized {
          token,
          user,
          expires_at,
        }))
      }
      _ => Err(StoreError::internal()),
    }
  }

  pub(in crate::auth) fn cli_identity(&self, token: &str, now: i64) -> Result<Identity> {
    self.connection()?.query_row(
      "SELECT u.github_id,u.github_login,c.expires_at FROM cli_credentials c JOIN users u USING(github_id) WHERE c.token_hash=?1 AND c.expires_at>?2",
      params![hash(token),now], |row| Ok(Identity { user:User {github_id:row.get(0)?,github_login:row.get(1)?},expires_at:row.get(2)? }),
    ).optional().map_err(|_| StoreError::internal())?.ok_or_else(unauthorized)
  }

  pub(in crate::auth) fn cli_credentials(
    &self,
    user_id: u64,
    now: i64,
  ) -> Result<Vec<CredentialDetails>> {
    let connection = self.connection()?;
    let mut statement = connection.prepare("SELECT id,created_at,expires_at FROM cli_credentials WHERE github_id=?1 AND expires_at>?2 ORDER BY created_at DESC,id").map_err(|_| StoreError::internal())?;
    let result = statement
      .query_map(params![user_id, now], |row| {
        Ok(CredentialDetails {
          id: row.get(0)?,
          created_at: row.get(1)?,
          expires_at: row.get(2)?,
        })
      })
      .map_err(|_| StoreError::internal())?
      .collect::<std::result::Result<Vec<_>, _>>()
      .map_err(|_| StoreError::internal());
    result
  }

  pub(in crate::auth) fn revoke_cli_credential(&self, user_id: u64, id: &str) -> Result<()> {
    if !valid_secret(id) {
      return Err(StoreError::missing());
    }
    let changed = self
      .connection()?
      .execute(
        "DELETE FROM cli_credentials WHERE id=?1 AND github_id=?2",
        params![id, user_id],
      )
      .map_err(|_| StoreError::internal())?;
    if changed == 0 {
      return Err(StoreError::missing());
    }
    Ok(())
  }

  pub(in crate::auth) fn logout_cli(&self, token: &str) -> Result<()> {
    self
      .connection()?
      .execute(
        "DELETE FROM cli_credentials WHERE token_hash=?1",
        [hash(token)],
      )
      .map_err(|_| StoreError::internal())?;
    Ok(())
  }
}
