use super::{Head, Owner, Revision, Write};
use crate::{Result, Store, StoreError};
use axum::http::StatusCode;
use chrono::{SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

pub(crate) fn migrate(connection: &Connection) -> Result<()> {
  connection
    .execute_batch(
      "CREATE TABLE IF NOT EXISTS config_sync_revisions (
         issuer TEXT NOT NULL,
         subject TEXT NOT NULL,
         revision_id TEXT NOT NULL,
         parent_revision_id TEXT,
         snapshot TEXT NOT NULL,
         updated_at TEXT NOT NULL,
         PRIMARY KEY (issuer, subject, revision_id),
         FOREIGN KEY (issuer, subject, parent_revision_id)
           REFERENCES config_sync_revisions (issuer, subject, revision_id)
       );
       CREATE TABLE IF NOT EXISTS config_sync_heads (
         issuer TEXT NOT NULL,
         subject TEXT NOT NULL,
         revision_id TEXT NOT NULL,
         PRIMARY KEY (issuer, subject),
         FOREIGN KEY (issuer, subject, revision_id)
           REFERENCES config_sync_revisions (issuer, subject, revision_id)
       );",
    )
    .map_err(|_| StoreError::internal())
}

impl Store {
  pub(super) fn config_sync_head(&self, owner: &Owner) -> Result<Head> {
    read_head(&self.connection()?, owner)
  }

  pub(super) fn write_config_sync_head(&self, owner: &Owner, input: Write) -> Result<Head> {
    let mut db = self.connection()?;
    let tx = db
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    let head = read_head(&tx, owner)?;
    let current_id = head
      .head
      .as_ref()
      .map(|current| current.revision_id.as_str());
    if let Some(current) = head
      .head
      .as_ref()
      .filter(|current| current.revision_id == input.revision_id)
    {
      if current.parent_revision_ids == input.parent_revision_ids
        && current.snapshot == input.snapshot
      {
        return Ok(head);
      }
      return Err(conflict());
    }
    if current_id != input.expected_head.as_deref() {
      return Err(conflict());
    }
    let updated_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let snapshot = serde_json::to_string(&input.snapshot).map_err(|_| StoreError::internal())?;
    let inserted = tx
      .execute(
        "INSERT INTO config_sync_revisions
         (issuer, subject, revision_id, parent_revision_id, snapshot, updated_at)
       VALUES (?1, ?2, ?3, ?4, ?5, ?6)
       ON CONFLICT (issuer, subject, revision_id) DO NOTHING",
        params![
          owner.issuer,
          owner.subject,
          input.revision_id,
          input.expected_head,
          snapshot,
          updated_at
        ],
      )
      .map_err(|_| StoreError::internal())?;
    if inserted != 1 {
      return Err(conflict());
    }
    tx.execute(
      "INSERT INTO config_sync_heads (issuer, subject, revision_id) VALUES (?1, ?2, ?3)
       ON CONFLICT (issuer, subject) DO UPDATE SET revision_id = excluded.revision_id",
      params![owner.issuer, owner.subject, input.revision_id],
    )
    .map_err(|_| StoreError::internal())?;
    tx.commit().map_err(|_| StoreError::internal())?;
    Ok(Head {
      head: Some(Revision {
        revision_id: input.revision_id,
        parent_revision_ids: input.parent_revision_ids,
        snapshot: input.snapshot,
        updated_at,
      }),
    })
  }
}

fn read_head(connection: &Connection, owner: &Owner) -> Result<Head> {
  let row = connection
    .query_row(
      "SELECT r.revision_id, r.parent_revision_id, r.snapshot, r.updated_at
       FROM config_sync_heads h
       JOIN config_sync_revisions r USING (issuer, subject, revision_id)
       WHERE h.issuer = ?1 AND h.subject = ?2",
      params![owner.issuer, owner.subject],
      |row| {
        Ok((
          row.get::<_, String>(0)?,
          row.get::<_, Option<String>>(1)?,
          row.get::<_, String>(2)?,
          row.get::<_, String>(3)?,
        ))
      },
    )
    .optional()
    .map_err(|_| StoreError::internal())?;
  let head = row
    .map(|(revision_id, parent, snapshot, updated_at)| {
      let snapshot = serde_json::from_str(&snapshot).map_err(|_| StoreError::internal())?;
      Ok(Revision {
        revision_id,
        parent_revision_ids: parent.into_iter().collect(),
        snapshot,
        updated_at,
      })
    })
    .transpose()?;
  Ok(Head { head })
}

fn conflict() -> StoreError {
  StoreError::new(
    StatusCode::CONFLICT,
    "config_sync_conflict",
    "Cloud provider settings changed. Check the latest settings before syncing.",
  )
}
