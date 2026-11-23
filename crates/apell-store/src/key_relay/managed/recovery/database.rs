use super::{expired, storage_error, Key, Purpose};
use crate::{
  key_relay::{busy, conflict, managed::database as managed, Owner},
  Result, StoreError,
};
use axum::http::StatusCode;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use subtle::ConstantTimeEq;

const MAX_PENDING_PER_ACCOUNT: i64 = 8;
const MAX_PENDING_TOTAL: i64 = 2048;

pub(super) fn migrate(db: &Connection) -> Result<()> {
  db.execute_batch(
    "CREATE TABLE IF NOT EXISTS key_recovery_requests (
       recovery_id TEXT PRIMARY KEY NOT NULL,
       issuer TEXT NOT NULL,
       subject TEXT NOT NULL,
       vault_id TEXT NOT NULL,
       purpose TEXT NOT NULL CHECK (purpose IN ('recover', 'enroll')),
       request_hash BLOB NOT NULL CHECK (length(request_hash) = 32),
       state_hash BLOB UNIQUE CHECK (length(state_hash) = 32),
       started_at INTEGER NOT NULL,
       expires_at INTEGER NOT NULL CHECK (expires_at > started_at),
       status TEXT NOT NULL CHECK (status IN ('pending', 'verified', 'rejected')),
       CHECK (status = 'pending' OR state_hash IS NULL)
     );
     CREATE INDEX IF NOT EXISTS key_recovery_expiry ON key_recovery_requests (expires_at);
     CREATE INDEX IF NOT EXISTS key_recovery_owner ON key_recovery_requests (issuer, subject);",
  )
  .map_err(|_| storage_error())
}

pub(super) fn prune(db: &Connection, time: i64) -> Result<()> {
  db.execute(
    "DELETE FROM key_recovery_requests WHERE expires_at <= ?1",
    [time],
  )
  .map(|_| ())
  .map_err(|_| storage_error())
}

pub(super) struct NewRequest<'a> {
  pub id: &'a str,
  pub vault_id: &'a str,
  pub purpose: Purpose,
  pub request_hash: &'a [u8],
  pub state_hash: &'a [u8],
  pub started_at: i64,
  pub expires_at: i64,
}

pub(super) fn start(db: &mut Connection, owner: &Owner, request: NewRequest<'_>) -> Result<()> {
  let tx = db
    .transaction_with_behavior(TransactionBehavior::Immediate)
    .map_err(|_| storage_error())?;
  prune(&tx, request.started_at)?;
  let eligible: bool = tx
    .query_row(
      "SELECT
        CASE WHEN ?4 = 'recover' THEN EXISTS (
          SELECT 1 FROM managed_key_vaults WHERE issuer = ?1 AND subject = ?2 AND vault_id = ?3
        ) ELSE EXISTS (
          SELECT 1 FROM key_vaults WHERE issuer = ?1 AND subject = ?2 AND vault_id = ?3
        ) AND NOT EXISTS (
          SELECT 1 FROM managed_key_vaults WHERE issuer = ?1 AND subject = ?2
        ) END",
      params![
        owner.issuer,
        owner.subject,
        request.vault_id,
        request.purpose.as_str()
      ],
      |row| row.get(0),
    )
    .map_err(|_| storage_error())?;
  if !eligible {
    return Err(conflict(
      "The Account key vault is unavailable for this operation.",
    ));
  }
  let (total, owned): (i64, i64) = tx
    .query_row(
      "SELECT (SELECT count(*) FROM key_recovery_requests), (
         SELECT count(*) FROM key_recovery_requests WHERE issuer = ?1 AND subject = ?2
       )",
      params![owner.issuer, owner.subject],
      |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .map_err(|_| storage_error())?;
  if total >= MAX_PENDING_TOTAL || owned >= MAX_PENDING_PER_ACCOUNT {
    return Err(busy());
  }
  tx.execute(
    "INSERT INTO key_recovery_requests (
       recovery_id, issuer, subject, vault_id, purpose, request_hash, state_hash,
       started_at, expires_at, status
     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending')",
    params![
      request.id,
      owner.issuer,
      owner.subject,
      request.vault_id,
      request.purpose.as_str(),
      request.request_hash,
      request.state_hash,
      request.started_at,
      request.expires_at,
    ],
  )
  .map_err(|_| storage_error())?;
  tx.commit().map_err(|_| storage_error())
}

pub(super) struct BrowserChallenge {
  pub id: String,
  pub subject: String,
  pub started_at: i64,
}

pub(super) fn take_browser_state(
  db: &mut Connection,
  issuer: &str,
  state_hash: &[u8],
  time: i64,
) -> Result<BrowserChallenge> {
  let tx = db
    .transaction_with_behavior(TransactionBehavior::Immediate)
    .map_err(|_| storage_error())?;
  let challenge = tx
    .query_row(
      "SELECT recovery_id, subject, started_at FROM key_recovery_requests
       WHERE issuer = ?1 AND state_hash = ?2 AND expires_at > ?3 AND status = 'pending'",
      params![issuer, state_hash, time],
      |row| {
        Ok(BrowserChallenge {
          id: row.get(0)?,
          subject: row.get(1)?,
          started_at: row.get(2)?,
        })
      },
    )
    .optional()
    .map_err(|_| storage_error())?
    .ok_or_else(expired)?;
  tx.execute(
    "UPDATE key_recovery_requests SET state_hash = NULL
     WHERE recovery_id = ?1",
    [&challenge.id],
  )
  .map_err(|_| storage_error())?;
  tx.commit().map_err(|_| storage_error())?;
  Ok(challenge)
}

pub(super) fn mark_verified(db: &Connection, id: &str, verified: bool, time: i64) -> Result<()> {
  let status = if verified { "verified" } else { "rejected" };
  let updated = db
    .execute(
      "UPDATE key_recovery_requests SET status = ?2
       WHERE recovery_id = ?1 AND expires_at > ?3 AND status = 'pending' AND state_hash IS NULL",
      params![id, status, time],
    )
    .map_err(|_| storage_error())?;
  if updated != 1 {
    return Err(expired());
  }
  Ok(())
}

pub(super) enum Stage {
  Pending,
  Verified,
  Complete(Option<Key>),
}

pub(super) fn finish(
  db: &mut Connection,
  master: &Key,
  owner: &Owner,
  id: &str,
  request_hash: &[u8],
  vault_key: Option<&Key>,
  time: i64,
) -> Result<Stage> {
  let tx = db
    .transaction_with_behavior(TransactionBehavior::Immediate)
    .map_err(|_| storage_error())?;
  let claim = tx
    .query_row(
      "SELECT request_hash, status, purpose, vault_id FROM key_recovery_requests
       WHERE recovery_id = ?1 AND issuer = ?2 AND subject = ?3 AND expires_at > ?4",
      params![id, owner.issuer, owner.subject, time],
      |row| {
        Ok((
          row.get::<_, Vec<u8>>(0)?,
          row.get::<_, String>(1)?,
          row.get::<_, String>(2)?,
          row.get::<_, String>(3)?,
        ))
      },
    )
    .optional()
    .map_err(|_| storage_error())?;
  let Some((_, status, purpose, vault_id)) =
    claim.filter(|(hash, _, _, _)| bool::from(hash.ct_eq(request_hash)))
  else {
    return Err(expired());
  };
  let key = match (status.as_str(), purpose.as_str(), vault_key) {
    ("pending", _, None) => return Ok(Stage::Pending),
    ("verified", "enroll", None) => return Ok(Stage::Verified),
    ("verified", "recover", None) => Some(managed::load_key(&tx, master, owner, &vault_id)?),
    ("verified", "enroll", Some(key)) => {
      managed::create_in_transaction(&tx, master, owner, &vault_id, key, true)?;
      None
    }
    ("rejected", _, _) => {
      return Err(StoreError::new(
        StatusCode::FORBIDDEN,
        "key_recovery_verification_failed",
        "Account verification could not be completed. Start again in Apell.",
      ))
    }
    _ => return Err(StoreError::invalid("Invalid Account recovery request.")),
  };
  // Commit the operation and grant consumption together. Racing finish calls
  // can deliver one result, and a failed escrow operation stays retryable.
  tx.execute(
    "DELETE FROM key_recovery_requests WHERE recovery_id = ?1",
    [id],
  )
  .map_err(|_| storage_error())?;
  tx.commit().map_err(|_| storage_error())?;
  Ok(Stage::Complete(key))
}

#[cfg(test)]
mod tests {
  use super::*;
  use zeroize::Zeroizing;

  const VAULT: &str = "12345678-1234-4567-8901-123456789012";
  const MASTER: [u8; 32] = [3; 32];
  const VAULT_KEY: [u8; 32] = [7; 32];
  const REQUEST_HASH: [u8; 32] = [1; 32];
  const STATE_HASH: [u8; 32] = [2; 32];
  const START: i64 = 1000;
  const EXPIRY: i64 = 1300;

  fn fixture(purpose: Purpose) -> (tempfile::TempDir, Connection, Owner) {
    let directory = tempfile::tempdir().unwrap();
    let mut db = crate::key_relay::database::open(directory.path()).unwrap();
    migrate(&db).unwrap();
    let owner = Owner {
      issuer: "fixture-issuer".into(),
      subject: "fixture-subject".into(),
    };
    match purpose {
      Purpose::Recover => managed::create(
        &mut db,
        &Zeroizing::new(MASTER),
        &owner,
        VAULT,
        &Zeroizing::new(VAULT_KEY),
        false,
      )
      .unwrap(),
      Purpose::Enroll => {
        db.execute(
          "INSERT INTO key_vaults VALUES (?1, ?2, ?3, 'test-nonce', 'test-ciphertext')",
          params![owner.issuer, owner.subject, VAULT],
        )
        .unwrap();
      }
    }
    start(
      &mut db,
      &owner,
      NewRequest {
        id: "fixture-recovery",
        vault_id: VAULT,
        purpose,
        request_hash: &REQUEST_HASH,
        state_hash: &STATE_HASH,
        started_at: START,
        expires_at: EXPIRY,
      },
    )
    .unwrap();
    (directory, db, owner)
  }

  fn finish_request(
    db: &mut Connection,
    owner: &Owner,
    id: &str,
    hash: &[u8],
    key: Option<&Key>,
    time: i64,
  ) -> Result<Stage> {
    finish(db, &Zeroizing::new(MASTER), owner, id, hash, key, time)
  }

  #[test]
  fn recovery_binds_browser_state_and_one_delivery_to_the_original_requester() {
    let (_directory, mut db, owner) = fixture(Purpose::Recover);
    let id = "fixture-recovery";
    assert!(mark_verified(&db, id, true, START).is_err());
    assert!(matches!(
      finish_request(&mut db, &owner, id, &REQUEST_HASH, None, START),
      Ok(Stage::Pending)
    ));
    assert!(take_browser_state(&mut db, &owner.issuer, &REQUEST_HASH, START).is_err());
    assert!(take_browser_state(&mut db, "other-issuer", &STATE_HASH, START).is_err());
    let browser = take_browser_state(&mut db, &owner.issuer, &STATE_HASH, START).unwrap();
    assert_eq!(browser.id, id);
    assert_eq!(browser.subject, owner.subject);
    assert_eq!(browser.started_at, START);
    assert!(take_browser_state(&mut db, &owner.issuer, &STATE_HASH, START).is_err());
    mark_verified(&db, id, true, START + 1).unwrap();
    assert!(finish_request(&mut db, &owner, id, &STATE_HASH, None, START + 1).is_err());
    let other = Owner {
      subject: "unrelated-subject".into(),
      ..owner.clone()
    };
    assert!(finish_request(&mut db, &other, id, &REQUEST_HASH, None, START + 1).is_err());
    // A failed escrow decrypt leaves the verified request usable.
    assert!(finish(
      &mut db,
      &Zeroizing::new([4; 32]),
      &owner,
      id,
      &REQUEST_HASH,
      None,
      START + 1,
    )
    .is_err());
    assert!(matches!(
      finish_request(&mut db, &owner, id, &REQUEST_HASH, None, START + 1),
      Ok(Stage::Complete(Some(key))) if *key == VAULT_KEY
    ));
    assert!(finish_request(&mut db, &owner, id, &REQUEST_HASH, None, START + 1).is_err());
  }

  #[test]
  fn enrollment_accepts_a_vault_key_only_after_browser_approval() {
    let (_directory, mut db, owner) = fixture(Purpose::Enroll);
    let id = "fixture-recovery";
    let key = Zeroizing::new(VAULT_KEY);
    assert!(finish_request(&mut db, &owner, id, &REQUEST_HASH, Some(&key), START).is_err());
    take_browser_state(&mut db, &owner.issuer, &STATE_HASH, START).unwrap();
    mark_verified(&db, id, true, START + 1).unwrap();
    assert!(matches!(
      finish_request(&mut db, &owner, id, &REQUEST_HASH, None, START + 1),
      Ok(Stage::Verified)
    ));
    assert!(matches!(
      finish_request(&mut db, &owner, id, &REQUEST_HASH, Some(&key), START + 1),
      Ok(Stage::Complete(None))
    ));
    let original = crate::key_relay::database::vault(&db, &owner)
      .unwrap()
      .unwrap();
    assert_eq!(original.vault_id, VAULT);
    assert_eq!(original.recovery.ciphertext, "test-ciphertext");
    assert_eq!(
      *managed::load_key(&db, &Zeroizing::new(MASTER), &owner, VAULT).unwrap(),
      VAULT_KEY,
    );
    assert!(finish_request(&mut db, &owner, id, &REQUEST_HASH, Some(&key), START + 1).is_err());
  }

  #[test]
  fn rejected_and_expired_challenges_never_authorize_delivery() {
    let (_directory, mut db, owner) = fixture(Purpose::Recover);
    let id = "fixture-recovery";
    assert!(take_browser_state(&mut db, &owner.issuer, &STATE_HASH, EXPIRY).is_err());
    take_browser_state(&mut db, &owner.issuer, &STATE_HASH, START).unwrap();
    assert!(mark_verified(&db, id, true, EXPIRY).is_err());
    mark_verified(&db, id, false, START + 1).unwrap();
    assert!(finish_request(&mut db, &owner, id, &REQUEST_HASH, None, START + 1).is_err());
    prune(&db, EXPIRY).unwrap();
    let count: usize = db
      .query_row("SELECT count(*) FROM key_recovery_requests", [], |row| {
        row.get(0)
      })
      .unwrap();
    assert_eq!(count, 0);
  }
}
