mod database;
mod invitations;
mod publishers;
#[cfg(test)]
mod tests;

use crate::{auth::User, Result, Store, StoreError, TrustedPublisher};
use axum::http::StatusCode;
use rusqlite::{params, Connection, TransactionBehavior};
use serde::Serialize;

pub(crate) use database::migrate;
use database::{audit, ensure_user, package_owner, require_member, user};

const MAX_PACKAGES: i64 = 256;
const MAX_MAINTAINERS: i64 = 31;
const MAX_INVITATIONS: i64 = 64;
const INVITATION_LIFETIME: u64 = 7 * 24 * 60 * 60;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PackageRole {
  Owner,
  Maintainer,
}

#[derive(Clone, Debug, Serialize)]
pub struct ManagedPackage {
  pub applet_id: String,
  pub role: PackageRole,
  pub published: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct PackageDetails {
  pub applet_id: String,
  pub role: PackageRole,
  pub owner: User,
  pub maintainers: Vec<User>,
  pub publisher: Option<TrustedPublisher>,
  pub invitations: Vec<Invitation>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InvitationKind {
  Maintainer,
  Ownership,
}

impl InvitationKind {
  fn as_str(&self) -> &'static str {
    match self {
      Self::Maintainer => "maintainer",
      Self::Ownership => "ownership",
    }
  }
}

#[derive(Clone, Debug, Serialize)]
pub struct Invitation {
  pub id: String,
  pub applet_id: String,
  pub kind: InvitationKind,
  pub target: User,
  pub invited_by: User,
  pub expires_at: u64,
}

#[derive(Clone, Debug)]
pub enum PublishAuthority {
  Administrator,
  User(u64),
  Oidc {
    applet_id: String,
    policy_revision: u64,
  },
}

impl Store {
  pub fn managed_packages(&self, user_id: u64) -> Result<Vec<ManagedPackage>> {
    let connection = self.connection()?;
    ensure_user(&connection, user_id)?;
    let mut statement = connection
      .prepare(
        "SELECT p.applet_id, p.owner_github_id=?1,
        EXISTS(SELECT 1 FROM applets a WHERE a.applet_id=p.applet_id)
       FROM packages p WHERE p.owner_github_id=?1 OR EXISTS(
         SELECT 1 FROM package_maintainers m WHERE m.applet_id=p.applet_id AND m.github_id=?1)
       ORDER BY p.applet_id LIMIT ?2",
      )
      .map_err(|_| StoreError::internal())?;
    let result = statement
      .query_map(params![user_id, MAX_PACKAGES], |row| {
        Ok(ManagedPackage {
          applet_id: row.get(0)?,
          role: if row.get::<_, bool>(1)? {
            PackageRole::Owner
          } else {
            PackageRole::Maintainer
          },
          published: row.get(2)?,
        })
      })
      .map_err(|_| StoreError::internal())?
      .collect::<std::result::Result<Vec<_>, _>>()
      .map_err(|_| StoreError::internal());
    result
  }

  pub fn package_details(&self, id: &str, actor_id: u64) -> Result<PackageDetails> {
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction()
      .map_err(|_| StoreError::internal())?;
    details(&transaction, id, actor_id)
  }

  pub fn register_package(&self, id: &str, actor: &User) -> Result<PackageDetails> {
    if !apell_package::valid_name(id) {
      return Err(StoreError::invalid("Choose a valid applet ID."));
    }
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    ensure_user(&transaction, actor.github_id)?;
    if transaction
      .query_row(
        "SELECT EXISTS(SELECT 1 FROM packages WHERE applet_id=?1)
        OR EXISTS(SELECT 1 FROM applets WHERE applet_id=?1)",
        [id],
        |row| row.get::<_, bool>(0),
      )
      .map_err(|_| StoreError::internal())?
    {
      return Err(conflict(
        "This applet ID is already registered or reserved.",
      ));
    }
    database::require_capacity(&transaction)?;
    transaction
      .execute(
        "INSERT INTO packages(applet_id,owner_github_id,status,policy_revision,created_at)
       VALUES(?1,?2,'active',0,?3)",
        params![id, actor.github_id, now()],
      )
      .map_err(|_| StoreError::internal())?;
    audit(
      &transaction,
      id,
      Some(actor.github_id),
      "package_registered",
      None,
    )?;
    let result = details(&transaction, id, actor.github_id)?;
    transaction.commit().map_err(|_| StoreError::internal())?;
    Ok(result)
  }

  pub fn remove_maintainer(&self, id: &str, actor_id: u64, target_id: u64) -> Result<()> {
    if target_id == 0 || target_id > i64::MAX as u64 {
      return Err(StoreError::invalid("Invalid GitHub user ID."));
    }
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    require_member(&transaction, id, actor_id)?;
    if package_owner(&transaction, id)? == Some(target_id) {
      return Err(conflict(
        "The owner cannot be removed. Transfer ownership instead.",
      ));
    }
    let removed = transaction
      .execute(
        "DELETE FROM package_maintainers WHERE applet_id=?1 AND github_id=?2",
        params![id, target_id],
      )
      .map_err(|_| StoreError::internal())?;
    if removed == 0 {
      return Err(StoreError::missing());
    }
    transaction
      .execute(
        "DELETE FROM package_invitations WHERE applet_id=?1
       AND (target_github_id=?2 OR invited_by=?2)",
        params![id, target_id],
      )
      .map_err(|_| StoreError::internal())?;
    audit(
      &transaction,
      id,
      Some(actor_id),
      "maintainer_removed",
      Some(target_id),
    )?;
    transaction.commit().map_err(|_| StoreError::internal())
  }

  /// Reject invalid authority before receiving an upload. Package membership is
  /// checked again with the parsed package ID in the publication transaction.
  pub fn authorize_publish(&self, authority: &PublishAuthority) -> Result<()> {
    let connection = self.connection()?;
    match authority {
      PublishAuthority::Administrator => Ok(()),
      PublishAuthority::User(user_id) => ensure_user(&connection, *user_id),
      PublishAuthority::Oidc { applet_id, .. } => {
        authorize_transaction(&connection, applet_id, authority)
      }
    }
  }
}

pub(crate) fn authorize_transaction(
  connection: &Connection,
  id: &str,
  authority: &PublishAuthority,
) -> Result<()> {
  match authority {
    PublishAuthority::Administrator => database::reserve_package(connection, id),
    PublishAuthority::User(user_id) => require_member(connection, id, *user_id).map(|_| ()),
    PublishAuthority::Oidc {
      applet_id,
      policy_revision,
    } => {
      if applet_id != id || *policy_revision > i64::MAX as u64 {
        return Err(forbidden());
      }
      let authorized = connection
        .query_row(
          "SELECT EXISTS(SELECT 1 FROM packages p JOIN package_publishers o
         ON o.applet_id=p.applet_id WHERE p.applet_id=?1 AND p.policy_revision=?2)",
          params![id, policy_revision],
          |row| row.get::<_, bool>(0),
        )
        .map_err(|_| StoreError::internal())?;
      if authorized {
        Ok(())
      } else {
        Err(forbidden())
      }
    }
  }
}

pub(crate) fn audit_publication(
  connection: &Connection,
  id: &str,
  authority: &PublishAuthority,
) -> Result<()> {
  let (actor, action) = match authority {
    PublishAuthority::Administrator => (None, "administrator_published"),
    PublishAuthority::User(user_id) => (Some(*user_id), "member_published"),
    PublishAuthority::Oidc { .. } => (None, "workflow_published"),
  };
  audit(connection, id, actor, action, None)
}

fn details(connection: &Connection, id: &str, actor_id: u64) -> Result<PackageDetails> {
  let role = require_member(connection, id, actor_id)?;
  let owner_id = package_owner(connection, id)?.ok_or_else(StoreError::internal)?;
  let owner = user(connection, owner_id)?.ok_or_else(StoreError::internal)?;
  let mut statement = connection
    .prepare(
      "SELECT u.github_id,u.github_login FROM users u JOIN package_maintainers m
     ON m.github_id=u.github_id WHERE m.applet_id=?1 ORDER BY u.github_login,u.github_id",
    )
    .map_err(|_| StoreError::internal())?;
  let maintainers = statement
    .query_map([id], |row| {
      Ok(User {
        github_id: row.get(0)?,
        github_login: row.get(1)?,
      })
    })
    .map_err(|_| StoreError::internal())?
    .collect::<std::result::Result<Vec<_>, _>>()
    .map_err(|_| StoreError::internal())?;
  let publisher = publishers::read_publisher(connection, id)?.map(|(policy, _)| policy);
  Ok(PackageDetails {
    applet_id: id.to_owned(),
    role,
    owner,
    maintainers,
    publisher,
    invitations: invitations::for_package(connection, id)?,
  })
}

fn now() -> u64 {
  crate::oidc::now()
}

fn forbidden() -> StoreError {
  StoreError::new(
    StatusCode::FORBIDDEN,
    "forbidden",
    "You cannot manage or publish this package.",
  )
}

fn conflict(message: &str) -> StoreError {
  StoreError::new(StatusCode::CONFLICT, "package_conflict", message)
}
