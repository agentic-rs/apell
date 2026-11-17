use super::{conflict, forbidden, now, PackageRole, MAX_PACKAGES};
use crate::{auth::User, Result, StoreError};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

pub(crate) fn migrate(connection: &Connection) -> Result<()> {
  let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
    .map_err(|_| StoreError::internal())?;
  transaction
    .execute_batch(
      "CREATE TABLE IF NOT EXISTS packages (
      applet_id TEXT PRIMARY KEY,
      owner_github_id INTEGER REFERENCES users(github_id),
      status TEXT NOT NULL CHECK(status IN ('reserved','active')),
      policy_revision INTEGER NOT NULL DEFAULT 0 CHECK(policy_revision>=0),
      created_at INTEGER NOT NULL,
      CHECK((status='reserved' AND owner_github_id IS NULL)
        OR (status='active' AND owner_github_id IS NOT NULL))
    );
    CREATE TABLE IF NOT EXISTS package_maintainers (
      applet_id TEXT NOT NULL REFERENCES packages(applet_id),
      github_id INTEGER NOT NULL REFERENCES users(github_id),
      added_by INTEGER NOT NULL REFERENCES users(github_id),
      added_at INTEGER NOT NULL,
      PRIMARY KEY(applet_id,github_id)
    );
    CREATE TABLE IF NOT EXISTS package_invitations (
      id TEXT PRIMARY KEY,
      applet_id TEXT NOT NULL REFERENCES packages(applet_id),
      kind TEXT NOT NULL CHECK(kind IN ('maintainer','ownership')),
      target_github_id INTEGER NOT NULL REFERENCES users(github_id),
      invited_by INTEGER NOT NULL REFERENCES users(github_id),
      expires_at INTEGER NOT NULL,
      UNIQUE(applet_id,kind,target_github_id)
    );
    CREATE INDEX IF NOT EXISTS package_invitation_targets
      ON package_invitations(target_github_id,expires_at);
    CREATE TABLE IF NOT EXISTS package_publishers (
      applet_id TEXT PRIMARY KEY REFERENCES packages(applet_id),
      policy_json TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS package_audit (
      id INTEGER PRIMARY KEY,
      applet_id TEXT NOT NULL REFERENCES packages(applet_id),
      actor_github_id INTEGER REFERENCES users(github_id),
      action TEXT NOT NULL,
      target_github_id INTEGER REFERENCES users(github_id),
      occurred_at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS package_migrations (
      name TEXT PRIMARY KEY
    );",
    )
    .map_err(|_| StoreError::internal())?;
  // Published IDs must never be claimable through the new public registration.
  transaction
    .execute(
      "INSERT OR IGNORE INTO packages(applet_id,owner_github_id,status,policy_revision,created_at)
     SELECT applet_id,NULL,'reserved',0,?1 FROM applets",
      [now()],
    )
    .map_err(|_| StoreError::internal())?;
  transaction.commit().map_err(|_| StoreError::internal())
}

pub(super) fn ensure_user(connection: &Connection, github_id: u64) -> Result<()> {
  if github_id == 0 || github_id > i64::MAX as u64 || user(connection, github_id)?.is_none() {
    return Err(forbidden());
  }
  Ok(())
}

pub(super) fn user(connection: &Connection, github_id: u64) -> Result<Option<User>> {
  if github_id == 0 || github_id > i64::MAX as u64 {
    return Ok(None);
  }
  connection
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
    .optional()
    .map_err(|_| StoreError::internal())
}

pub(super) fn package_owner(connection: &Connection, id: &str) -> Result<Option<u64>> {
  connection
    .query_row(
      "SELECT owner_github_id FROM packages WHERE applet_id=?1",
      [id],
      |row| row.get::<_, Option<u64>>(0),
    )
    .optional()
    .map_err(|_| StoreError::internal())?
    .ok_or_else(StoreError::missing)
}

pub(super) fn require_member(
  connection: &Connection,
  id: &str,
  actor_id: u64,
) -> Result<PackageRole> {
  ensure_user(connection, actor_id)?;
  let Some(owner) = package_owner(connection, id)? else {
    return Err(forbidden());
  };
  if owner == actor_id {
    return Ok(PackageRole::Owner);
  }
  let member = connection
    .query_row(
      "SELECT EXISTS(SELECT 1 FROM package_maintainers WHERE applet_id=?1 AND github_id=?2)",
      params![id, actor_id],
      |row| row.get::<_, bool>(0),
    )
    .map_err(|_| StoreError::internal())?;
  if member {
    Ok(PackageRole::Maintainer)
  } else {
    Err(forbidden())
  }
}

pub(super) fn require_capacity(connection: &Connection) -> Result<()> {
  let count: i64 = connection
    .query_row("SELECT COUNT(*) FROM packages", [], |row| row.get(0))
    .map_err(|_| StoreError::internal())?;
  if count >= MAX_PACKAGES {
    return Err(conflict("The store has reached its package limit."));
  }
  Ok(())
}

pub(super) fn reserve_package(connection: &Connection, id: &str) -> Result<()> {
  if !apell_package::valid_name(id) {
    return Err(StoreError::invalid("Invalid applet ID."));
  }
  let exists = connection
    .query_row(
      "SELECT EXISTS(SELECT 1 FROM packages WHERE applet_id=?1)",
      [id],
      |row| row.get::<_, bool>(0),
    )
    .map_err(|_| StoreError::internal())?;
  if exists {
    return Ok(());
  }
  require_capacity(connection)?;
  connection
    .execute(
      "INSERT INTO packages(applet_id,owner_github_id,status,policy_revision,created_at)
     VALUES(?1,NULL,'reserved',0,?2)",
      params![id, now()],
    )
    .map_err(|_| StoreError::internal())?;
  audit(connection, id, None, "package_reserved", None)
}

pub(super) fn audit(
  connection: &Connection,
  id: &str,
  actor_id: Option<u64>,
  action: &'static str,
  target_id: Option<u64>,
) -> Result<()> {
  connection
    .execute(
      "INSERT INTO package_audit(applet_id,actor_github_id,action,target_github_id,occurred_at)
     VALUES(?1,?2,?3,?4,?5)",
      params![id, actor_id, action, target_id, now()],
    )
    .map_err(|_| StoreError::internal())?;
  Ok(())
}
