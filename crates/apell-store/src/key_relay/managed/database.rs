use super::{
  busy, conflict, missing_pair, open_vault, random_code, seal_vault, storage_error, verify_proof,
  Key, Owner, PairSummary, Status,
};
use crate::key_relay::database as legacy;
use crate::Result;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use subtle::ConstantTimeEq;

const PAIR_TTL: i64 = 5 * 60;
const MAX_RECENT_PER_ACCOUNT: i64 = 16;
const MAX_RECENT_TOTAL: i64 = 2048;

pub(in crate::key_relay) fn migrate(db: &Connection) -> Result<()> {
  db.execute_batch(
    "CREATE TABLE IF NOT EXISTS managed_key_vaults (
       issuer TEXT NOT NULL,
       subject TEXT NOT NULL,
       vault_id TEXT NOT NULL,
       key_nonce BLOB NOT NULL CHECK (length(key_nonce) = 12),
       key_ciphertext BLOB NOT NULL CHECK (length(key_ciphertext) = 48),
       PRIMARY KEY (issuer, subject),
       UNIQUE (issuer, subject, vault_id)
     );
     CREATE TABLE IF NOT EXISTS managed_key_pairs (
       issuer TEXT NOT NULL,
       subject TEXT NOT NULL,
       vault_id TEXT NOT NULL,
       pair_id TEXT NOT NULL,
       code TEXT NOT NULL CHECK (length(code) = 6),
       approval_nonce TEXT NOT NULL CHECK (length(approval_nonce) = 22),
       request_digest BLOB NOT NULL CHECK (length(request_digest) = 32),
       expires_at INTEGER NOT NULL,
       approved INTEGER NOT NULL DEFAULT 0 CHECK (approved IN (0, 1)),
       consumed INTEGER NOT NULL DEFAULT 0 CHECK (consumed IN (0, 1)),
       PRIMARY KEY (issuer, subject, pair_id),
       FOREIGN KEY (issuer, subject, vault_id)
         REFERENCES managed_key_vaults (issuer, subject, vault_id)
     );
     CREATE INDEX IF NOT EXISTS managed_key_pairs_expiry ON managed_key_pairs (expires_at);",
  )
  .map_err(|_| storage_error())
}

fn id(db: &Connection, owner: &Owner) -> Result<Option<String>> {
  db.query_row(
    "SELECT vault_id FROM managed_key_vaults WHERE issuer = ?1 AND subject = ?2",
    params![owner.issuer, owner.subject],
    |row| row.get(0),
  )
  .optional()
  .map_err(|_| storage_error())
}

pub(in crate::key_relay) fn has_account(db: &Connection, owner: &Owner) -> Result<bool> {
  Ok(id(db, owner)?.is_some())
}

pub(super) fn status(db: &Connection, owner: &Owner) -> Result<Status> {
  if let Some(vault_id) = id(db, owner)? {
    return Ok(Status {
      vault_id: Some(vault_id),
      managed: true,
    });
  }
  Ok(Status {
    vault_id: legacy::vault(db, owner)?.map(|vault| vault.vault_id),
    managed: false,
  })
}

pub(super) fn create(
  db: &mut Connection,
  master: &Key,
  owner: &Owner,
  vault_id: &str,
  key: &Key,
  legacy_enrollment: bool,
) -> Result<()> {
  let tx = db
    .transaction_with_behavior(TransactionBehavior::Immediate)
    .map_err(|_| storage_error())?;
  create_in_transaction(&tx, master, owner, vault_id, key, legacy_enrollment)?;
  tx.commit().map_err(|_| storage_error())
}

pub(super) fn create_in_transaction(
  tx: &Transaction<'_>,
  master: &Key,
  owner: &Owner,
  vault_id: &str,
  key: &Key,
  legacy_enrollment: bool,
) -> Result<()> {
  if let Some(existing) = id(tx, owner)? {
    if existing == vault_id
      && bool::from(load_key(tx, master, owner, &existing)?.ct_eq(key.as_ref()))
    {
      return Ok(());
    }
    return Err(conflict("This Account already has a managed key vault."));
  }
  match (legacy::vault(tx, owner)?, legacy_enrollment) {
    (Some(vault), true) if vault.vault_id == vault_id => {}
    (Some(_), _) => {
      return Err(conflict(
        "Verify your Account to update recovery for this key vault.",
      ))
    }
    (None, false) => {}
    (None, true) => return Err(conflict("This Account has no existing key vault.")),
  }
  let (nonce, envelope) = seal_vault(master, owner, vault_id, key)?;
  tx.execute(
    "INSERT INTO managed_key_vaults (issuer, subject, vault_id, key_nonce, key_ciphertext)
     VALUES (?1, ?2, ?3, ?4, ?5)",
    params![
      owner.issuer,
      owner.subject,
      vault_id,
      nonce.as_slice(),
      envelope
    ],
  )
  .map_err(|_| storage_error())?;
  Ok(())
}

pub(super) fn load_key(
  db: &Connection,
  master: &Key,
  owner: &Owner,
  vault_id: &str,
) -> Result<Key> {
  let (nonce, envelope) = db
    .query_row(
      "SELECT key_nonce, key_ciphertext FROM managed_key_vaults
       WHERE issuer = ?1 AND subject = ?2 AND vault_id = ?3",
      params![owner.issuer, owner.subject, vault_id],
      |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(|_| storage_error())?
    .ok_or_else(|| conflict("This Account has no matching managed key vault."))?;
  open_vault(master, owner, vault_id, nonce, envelope)
}

pub(in crate::key_relay) fn prune(db: &Connection, now: i64) -> Result<()> {
  db.execute(
    "DELETE FROM managed_key_pairs WHERE expires_at <= ?1",
    [now],
  )
  .map(|_| ())
  .map_err(|_| storage_error())
}

struct Pair {
  summary: PairSummary,
  vault_id: String,
  request_digest: Vec<u8>,
  consumed: bool,
}

fn read_pair(db: &Connection, owner: &Owner, pair_id: &str, now: i64) -> Result<Option<Pair>> {
  db.query_row(
    "SELECT vault_id, code, approval_nonce, request_digest, expires_at, approved, consumed
     FROM managed_key_pairs
     WHERE issuer = ?1 AND subject = ?2 AND pair_id = ?3 AND expires_at > ?4",
    params![owner.issuer, owner.subject, pair_id, now],
    |row| {
      Ok(Pair {
        summary: PairSummary {
          pair_id: pair_id.to_owned(),
          code: row.get(1)?,
          approval_nonce: row.get(2)?,
          expires_at: row.get(4)?,
          approved: row.get(5)?,
        },
        vault_id: row.get(0)?,
        request_digest: row.get(3)?,
        consumed: row.get(6)?,
      })
    },
  )
  .optional()
  .map_err(|_| storage_error())
}

fn request_matches(pair: &Pair, request_digest: &[u8; 32]) -> bool {
  bool::from(pair.request_digest.ct_eq(request_digest.as_slice()))
}

pub(super) fn start_pair(
  db: &mut Connection,
  owner: &Owner,
  vault_id: &str,
  pair_id: &str,
  request_digest: &[u8; 32],
  now: i64,
) -> Result<PairSummary> {
  let tx = db
    .transaction_with_behavior(TransactionBehavior::Immediate)
    .map_err(|_| storage_error())?;
  if id(&tx, owner)?.as_deref() != Some(vault_id) {
    return Err(conflict("This Account has no matching managed key vault."));
  }
  if let Some(pair) = read_pair(&tx, owner, pair_id, now)? {
    return if !pair.consumed && pair.vault_id == vault_id && request_matches(&pair, request_digest)
    {
      Ok(pair.summary)
    } else {
      Err(missing_pair())
    };
  }
  prune(&tx, now)?;
  let (total, recent): (i64, i64) = tx
    .query_row(
      "SELECT (SELECT COUNT(*) FROM managed_key_pairs),
              (SELECT COUNT(*) FROM managed_key_pairs WHERE issuer = ?1 AND subject = ?2)",
      params![owner.issuer, owner.subject],
      |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .map_err(|_| storage_error())?;
  if total >= MAX_RECENT_TOTAL || recent >= MAX_RECENT_PER_ACCOUNT {
    return Err(busy());
  }
  // Codes distinguish concurrent nearby requests. They are never an input to
  // authorization. Keep them unique even across recently consumed requests.
  let code = loop {
    let code = random_code()?;
    let exists: bool = tx
      .query_row(
        "SELECT EXISTS(SELECT 1 FROM managed_key_pairs
         WHERE issuer = ?1 AND subject = ?2 AND code = ?3)",
        params![owner.issuer, owner.subject, code],
        |row| row.get(0),
      )
      .map_err(|_| storage_error())?;
    if !exists {
      break code;
    }
  };
  let mut approval_nonce = [0; 16];
  getrandom::fill(&mut approval_nonce).map_err(|_| storage_error())?;
  let summary = PairSummary {
    pair_id: pair_id.to_owned(),
    code,
    approval_nonce: URL_SAFE_NO_PAD.encode(approval_nonce),
    expires_at: now + PAIR_TTL,
    approved: false,
  };
  tx.execute(
    "INSERT INTO managed_key_pairs
       (issuer, subject, vault_id, pair_id, code, approval_nonce, request_digest, expires_at)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    params![
      owner.issuer,
      owner.subject,
      vault_id,
      pair_id,
      summary.code,
      summary.approval_nonce,
      request_digest.as_slice(),
      summary.expires_at
    ],
  )
  .map_err(|_| storage_error())?;
  tx.commit().map_err(|_| storage_error())?;
  Ok(summary)
}

pub(super) fn list_pairs(
  db: &Connection,
  owner: &Owner,
  vault_id: &str,
  now: i64,
) -> Result<Vec<PairSummary>> {
  let mut statement = db
    .prepare(
      "SELECT pair_id, code, approval_nonce, expires_at
       FROM managed_key_pairs
       WHERE issuer = ?1 AND subject = ?2 AND vault_id = ?3 AND expires_at > ?4
         AND consumed = 0 AND approved = 0
       ORDER BY expires_at, pair_id",
    )
    .map_err(|_| storage_error())?;
  let rows = statement
    .query_map(params![owner.issuer, owner.subject, vault_id, now], |row| {
      Ok(PairSummary {
        pair_id: row.get(0)?,
        code: row.get(1)?,
        approval_nonce: row.get(2)?,
        expires_at: row.get(3)?,
        approved: false,
      })
    })
    .map_err(|_| storage_error())?;
  rows
    .collect::<rusqlite::Result<_>>()
    .map_err(|_| storage_error())
}

pub(super) fn approve_pair(
  db: &mut Connection,
  master: &Key,
  owner: &Owner,
  vault_id: &str,
  pair_id: &str,
  proof: &Key,
  now: i64,
) -> Result<()> {
  let tx = db
    .transaction_with_behavior(TransactionBehavior::Immediate)
    .map_err(|_| storage_error())?;
  let pair = read_pair(&tx, owner, pair_id, now)?
    .filter(|pair| !pair.consumed && pair.vault_id == vault_id)
    .ok_or_else(missing_pair)?;
  let key = load_key(&tx, master, owner, vault_id)?;
  verify_proof(
    &key,
    proof.as_ref(),
    "approve",
    vault_id,
    pair_id,
    Some(&pair.summary.approval_nonce),
  )?;
  tx.execute(
    "UPDATE managed_key_pairs SET approved = 1
     WHERE issuer = ?1 AND subject = ?2 AND pair_id = ?3",
    params![owner.issuer, owner.subject, pair_id],
  )
  .map_err(|_| storage_error())?;
  tx.commit().map_err(|_| storage_error())
}

pub(super) fn finish_pair(
  db: &Connection,
  master: &Key,
  owner: &Owner,
  pair_id: &str,
  request_digest: &[u8; 32],
  now: i64,
) -> Result<Option<Key>> {
  let pair = read_pair(db, owner, pair_id, now)?
    .filter(|pair| !pair.consumed && request_matches(pair, request_digest))
    .ok_or_else(missing_pair)?;
  if pair.summary.approved {
    load_key(db, master, owner, &pair.vault_id).map(Some)
  } else {
    Ok(None)
  }
}

pub(super) fn consume_pair(
  db: &mut Connection,
  owner: &Owner,
  pair_id: &str,
  request_digest: &[u8; 32],
  now: i64,
) -> Result<()> {
  let tx = db
    .transaction_with_behavior(TransactionBehavior::Immediate)
    .map_err(|_| storage_error())?;
  read_pair(&tx, owner, pair_id, now)?
    .filter(|pair| request_matches(pair, request_digest))
    .ok_or_else(missing_pair)?;
  tx.execute(
    "UPDATE managed_key_pairs SET consumed = 1
     WHERE issuer = ?1 AND subject = ?2 AND pair_id = ?3",
    params![owner.issuer, owner.subject, pair_id],
  )
  .map_err(|_| storage_error())?;
  tx.commit().map_err(|_| storage_error())
}
