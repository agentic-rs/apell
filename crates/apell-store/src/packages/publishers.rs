use super::{
  conflict,
  database::{audit, ensure_user, require_member, reserve_package},
};
use crate::{auth::User, oidc::OidcConfig, Result, Store, StoreError, TrustedPublisher};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

impl Store {
  /// Import the operator-managed policy once, and only explicitly assign reserved
  /// packages. A restart must never resurrect a disconnected publisher or reclaim
  /// a package that has already been transferred to another owner.
  pub fn bootstrap_packages(
    &self,
    publishers: &[TrustedPublisher],
    owner: Option<&User>,
  ) -> Result<()> {
    validate_publishers(publishers)?;
    if let Some(owner) = owner {
      self.upsert_user(owner)?;
    }
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    let imported = transaction
      .query_row(
        "SELECT EXISTS(SELECT 1 FROM package_migrations WHERE name='trusted_publishers_imported')",
        [],
        |row| row.get::<_, bool>(0),
      )
      .map_err(|_| StoreError::internal())?;
    if !imported {
      for publisher in publishers {
        reserve_package(&transaction, &publisher.applet_id)?;
        // An existing database-managed policy wins over legacy configuration.
        let added = transaction
          .execute(
            "INSERT OR IGNORE INTO package_publishers(applet_id,policy_json) VALUES(?1,?2)",
            params![
              publisher.applet_id,
              serde_json::to_string(publisher).map_err(|_| StoreError::internal())?
            ],
          )
          .map_err(|_| StoreError::internal())?;
        if added > 0 {
          bump_revision(&transaction, &publisher.applet_id)?;
          audit(
            &transaction,
            &publisher.applet_id,
            None,
            "publisher_imported",
            None,
          )?;
        }
      }
      transaction
        .execute(
          "INSERT INTO package_migrations(name) VALUES('trusted_publishers_imported')",
          [],
        )
        .map_err(|_| StoreError::internal())?;
    }
    if let Some(owner) = owner {
      ensure_user(&transaction, owner.github_id)?;
      let mut statement = transaction
        .prepare("SELECT applet_id FROM packages WHERE owner_github_id IS NULL ORDER BY applet_id")
        .map_err(|_| StoreError::internal())?;
      let ids = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|_| StoreError::internal())?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| StoreError::internal())?;
      drop(statement);
      for id in ids {
        transaction.execute(
          "UPDATE packages SET owner_github_id=?2,status='active' WHERE applet_id=?1 AND owner_github_id IS NULL",
          params![id, owner.github_id],
        ).map_err(|_| StoreError::internal())?;
        audit(
          &transaction,
          &id,
          None,
          "owner_bootstrapped",
          Some(owner.github_id),
        )?;
      }
    }
    transaction.commit().map_err(|_| StoreError::internal())
  }

  pub fn set_publisher(&self, id: &str, actor_id: u64, publisher: TrustedPublisher) -> Result<()> {
    if publisher.applet_id != id {
      return Err(StoreError::invalid(
        "Publisher applet ID does not match the package.",
      ));
    }
    validate_publishers(std::slice::from_ref(&publisher))?;
    let policy = serde_json::to_string(&publisher).map_err(|_| StoreError::internal())?;
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    require_member(&transaction, id, actor_id)?;
    transaction
      .execute(
        "INSERT INTO package_publishers(applet_id,policy_json) VALUES(?1,?2)
       ON CONFLICT(applet_id) DO UPDATE SET policy_json=excluded.policy_json",
        params![id, policy],
      )
      .map_err(|_| StoreError::internal())?;
    bump_revision(&transaction, id)?;
    audit(
      &transaction,
      id,
      Some(actor_id),
      "publisher_configured",
      None,
    )?;
    transaction.commit().map_err(|_| StoreError::internal())
  }

  pub fn disconnect_publisher(&self, id: &str, actor_id: u64) -> Result<()> {
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    require_member(&transaction, id, actor_id)?;
    transaction
      .execute("DELETE FROM package_publishers WHERE applet_id=?1", [id])
      .map_err(|_| StoreError::internal())?;
    bump_revision(&transaction, id)?;
    audit(
      &transaction,
      id,
      Some(actor_id),
      "publisher_disconnected",
      None,
    )?;
    transaction.commit().map_err(|_| StoreError::internal())
  }

  pub fn publisher_for(&self, id: &str) -> Result<Option<(TrustedPublisher, u64)>> {
    read_publisher(&self.connection()?, id)
  }

  pub fn all_publishers(&self) -> Result<Vec<(TrustedPublisher, u64)>> {
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction()
      .map_err(|_| StoreError::internal())?;
    let mut statement = transaction
      .prepare("SELECT applet_id FROM package_publishers ORDER BY applet_id")
      .map_err(|_| StoreError::internal())?;
    let ids = statement
      .query_map([], |row| row.get::<_, String>(0))
      .map_err(|_| StoreError::internal())?
      .collect::<std::result::Result<Vec<_>, _>>()
      .map_err(|_| StoreError::internal())?;
    ids
      .into_iter()
      .map(|id| read_publisher(&transaction, &id)?.ok_or_else(StoreError::internal))
      .collect()
  }
}

pub(super) fn read_publisher(
  connection: &Connection,
  id: &str,
) -> Result<Option<(TrustedPublisher, u64)>> {
  let row = connection
    .query_row(
      "SELECT o.policy_json,p.policy_revision FROM package_publishers o
     JOIN packages p ON p.applet_id=o.applet_id WHERE o.applet_id=?1",
      [id],
      |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)),
    )
    .optional()
    .map_err(|_| StoreError::internal())?;
  row
    .map(|(json, revision)| {
      let publisher: TrustedPublisher =
        serde_json::from_str(&json).map_err(|_| StoreError::internal())?;
      if publisher.applet_id != id {
        return Err(StoreError::internal());
      }
      validate_publishers(std::slice::from_ref(&publisher)).map_err(|_| StoreError::internal())?;
      Ok((publisher, revision))
    })
    .transpose()
}

fn validate_publishers(publishers: &[TrustedPublisher]) -> Result<()> {
  OidcConfig {
    audience: "https://store.example.invalid".into(),
    publishers: publishers.to_vec(),
  }
  .validate()
  .map_err(StoreError::invalid)
}

fn bump_revision(connection: &Connection, id: &str) -> Result<()> {
  let changed = connection
    .execute(
      "UPDATE packages SET policy_revision=policy_revision+1
     WHERE applet_id=?1 AND policy_revision<9223372036854775807",
      [id],
    )
    .map_err(|_| StoreError::internal())?;
  if changed == 1 {
    Ok(())
  } else {
    Err(conflict(
      "This package cannot update its publishing policy.",
    ))
  }
}
