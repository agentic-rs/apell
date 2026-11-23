use super::{busy, conflict, missing_pair, storage_error, Envelope, Owner, PairStatus, Vault};
use crate::Result;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::{path::Path, time::Duration};

const PAIR_TTL: i64 = 5 * 60;
const MAX_RECENT_PAIRS_PER_ACCOUNT: i64 = 16;

pub(super) fn open(root: &Path) -> Result<Connection> {
  std::fs::create_dir_all(root).map_err(|_| storage_error())?;
  let connection = Connection::open(root.join("key-relay.sqlite3")).map_err(|_| storage_error())?;
  connection
    .busy_timeout(Duration::from_secs(5))
    .map_err(|_| storage_error())?;
  connection
    .execute_batch(
      "PRAGMA journal_mode=WAL;
     PRAGMA synchronous=FULL;
     PRAGMA foreign_keys=ON;
     PRAGMA secure_delete=ON;
     CREATE TABLE IF NOT EXISTS key_vaults (
       issuer TEXT NOT NULL,
       subject TEXT NOT NULL,
       vault_id TEXT NOT NULL,
       recovery_nonce TEXT NOT NULL,
       recovery_ciphertext TEXT NOT NULL,
       PRIMARY KEY (issuer, subject),
       UNIQUE (issuer, subject, vault_id)
     );
     CREATE TABLE IF NOT EXISTS key_pairs (
       issuer TEXT NOT NULL,
       subject TEXT NOT NULL,
       pair_id TEXT NOT NULL,
       vault_id TEXT NOT NULL,
       expires_at INTEGER NOT NULL,
       envelope_nonce TEXT,
       envelope_ciphertext TEXT,
       consumed INTEGER NOT NULL DEFAULT 0 CHECK (consumed IN (0, 1)),
       CHECK ((envelope_nonce IS NULL) = (envelope_ciphertext IS NULL)),
       CHECK (consumed = 0 OR envelope_nonce IS NULL),
       PRIMARY KEY (issuer, subject, pair_id),
       FOREIGN KEY (issuer, subject, vault_id) REFERENCES key_vaults (issuer, subject, vault_id)
     );
     CREATE INDEX IF NOT EXISTS key_pairs_expiry ON key_pairs (expires_at);",
    )
    .map_err(|_| storage_error())?;
  super::managed::migrate(&connection)?;
  Ok(connection)
}

pub(super) fn vault(db: &Connection, owner: &Owner) -> Result<Option<Vault>> {
  db.query_row(
    "SELECT vault_id, recovery_nonce, recovery_ciphertext
     FROM key_vaults WHERE issuer = ?1 AND subject = ?2",
    params![owner.issuer, owner.subject],
    |row| {
      Ok(Vault {
        vault_id: row.get(0)?,
        recovery: Envelope {
          nonce: row.get(1)?,
          ciphertext: row.get(2)?,
        },
      })
    },
  )
  .optional()
  .map_err(|_| storage_error())
}

pub(super) fn create_vault(db: &mut Connection, owner: &Owner, incoming: &Vault) -> Result<()> {
  let tx = db
    .transaction_with_behavior(TransactionBehavior::Immediate)
    .map_err(|_| storage_error())?;
  if let Some(existing) = vault(&tx, owner)? {
    if &existing == incoming {
      return Ok(());
    }
    return Err(conflict("This Account already has an encrypted key vault."));
  }
  if super::managed::has_account(&tx, owner)? {
    return Err(conflict("This Account already has an encrypted key vault."));
  }
  tx.execute(
    "INSERT INTO key_vaults (issuer, subject, vault_id, recovery_nonce, recovery_ciphertext)
     VALUES (?1, ?2, ?3, ?4, ?5)",
    params![
      owner.issuer,
      owner.subject,
      incoming.vault_id,
      incoming.recovery.nonce,
      incoming.recovery.ciphertext
    ],
  )
  .map_err(|_| storage_error())?;
  tx.commit().map_err(|_| storage_error())
}

pub(super) fn prune_expired(db: &Connection, now: i64) -> Result<()> {
  db.execute("DELETE FROM key_pairs WHERE expires_at <= ?1", [now])
    .map(|_| ())
    .map_err(|_| storage_error())
}

pub(super) fn create_pair(
  db: &mut Connection,
  owner: &Owner,
  pair_id: &str,
  vault_id: &str,
  now: i64,
) -> Result<()> {
  let tx = db
    .transaction_with_behavior(TransactionBehavior::Immediate)
    .map_err(|_| storage_error())?;
  if !vault(&tx, owner)?.is_some_and(|active| active.vault_id == vault_id) {
    return Err(conflict(
      "The Account key vault has changed. Reload it and try again.",
    ));
  }
  let existing = tx
    .query_row(
      "SELECT expires_at, consumed FROM key_pairs
     WHERE issuer = ?1 AND subject = ?2 AND pair_id = ?3",
      params![owner.issuer, owner.subject, pair_id],
      |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?)),
    )
    .optional()
    .map_err(|_| storage_error())?;
  if let Some((expires_at, consumed)) = existing {
    return if !consumed && expires_at > now {
      Ok(())
    } else {
      Err(missing_pair())
    };
  }
  prune_expired(&tx, now)?;
  let recent: i64 = tx
    .query_row(
      "SELECT COUNT(*) FROM key_pairs WHERE issuer = ?1 AND subject = ?2",
      params![owner.issuer, owner.subject],
      |row| row.get(0),
    )
    .map_err(|_| storage_error())?;
  if recent >= MAX_RECENT_PAIRS_PER_ACCOUNT {
    return Err(busy());
  }
  tx.execute(
    "INSERT INTO key_pairs (issuer, subject, pair_id, vault_id, expires_at)
     VALUES (?1, ?2, ?3, ?4, ?5)",
    params![
      owner.issuer,
      owner.subject,
      pair_id,
      vault_id,
      now + PAIR_TTL
    ],
  )
  .map_err(|_| storage_error())?;
  tx.commit().map_err(|_| storage_error())
}

pub(super) fn pair(db: &Connection, owner: &Owner, pair_id: &str, now: i64) -> Result<PairStatus> {
  db.query_row(
    "SELECT vault_id, envelope_nonce, envelope_ciphertext FROM key_pairs
     WHERE issuer = ?1 AND subject = ?2 AND pair_id = ?3 AND expires_at > ?4 AND consumed = 0",
    params![owner.issuer, owner.subject, pair_id, now],
    |row| {
      Ok(PairStatus {
        vault_id: row.get(0)?,
        envelope: row
          .get::<_, Option<String>>(1)?
          .zip(row.get::<_, Option<String>>(2)?)
          .map(|(nonce, ciphertext)| Envelope { nonce, ciphertext }),
      })
    },
  )
  .optional()
  .map_err(|_| storage_error())?
  .ok_or_else(missing_pair)
}

pub(super) fn publish_pair(
  db: &mut Connection,
  owner: &Owner,
  pair_id: &str,
  incoming: &Envelope,
  now: i64,
) -> Result<()> {
  let tx = db
    .transaction_with_behavior(TransactionBehavior::Immediate)
    .map_err(|_| storage_error())?;
  if let Some(existing) = pair(&tx, owner, pair_id, now)?.envelope {
    if &existing == incoming {
      return Ok(());
    }
    return Err(conflict("This pairing request already has a key envelope."));
  }
  tx.execute(
    "UPDATE key_pairs SET envelope_nonce = ?4, envelope_ciphertext = ?5
     WHERE issuer = ?1 AND subject = ?2 AND pair_id = ?3",
    params![
      owner.issuer,
      owner.subject,
      pair_id,
      incoming.nonce,
      incoming.ciphertext
    ],
  )
  .map_err(|_| storage_error())?;
  tx.commit().map_err(|_| storage_error())
}

pub(super) fn consume_pair(db: &Connection, owner: &Owner, pair_id: &str, now: i64) -> Result<()> {
  // Keep the consumed ID until it expires, so a retried create cannot reopen it.
  db.execute(
    "UPDATE key_pairs SET consumed = 1, envelope_nonce = NULL, envelope_ciphertext = NULL
     WHERE issuer = ?1 AND subject = ?2 AND pair_id = ?3 AND expires_at > ?4 AND consumed = 0",
    params![owner.issuer, owner.subject, pair_id, now],
  )
  .map(|_| ())
  .map_err(|_| storage_error())
}
