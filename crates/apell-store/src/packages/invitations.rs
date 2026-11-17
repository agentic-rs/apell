use super::{
  conflict,
  database::{audit, ensure_user, package_owner, require_member},
  forbidden, now, Invitation, InvitationKind, PackageRole, INVITATION_LIFETIME, MAX_INVITATIONS,
  MAX_MAINTAINERS,
};
use crate::{auth::User, Result, Store, StoreError};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

const INVITATION_COLUMNS: &str = "i.id,i.applet_id,i.kind,t.github_id,t.github_login,
  a.github_id,a.github_login,i.expires_at";

impl Store {
  pub fn invite_maintainer(&self, id: &str, actor_id: u64, target: &User) -> Result<Invitation> {
    self.create_invitation(id, actor_id, target.github_id, InvitationKind::Maintainer)
  }

  pub fn invite_transfer(&self, id: &str, actor_id: u64, target_id: u64) -> Result<Invitation> {
    self.create_invitation(id, actor_id, target_id, InvitationKind::Ownership)
  }

  fn create_invitation(
    &self,
    id: &str,
    actor_id: u64,
    target_id: u64,
    kind: InvitationKind,
  ) -> Result<Invitation> {
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    let role = require_member(&transaction, id, actor_id)?;
    ensure_user(&transaction, target_id)?;
    if target_id == actor_id || package_owner(&transaction, id)? == Some(target_id) {
      return Err(conflict("This user already has access to the package."));
    }
    let target_is_member = transaction
      .query_row(
        "SELECT EXISTS(SELECT 1 FROM package_maintainers WHERE applet_id=?1 AND github_id=?2)",
        params![id, target_id],
        |row| row.get::<_, bool>(0),
      )
      .map_err(|_| StoreError::internal())?;
    match kind {
      InvitationKind::Maintainer => {
        if target_is_member {
          return Err(conflict("This user is already a maintainer."));
        }
        require_member_capacity(&transaction, id)?;
      }
      InvitationKind::Ownership => {
        if role != PackageRole::Owner {
          return Err(forbidden());
        }
        if !target_is_member {
          return Err(conflict("Transfer ownership to an existing maintainer."));
        }
      }
    }
    transaction
      .execute(
        "DELETE FROM package_invitations WHERE expires_at<=?1",
        [now()],
      )
      .map_err(|_| StoreError::internal())?;
    let existing = transaction
      .query_row(
        "SELECT id FROM package_invitations WHERE applet_id=?1 AND kind=?2 AND target_github_id=?3",
        params![id, kind.as_str(), target_id],
        |row| row.get::<_, String>(0),
      )
      .optional()
      .map_err(|_| StoreError::internal())?;
    if let Some(existing) = existing {
      return read_invitation(&transaction, &existing)?.ok_or_else(StoreError::internal);
    }
    let count: i64 = transaction
      .query_row(
        "SELECT COUNT(*) FROM package_invitations WHERE applet_id=?1",
        [id],
        |row| row.get(0),
      )
      .map_err(|_| StoreError::internal())?;
    if count >= MAX_INVITATIONS {
      return Err(conflict("This package has too many pending invitations."));
    }
    if kind == InvitationKind::Ownership {
      // Only one ownership transfer may be pending for a package.
      transaction
        .execute(
          "DELETE FROM package_invitations WHERE applet_id=?1 AND kind='ownership'",
          [id],
        )
        .map_err(|_| StoreError::internal())?;
    }
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random).map_err(|_| StoreError::internal())?;
    let invitation_id = apell_package::digest(&random);
    transaction
      .execute(
        "INSERT INTO package_invitations(id,applet_id,kind,target_github_id,invited_by,expires_at)
       VALUES(?1,?2,?3,?4,?5,?6)",
        params![
          invitation_id,
          id,
          kind.as_str(),
          target_id,
          actor_id,
          now() + INVITATION_LIFETIME
        ],
      )
      .map_err(|_| StoreError::internal())?;
    audit(
      &transaction,
      id,
      Some(actor_id),
      match kind {
        InvitationKind::Maintainer => "maintainer_invited",
        InvitationKind::Ownership => "ownership_invited",
      },
      Some(target_id),
    )?;
    let result = read_invitation(&transaction, &invitation_id)?.ok_or_else(StoreError::internal)?;
    transaction.commit().map_err(|_| StoreError::internal())?;
    Ok(result)
  }

  pub fn invitations_for(&self, user_id: u64) -> Result<Vec<Invitation>> {
    let connection = self.connection()?;
    ensure_user(&connection, user_id)?;
    let mut statement = connection
      .prepare(&format!(
        "SELECT {INVITATION_COLUMNS} FROM package_invitations i
       JOIN users t ON t.github_id=i.target_github_id JOIN users a ON a.github_id=i.invited_by
       WHERE i.target_github_id=?1 AND i.expires_at>?2 ORDER BY i.applet_id,i.kind",
      ))
      .map_err(|_| StoreError::internal())?;
    let result = statement
      .query_map(params![user_id, now()], read_row)
      .map_err(|_| StoreError::internal())?
      .collect::<std::result::Result<Vec<_>, _>>()
      .map_err(|_| StoreError::internal());
    result
  }

  pub fn cancel_invitation(&self, id: &str, actor_id: u64, invitation_id: &str) -> Result<()> {
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    require_member(&transaction, id, actor_id)?;
    let invitation =
      read_invitation(&transaction, invitation_id)?.ok_or_else(StoreError::missing)?;
    if invitation.applet_id != id {
      return Err(StoreError::missing());
    }
    transaction
      .execute(
        "DELETE FROM package_invitations WHERE id=?1",
        [invitation_id],
      )
      .map_err(|_| StoreError::internal())?;
    audit(
      &transaction,
      id,
      Some(actor_id),
      "invitation_cancelled",
      Some(invitation.target.github_id),
    )?;
    transaction.commit().map_err(|_| StoreError::internal())
  }

  pub fn decline_invitation(&self, invitation_id: &str, actor_id: u64) -> Result<()> {
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    ensure_user(&transaction, actor_id)?;
    let invitation =
      read_invitation(&transaction, invitation_id)?.ok_or_else(StoreError::missing)?;
    if invitation.target.github_id != actor_id {
      return Err(forbidden());
    }
    transaction
      .execute(
        "DELETE FROM package_invitations WHERE id=?1",
        [invitation_id],
      )
      .map_err(|_| StoreError::internal())?;
    audit(
      &transaction,
      &invitation.applet_id,
      Some(actor_id),
      "invitation_declined",
      None,
    )?;
    transaction.commit().map_err(|_| StoreError::internal())
  }

  pub fn accept_invitation(&self, invitation_id: &str, actor_id: u64) -> Result<()> {
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    ensure_user(&transaction, actor_id)?;
    let invitation =
      read_invitation(&transaction, invitation_id)?.ok_or_else(StoreError::missing)?;
    if invitation.target.github_id != actor_id {
      return Err(forbidden());
    }
    let id = &invitation.applet_id;
    let inviter_role = require_member(&transaction, id, invitation.invited_by.github_id)?;
    match invitation.kind {
      InvitationKind::Maintainer => {
        if package_owner(&transaction, id)? == Some(actor_id) {
          return Err(conflict("This user is already the package owner."));
        }
        require_member_capacity(&transaction, id)?;
        transaction.execute(
          "INSERT INTO package_maintainers(applet_id,github_id,added_by,added_at) VALUES(?1,?2,?3,?4)",
          params![id, actor_id, invitation.invited_by.github_id, now()],
        ).map_err(|_| conflict("This user is already a maintainer."))?;
        audit(
          &transaction,
          id,
          Some(actor_id),
          "maintainer_joined",
          Some(invitation.invited_by.github_id),
        )?;
      }
      InvitationKind::Ownership => {
        if inviter_role != PackageRole::Owner {
          return Err(forbidden());
        }
        if require_member(&transaction, id, actor_id)? != PackageRole::Maintainer {
          return Err(conflict("The new owner must still be a maintainer."));
        }
        let old_owner = invitation.invited_by.github_id;
        transaction
          .execute(
            "DELETE FROM package_maintainers WHERE applet_id=?1 AND github_id=?2",
            params![id, actor_id],
          )
          .map_err(|_| StoreError::internal())?;
        transaction.execute(
          "UPDATE packages SET owner_github_id=?2,policy_revision=policy_revision+1 WHERE applet_id=?1",
          params![id, actor_id],
        ).map_err(|_| StoreError::internal())?;
        transaction.execute(
          "INSERT INTO package_maintainers(applet_id,github_id,added_by,added_at) VALUES(?1,?2,?3,?4)",
          params![id, old_owner, actor_id, now()],
        ).map_err(|_| StoreError::internal())?;
        transaction
          .execute(
            "DELETE FROM package_invitations WHERE applet_id=?1 AND kind='ownership'",
            [id],
          )
          .map_err(|_| StoreError::internal())?;
        audit(
          &transaction,
          id,
          Some(actor_id),
          "ownership_transferred",
          Some(old_owner),
        )?;
      }
    }
    transaction
      .execute(
        "DELETE FROM package_invitations WHERE id=?1",
        [invitation_id],
      )
      .map_err(|_| StoreError::internal())?;
    transaction.commit().map_err(|_| StoreError::internal())
  }
}

pub(super) fn for_package(connection: &Connection, id: &str) -> Result<Vec<Invitation>> {
  let mut statement = connection
    .prepare(&format!(
      "SELECT {INVITATION_COLUMNS} FROM package_invitations i
     JOIN users t ON t.github_id=i.target_github_id JOIN users a ON a.github_id=i.invited_by
     WHERE i.applet_id=?1 AND i.expires_at>?2 ORDER BY i.kind,t.github_login",
    ))
    .map_err(|_| StoreError::internal())?;
  let result = statement
    .query_map(params![id, now()], read_row)
    .map_err(|_| StoreError::internal())?
    .collect::<std::result::Result<Vec<_>, _>>()
    .map_err(|_| StoreError::internal());
  result
}

fn read_invitation(connection: &Connection, id: &str) -> Result<Option<Invitation>> {
  if !apell_package::valid_hash(id) || id.bytes().any(|byte| byte.is_ascii_uppercase()) {
    return Ok(None);
  }
  connection
    .query_row(
      &format!(
        "SELECT {INVITATION_COLUMNS} FROM package_invitations i
     JOIN users t ON t.github_id=i.target_github_id JOIN users a ON a.github_id=i.invited_by
     WHERE i.id=?1 AND i.expires_at>?2",
      ),
      params![id, now()],
      read_row,
    )
    .optional()
    .map_err(|_| StoreError::internal())
}

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Invitation> {
  let kind = match row.get::<_, String>(2)?.as_str() {
    "maintainer" => InvitationKind::Maintainer,
    "ownership" => InvitationKind::Ownership,
    _ => return Err(rusqlite::Error::InvalidQuery),
  };
  Ok(Invitation {
    id: row.get(0)?,
    applet_id: row.get(1)?,
    kind,
    target: User {
      github_id: row.get(3)?,
      github_login: row.get(4)?,
    },
    invited_by: User {
      github_id: row.get(5)?,
      github_login: row.get(6)?,
    },
    expires_at: row.get(7)?,
  })
}

fn require_member_capacity(connection: &Connection, id: &str) -> Result<()> {
  let count: i64 = connection
    .query_row(
      "SELECT COUNT(*) FROM package_maintainers WHERE applet_id=?1",
      [id],
      |row| row.get(0),
    )
    .map_err(|_| StoreError::internal())?;
  if count >= MAX_MAINTAINERS {
    return Err(conflict("This package has reached its maintainer limit."));
  }
  Ok(())
}
