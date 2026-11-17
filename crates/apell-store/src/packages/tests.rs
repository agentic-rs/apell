use super::*;
use std::{
  io::{Cursor, Write},
  sync::{Arc, Barrier},
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

fn fixture() -> (tempfile::TempDir, Store, [User; 4]) {
  let directory = tempfile::tempdir().unwrap();
  let store = Store::open(directory.path()).unwrap();
  let users = [101, 102, 103, 104].map(|github_id| User {
    github_id,
    github_login: format!("fixture-{github_id}"),
  });
  for user in &users {
    store.upsert_user(user).unwrap();
  }
  (directory, store, users)
}

fn publisher(id: &str) -> TrustedPublisher {
  TrustedPublisher {
    applet_id: id.into(),
    repository_id: 1234,
    repository_owner_id: 5678,
    repository: Some("example/project".into()),
    workflow: ".github/workflows/publish.yml".into(),
    allowed_refs: vec!["refs/heads/main".into()],
    event_names: vec!["push".into()],
    environment: None,
  }
}

fn package(id: &str, version: &str) -> Vec<u8> {
  let manifest = serde_json::to_vec(&serde_json::json!({
    "schema_version":1,"api_version":2,"applet_id":id,"name":"Fixture",
    "version":version,"permissions":[],"network":false,
  }))
  .unwrap();
  let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
  for (path, content) in [
    ("manifest.json", manifest.as_slice()),
    ("index.html", b"<p>Fixture</p>".as_slice()),
  ] {
    archive
      .start_file(
        path,
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
      )
      .unwrap();
    archive.write_all(content).unwrap();
  }
  archive.finish().unwrap().into_inner()
}

fn metadata() -> crate::PublishMetadata {
  crate::PublishMetadata {
    description: "A fixture applet.".into(),
    project_url: None,
    release_notes: String::new(),
  }
}

fn add_maintainer(store: &Store, id: &str, owner: &User, maintainer: &User) {
  let invitation = store
    .invite_maintainer(id, owner.github_id, maintainer)
    .unwrap();
  store
    .accept_invitation(&invitation.id, maintainer.github_id)
    .unwrap();
}

#[test]
fn registration_reserves_identity_without_creating_a_catalog_entry() {
  let (_directory, store, [owner, stranger, ..]) = fixture();
  let created = store.register_package("sample", &owner).unwrap();
  assert_eq!(created.owner, owner);
  assert_eq!(created.role, PackageRole::Owner);
  assert!(store.catalog().unwrap().applets.is_empty());
  assert!(!store.managed_packages(owner.github_id).unwrap()[0].published);
  assert!(store.register_package("sample", &stranger).is_err());
  assert!(store.package_details("sample", stranger.github_id).is_err());
  assert!(store.register_package("invalid/name", &owner).is_err());
  assert!(store
    .register_package(
      "unverified",
      &User {
        github_id: 999,
        github_login: "unknown".into()
      }
    )
    .is_err());
  store
    .publish_authorized(
      package("sample", "1.0.0"),
      metadata(),
      None,
      PublishAuthority::User(owner.github_id),
    )
    .unwrap();
  assert!(store.managed_packages(owner.github_id).unwrap()[0].published);
  assert_eq!(store.catalog().unwrap().applets.len(), 1);
}

#[test]
fn concurrent_registration_assigns_exactly_one_owner() {
  let (_directory, store, users) = fixture();
  let barrier = Arc::new(Barrier::new(2));
  let results: Vec<_> = users[..2]
    .iter()
    .cloned()
    .map(|user| {
      let store = store.clone();
      let barrier = barrier.clone();
      std::thread::spawn(move || {
        barrier.wait();
        let accepted = store.register_package("contested", &user).is_ok();
        (user, accepted)
      })
    })
    .collect::<Vec<_>>()
    .into_iter()
    .map(|worker| worker.join().unwrap())
    .collect();
  assert_eq!(results.iter().filter(|(_, accepted)| *accepted).count(), 1);
  let owner = &results.iter().find(|(_, accepted)| *accepted).unwrap().0;
  assert_eq!(
    store
      .package_details("contested", owner.github_id)
      .unwrap()
      .owner,
    *owner
  );
}

#[test]
fn legacy_published_ids_and_configured_ids_cannot_be_claimed() {
  let (directory, store, [owner, stranger, ..]) = fixture();
  let bytes = package("published", "1.0.0");
  let published = store.publish(bytes.clone(), metadata(), None).unwrap();
  store
    .bootstrap_packages(&[publisher("pending")], None)
    .unwrap();
  assert!(store.register_package("published", &stranger).is_err());
  assert!(store.register_package("pending", &stranger).is_err());
  assert!(store.managed_packages(owner.github_id).unwrap().is_empty());
  let reopened = Store::open(directory.path()).unwrap();
  reopened
    .bootstrap_packages(&[publisher("pending")], Some(&owner))
    .unwrap();
  assert_eq!(reopened.managed_packages(owner.github_id).unwrap().len(), 2);
  assert_eq!(
    reopened.detail("published").unwrap().releases[0].sha256,
    published.sha256
  );
  assert_eq!(apell_package::digest(&bytes), published.sha256);
}

#[test]
fn migration_reserves_catalog_rows_from_before_accounts_existed() {
  let directory = tempfile::tempdir().unwrap();
  let connection = rusqlite::Connection::open(directory.path().join("catalog.sqlite3")).unwrap();
  connection.execute_batch(
    "CREATE TABLE applets(applet_id TEXT PRIMARY KEY,latest_version TEXT NOT NULL,stable_version TEXT);
     INSERT INTO applets VALUES('legacy','1.0.0','1.0.0');",
  ).unwrap();
  drop(connection);
  let store = Store::open(directory.path()).unwrap();
  let user = User {
    github_id: 111,
    github_login: "fixture-owner".into(),
  };
  store.upsert_user(&user).unwrap();
  assert!(store.register_package("legacy", &user).is_err());
  assert!(store.managed_packages(user.github_id).unwrap().is_empty());
  store.bootstrap_packages(&[], Some(&user)).unwrap();
  assert_eq!(
    store
      .package_details("legacy", user.github_id)
      .unwrap()
      .owner,
    user
  );
}

#[test]
fn invitations_require_target_acceptance_and_maintainers_manage_people() {
  let (_directory, store, [owner, maintainer, colleague, stranger]) = fixture();
  store.register_package("sample", &owner).unwrap();
  let invite = store
    .invite_maintainer("sample", owner.github_id, &maintainer)
    .unwrap();
  assert!(store
    .package_details("sample", maintainer.github_id)
    .is_err());
  assert_eq!(
    store.invitations_for(maintainer.github_id).unwrap().len(),
    1
  );
  assert!(store
    .accept_invitation(&invite.id, stranger.github_id)
    .is_err());
  store
    .accept_invitation(&invite.id, maintainer.github_id)
    .unwrap();
  assert!(store
    .accept_invitation(&invite.id, maintainer.github_id)
    .is_err());
  assert_eq!(
    store
      .package_details("sample", maintainer.github_id)
      .unwrap()
      .role,
    PackageRole::Maintainer
  );
  add_maintainer(&store, "sample", &maintainer, &colleague);
  store
    .set_publisher("sample", maintainer.github_id, publisher("sample"))
    .unwrap();
  store
    .remove_maintainer("sample", colleague.github_id, maintainer.github_id)
    .unwrap();
  assert!(store
    .package_details("sample", maintainer.github_id)
    .is_err());
  assert!(store
    .remove_maintainer("sample", colleague.github_id, owner.github_id)
    .is_err());
  assert!(store
    .invite_maintainer("sample", stranger.github_id, &maintainer)
    .is_err());
}

#[test]
fn revoked_members_and_invites_cannot_authorize_later_writes() {
  let (directory, store, [owner, maintainer, target, ..]) = fixture();
  store.register_package("sample", &owner).unwrap();
  add_maintainer(&store, "sample", &owner, &maintainer);
  let invite = store
    .invite_maintainer("sample", maintainer.github_id, &target)
    .unwrap();
  let authority = PublishAuthority::User(maintainer.github_id);
  store.authorize_publish(&authority).unwrap();
  store
    .remove_maintainer("sample", owner.github_id, maintainer.github_id)
    .unwrap();
  assert!(store
    .accept_invitation(&invite.id, target.github_id)
    .is_err());
  assert!(store
    .publish_authorized(package("sample", "1.0.0"), metadata(), None, authority)
    .is_err());
  assert!(store.catalog().unwrap().applets.is_empty());
  assert_eq!(
    std::fs::read_dir(directory.path().join("packages"))
      .unwrap()
      .count(),
    0
  );
  let invite = store
    .invite_maintainer("sample", owner.github_id, &target)
    .unwrap();
  store
    .connection()
    .unwrap()
    .execute(
      "UPDATE package_invitations SET expires_at=0 WHERE id=?1",
      [&invite.id],
    )
    .unwrap();
  assert!(store
    .accept_invitation(&invite.id, target.github_id)
    .is_err());
}

#[test]
fn ownership_transfer_is_consensual_and_does_not_remove_the_former_owner() {
  let (_directory, store, [owner, maintainer, other, ..]) = fixture();
  store.register_package("sample", &owner).unwrap();
  add_maintainer(&store, "sample", &owner, &maintainer);
  assert!(store
    .invite_transfer("sample", maintainer.github_id, owner.github_id)
    .is_err());
  assert!(store
    .invite_transfer("sample", owner.github_id, other.github_id)
    .is_err());
  let transfer = store
    .invite_transfer("sample", owner.github_id, maintainer.github_id)
    .unwrap();
  assert_eq!(
    store
      .package_details("sample", owner.github_id)
      .unwrap()
      .owner,
    owner
  );
  assert!(store
    .accept_invitation(&transfer.id, other.github_id)
    .is_err());
  store
    .accept_invitation(&transfer.id, maintainer.github_id)
    .unwrap();
  let detail = store.package_details("sample", owner.github_id).unwrap();
  assert_eq!(detail.role, PackageRole::Maintainer);
  assert_eq!(detail.owner, maintainer);
  assert_eq!(detail.maintainers, vec![owner.clone()]);
  store.bootstrap_packages(&[], Some(&owner)).unwrap();
  assert_eq!(
    store
      .package_details("sample", owner.github_id)
      .unwrap()
      .owner,
    maintainer
  );
  assert!(store
    .accept_invitation(&transfer.id, maintainer.github_id)
    .is_err());
}

#[test]
fn changed_or_disconnected_policies_revoke_existing_upload_authority() {
  let (_directory, store, [owner, stranger, ..]) = fixture();
  store.register_package("sample", &owner).unwrap();
  store
    .set_publisher("sample", owner.github_id, publisher("sample"))
    .unwrap();
  let (_, revision) = store.publisher_for("sample").unwrap().unwrap();
  let old = PublishAuthority::Oidc {
    applet_id: "sample".into(),
    policy_revision: revision,
  };
  store.authorize_publish(&old).unwrap();
  let mut changed = publisher("sample");
  changed.repository_id += 1;
  store
    .set_publisher("sample", owner.github_id, changed)
    .unwrap();
  assert!(store.authorize_publish(&old).is_err());
  assert!(store
    .publish_authorized(package("sample", "1.0.0"), metadata(), None, old)
    .is_err());
  let (_, revision) = store.publisher_for("sample").unwrap().unwrap();
  let current = PublishAuthority::Oidc {
    applet_id: "sample".into(),
    policy_revision: revision,
  };
  assert!(store
    .publish_authorized(package("other", "1.0.0"), metadata(), None, current.clone())
    .is_err());
  assert!(store
    .disconnect_publisher("sample", stranger.github_id)
    .is_err());
  store.authorize_publish(&current).unwrap();
  store
    .disconnect_publisher("sample", owner.github_id)
    .unwrap();
  assert!(store.authorize_publish(&current).is_err());
  assert!(store
    .publish_authorized(package("sample", "1.0.0"), metadata(), None, current)
    .is_err());
  assert!(store.catalog().unwrap().applets.is_empty());
}

#[test]
fn bootstrap_does_not_restore_disconnected_or_changed_policies() {
  let (directory, store, [owner, ..]) = fixture();
  let original = publisher("sample");
  store
    .bootstrap_packages(std::slice::from_ref(&original), Some(&owner))
    .unwrap();
  assert_eq!(store.all_publishers().unwrap().len(), 1);
  store
    .disconnect_publisher("sample", owner.github_id)
    .unwrap();
  let reopened = Store::open(directory.path()).unwrap();
  reopened
    .bootstrap_packages(std::slice::from_ref(&original), Some(&owner))
    .unwrap();
  assert!(reopened.publisher_for("sample").unwrap().is_none());
  let mut changed = original.clone();
  changed.repository_id = 3333;
  reopened
    .set_publisher("sample", owner.github_id, changed.clone())
    .unwrap();
  reopened
    .bootstrap_packages(&[original], Some(&owner))
    .unwrap();
  assert_eq!(
    reopened.publisher_for("sample").unwrap().unwrap().0,
    changed
  );
}

#[test]
fn cancelling_and_declining_invitations_preserves_membership() {
  let (_directory, store, [owner, target, other, ..]) = fixture();
  store.register_package("sample", &owner).unwrap();
  let invite = store
    .invite_maintainer("sample", owner.github_id, &target)
    .unwrap();
  assert!(store
    .decline_invitation(&invite.id, other.github_id)
    .is_err());
  store
    .decline_invitation(&invite.id, target.github_id)
    .unwrap();
  assert!(store
    .accept_invitation(&invite.id, target.github_id)
    .is_err());
  let invite = store
    .invite_maintainer("sample", owner.github_id, &target)
    .unwrap();
  store
    .cancel_invitation("sample", owner.github_id, &invite.id)
    .unwrap();
  assert!(store
    .accept_invitation(&invite.id, target.github_id)
    .is_err());
  assert!(store.invitations_for(target.github_id).unwrap().is_empty());
  assert!(store
    .package_details("sample", owner.github_id)
    .unwrap()
    .maintainers
    .is_empty());
}
