use super::{Head, Owner, Revision, Write};
use crate::{Result, Store, StoreError};
use axum::http::StatusCode;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

pub(crate) fn migrate(connection: &Connection) -> Result<()> {
  connection
    .execute_batch(
      "CREATE TABLE IF NOT EXISTS credential_sync_revisions (
         issuer TEXT NOT NULL,
         subject TEXT NOT NULL,
         revision_id TEXT NOT NULL,
         parent_revision_id TEXT,
         PRIMARY KEY (issuer, subject, revision_id),
         FOREIGN KEY (issuer, subject, parent_revision_id)
           REFERENCES credential_sync_revisions (issuer, subject, revision_id)
       );
       CREATE TABLE IF NOT EXISTS credential_sync_heads (
         issuer TEXT NOT NULL,
         subject TEXT NOT NULL,
         revision_id TEXT NOT NULL,
         vault_id TEXT NOT NULL,
         entries TEXT NOT NULL,
         PRIMARY KEY (issuer, subject),
         FOREIGN KEY (issuer, subject, revision_id)
           REFERENCES credential_sync_revisions (issuer, subject, revision_id)
       );",
    )
    .map_err(|_| StoreError::internal())
}

struct SavedHead {
  revision: Revision,
  parent_revision_id: Option<String>,
}

impl Store {
  pub(super) fn credential_sync_head(&self, owner: &Owner) -> Result<Head> {
    Ok(Head {
      head: read_head(&self.connection()?, owner)?.map(|head| head.revision),
    })
  }

  pub(super) fn write_credential_sync_head(&self, owner: &Owner, input: Write) -> Result<Head> {
    let mut db = self.connection()?;
    let tx = db
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    let current = read_head(&tx, owner)?;
    if let Some(head) = current
      .as_ref()
      .filter(|head| head.revision.revision_id == input.revision_id)
    {
      if head.parent_revision_id == input.expected_head
        && head.revision.vault_id == input.vault_id
        && head.revision.entries == input.entries
      {
        return Ok(Head {
          head: current.map(|head| head.revision),
        });
      }
      return Err(conflict());
    }
    if current
      .as_ref()
      .map(|head| head.revision.revision_id.as_str())
      != input.expected_head.as_deref()
    {
      return Err(conflict());
    }

    let inserted = tx
      .execute(
        "INSERT INTO credential_sync_revisions (issuer, subject, revision_id, parent_revision_id)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (issuer, subject, revision_id) DO NOTHING",
        params![
          owner.issuer,
          owner.subject,
          input.revision_id,
          input.expected_head
        ],
      )
      .map_err(|_| StoreError::internal())?;
    if inserted != 1 {
      return Err(conflict());
    }
    // Retain ancestry and revision identifiers for conflict checks. Only the
    // current revision needs to store any encrypted API key entries.
    let entries = serde_json::to_string(&input.entries).map_err(|_| StoreError::internal())?;
    tx.execute(
      "INSERT INTO credential_sync_heads (issuer, subject, revision_id, vault_id, entries)
       VALUES (?1, ?2, ?3, ?4, ?5)
       ON CONFLICT (issuer, subject) DO UPDATE SET
         revision_id = excluded.revision_id, vault_id = excluded.vault_id, entries = excluded.entries",
      params![owner.issuer, owner.subject, input.revision_id, input.vault_id, entries],
    )
    .map_err(|_| StoreError::internal())?;
    tx.commit().map_err(|_| StoreError::internal())?;
    Ok(Head {
      head: Some(Revision {
        revision_id: input.revision_id,
        vault_id: input.vault_id,
        entries: input.entries,
      }),
    })
  }
}

fn read_head(connection: &Connection, owner: &Owner) -> Result<Option<SavedHead>> {
  let row = connection
    .query_row(
      "SELECT h.revision_id, h.vault_id, h.entries, r.parent_revision_id
       FROM credential_sync_heads h
       JOIN credential_sync_revisions r USING (issuer, subject, revision_id)
       WHERE h.issuer = ?1 AND h.subject = ?2",
      params![owner.issuer, owner.subject],
      |row| {
        Ok((
          row.get::<_, String>(0)?,
          row.get::<_, String>(1)?,
          row.get::<_, String>(2)?,
          row.get::<_, Option<String>>(3)?,
        ))
      },
    )
    .optional()
    .map_err(|_| StoreError::internal())?;
  row
    .map(|(revision_id, vault_id, entries, parent_revision_id)| {
      let entries = serde_json::from_str(&entries).map_err(|_| StoreError::internal())?;
      Ok(SavedHead {
        revision: Revision {
          revision_id,
          vault_id,
          entries,
        },
        parent_revision_id,
      })
    })
    .transpose()
}

fn conflict() -> StoreError {
  StoreError::new(
    StatusCode::CONFLICT,
    "credential_sync_conflict",
    "Cloud credentials changed. Check the latest encrypted keys before syncing.",
  )
}
