use super::*;
use crate::account::Access;
use std::{collections::btree_map, sync::Weak};

const STALE_REVIEW: &str = "The API key conflict changed. Refresh the review and choose again.";
const TRACKING_WARNING: &str =
  "Your choice was saved. Apell could not update the local sync record and will check again.";
const CONCURRENT_EDIT_WARNING: &str =
  "Your choice was saved in the cloud. The device copy changed during the update. Review the latest copies again.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::key_sync) struct ConflictResolution {
  connection_id: String,
  review_id: Uuid,
  resolution: Choice,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Choice {
  KeepDevice,
  UseCloud,
}

type Session = Weak<Mutex<()>>;

#[derive(Clone)]
struct Stamp {
  session: Session,
  owner: Owner,
  revision: Option<Uuid>,
  connection: Connection,
  tracked: Tracked,
  device_hash: Option<String>,
  cloud_hash: Option<String>,
}

impl Stamp {
  fn matches(&self, other: &Self) -> bool {
    self.matches_head(&other.session, &other.owner, other.revision)
      && self.connection == other.connection
      && self.tracked == other.tracked
      && self.device_hash == other.device_hash
      && self.cloud_hash == other.cloud_hash
  }

  fn matches_head(&self, session: &Session, owner: &Owner, revision: Option<Uuid>) -> bool {
    self.session.ptr_eq(session) && &self.owner == owner && self.revision == revision
  }

  fn local_is_current(
    &self,
    settings: &Settings,
    device: &Device,
    vault: &Secret,
    read: impl FnOnce(&Connection) -> Result<Option<Zeroizing<String>>, String>,
  ) -> Result<bool, String> {
    let id = &self.connection.connection_id;
    if settings.connection(id).ok() != Some(&self.connection)
      || device.connections.get(id) != Some(&self.tracked)
    {
      return Ok(false);
    }
    let keys = VaultKeys::new(&self.owner, vault);
    let aad = keys.context(&self.connection)?;
    let hash = read(&self.connection)?
      .map(|key| current_fingerprint(&keys.fingerprint, &aad, key.as_bytes()));
    Ok(hash == self.device_hash)
  }
}

struct Reviewed {
  id: Uuid,
  stamp: Stamp,
}

#[derive(Default)]
pub(super) struct Reviews(BTreeMap<String, Reviewed>);

impl Reviews {
  pub(super) fn clear(&mut self) {
    self.0.clear();
  }

  fn replace(&mut self, stamps: Vec<Stamp>) -> Value {
    let previous = std::mem::take(&mut self.0);
    let mut rows = Vec::with_capacity(stamps.len());
    for stamp in stamps {
      let connection = &stamp.connection;
      // Preserve handles on a refresh when the user would see the same review.
      let id = previous
        .get(&connection.connection_id)
        .filter(|current| current.stamp.matches(&stamp))
        .map_or_else(Uuid::new_v4, |current| current.id);
      rows.push(json!({
        "connection_id": connection.connection_id,
        "review_id": id,
        "name": connection.name,
        "provider_id": connection.provider_id,
        "model_id": connection.model_id,
        "device_has_key": stamp.device_hash.is_some(),
        "cloud_has_key": stamp.cloud_hash.is_some(),
      }));
      self
        .0
        .insert(connection.connection_id.clone(), Reviewed { id, stamp });
    }
    Value::Array(rows)
  }

  fn take(&mut self, input: &ConflictResolution) -> Result<Stamp, String> {
    match self.0.entry(input.connection_id.clone()) {
      btree_map::Entry::Occupied(review) if review.get().id == input.review_id => {
        Ok(review.remove().stamp)
      }
      _ => Err(STALE_REVIEW.into()),
    }
  }
}

struct ReviewState {
  session: Session,
  owner: Owner,
  vault: Secret,
  revision: Option<Uuid>,
  entries: BTreeMap<String, Entry>,
}

impl ReviewState {
  async fn load(
    app: &AppHandle,
    host: &Host,
    stop: &BackgroundStop,
  ) -> Result<(Access, Self), String> {
    // Load a saved session on the first call after a shell restart.
    let (account, _) = stop
      .session(app, host)
      .await?
      .ok_or("Sign in to Apell Account to sync encrypted API keys.")?;
    let relay = stop.access(app, host, true, None).await?;
    if relay.account_key != account {
      return Err(OWNERSHIP_CHANGED.into());
    }
    let (vault, access, head) = cloud_context(app, host, &relay, stop).await?;
    let state = Self {
      session: access.session_marker(),
      owner: Owner {
        account,
        vault_id: vault.vault_id,
      },
      vault: vault.key()?,
      revision: head.as_ref().map(|head| head.revision_id),
      entries: head
        .into_iter()
        .flat_map(|head| head.entries)
        .map(|entry| (entry.connection_id.clone(), entry))
        .collect(),
    };
    Ok((access, state))
  }

  fn conflict(
    &self,
    device: &Device,
    connection: &Connection,
    keys: &VaultKeys<'_>,
    read: impl FnOnce(&Connection) -> Result<Option<Zeroizing<String>>, String>,
  ) -> Result<Option<(Stamp, KeyCopies)>, String> {
    let id = &connection.connection_id;
    let Some(tracked) = device
      .connections
      .get(id)
      .filter(|tracked| tracked.belongs_to(&self.owner))
      .filter(|_| eligible_endpoint(connection).is_some())
    else {
      return Ok(None);
    };
    let copies = keys.inspect(
      connection,
      read(connection)?,
      self.entries.get(id),
      tracked.common.as_deref(),
    )?;
    Ok((copies.merge == Merge::Conflict).then(|| {
      let stamp = Stamp {
        session: self.session.clone(),
        owner: self.owner.clone(),
        revision: self.revision,
        connection: connection.clone(),
        tracked: tracked.clone(),
        device_hash: copies.device_hash.clone(),
        cloud_hash: copies.cloud_hash.clone(),
      };
      (stamp, copies)
    }))
  }

  fn prepare(
    mut self,
    root: &Path,
    settings: &Settings,
    reviewed: &Stamp,
    choice: Choice,
    read: impl FnOnce(&Connection) -> Result<Option<Zeroizing<String>>, String>,
  ) -> Result<Prepared, String> {
    let device = Device::load(root)?;
    let id = &reviewed.connection.connection_id;
    let connection = settings.connection(id).map_err(|_| STALE_REVIEW)?;
    let keys = VaultKeys::new(&self.owner, &self.vault);
    let Some((current, copies)) = self
      .conflict(&device, connection, &keys, read)
      .map_err(|_| STALE_REVIEW)?
      .filter(|(current, _)| reviewed.matches(current))
    else {
      return Err(STALE_REVIEW.into());
    };
    match choice {
      Choice::KeepDevice => {
        set_cloud_entry(&mut self.entries, &keys, id, copies)?;
        Ok(Prepared::KeepDevice {
          entries: self.entries.into_values().collect(),
          vault: self.vault,
        })
      }
      Choice::UseCloud => Ok(Prepared::UseCloud {
        slot: connection.credential.account.clone(),
        value: copies
          .cloud
          .as_ref()
          .map(|key| encode_key(connection, key))
          .transpose()?,
        device,
        common: current.cloud_hash,
      }),
    }
  }
}

enum Prepared {
  KeepDevice {
    entries: Vec<Entry>,
    vault: Secret,
  },
  UseCloud {
    slot: String,
    value: Option<String>,
    device: Device,
    common: Option<String>,
  },
}

fn set_cloud_entry(
  entries: &mut BTreeMap<String, Entry>,
  keys: &VaultKeys<'_>,
  id: &str,
  copies: KeyCopies,
) -> Result<(), String> {
  let Some(key) = copies.device else {
    entries.remove(id);
    return Ok(());
  };
  if !entries.contains_key(id) && entries.len() >= MAX_CONNECTIONS {
    return Err("The encrypted API key limit was reached.".into());
  }
  entries.insert(
    id.to_owned(),
    keys.seal_entry(id, &copies.aad, key.as_bytes())?,
  );
  Ok(())
}

pub(in crate::key_sync) async fn review_conflicts(
  app: &AppHandle,
  host: &Host,
  stop: &BackgroundStop,
) -> Result<Value, String> {
  let _sync = stop.wait(host.key_sync.gate.lock()).await?;
  let (access, state) = ReviewState::load(app, host, stop).await?;
  let _credentials = stop.wait(host.credential_gate.lock()).await?;
  let settings = stop.wait(host.settings.lock()).await?.clone();
  let work_app = app.clone();
  let root = host.root.clone();
  let stamps = blocking(move || {
    let device = Device::load(&root)?;
    let keys = VaultKeys::new(&state.owner, &state.vault);
    Ok(
      settings
        .connections
        .iter()
        .filter_map(|connection| {
          state
            .conflict(&device, connection, &keys, |connection| {
              local_key(&work_app, connection)
            })
            .ok()
            .flatten()
            .map(|(stamp, _)| stamp)
        })
        .collect(),
    )
  })
  .await?;
  host.account.require_current_access(&access).await?;
  stop.check()?;
  Ok(host.key_sync.automatic.reviews.lock().await.replace(stamps))
}

pub(in crate::key_sync) async fn resolve_conflict(
  app: &AppHandle,
  host: &Host,
  input: ConflictResolution,
  stop: &BackgroundStop,
) -> Result<Value, String> {
  let _sync = stop.wait(host.key_sync.gate.lock()).await?;
  // A failed reply can leave the remote outcome unknown. Read a new review
  // before allowing another attempt with the same connection.
  let reviewed = host.key_sync.automatic.reviews.lock().await.take(&input)?;
  let (access, state) = ReviewState::load(app, host, stop).await?;
  if !reviewed.matches_head(&state.session, &state.owner, state.revision) {
    return Err(STALE_REVIEW.into());
  }

  // The key sync gate serializes other cloud operations. The credential gate
  // serializes fresh local verification and local writes with Settings edits.
  let credentials = stop.wait(host.credential_gate.lock()).await?;
  let settings = stop.wait(host.settings.lock()).await?.clone();
  let work_app = app.clone();
  let root = host.root.clone();
  let reviewed_for_work = reviewed.clone();
  let settings_for_work = settings.clone();
  let prepared = blocking(move || {
    state.prepare(
      &root,
      &settings_for_work,
      &reviewed_for_work,
      input.resolution,
      |connection| local_key(&work_app, connection),
    )
  })
  .await?;
  host.account.require_current_access(&access).await?;
  stop.check()?;
  let warning = match prepared {
    Prepared::KeepDevice { entries, vault } => {
      drop(credentials);
      stop
        .wait(save_head(
          host,
          &access,
          reviewed.owner.vault_id,
          reviewed.revision,
          entries,
        ))
        .await
        .map_err(|_| {
          "Apell could not confirm your cloud choice. Refresh the review before choosing again."
        })??;
      checkpoint_device_choice(app, host, &access, stop, reviewed, vault)
        .await
        .unwrap_or(Some(TRACKING_WARNING))
    }
    Prepared::UseCloud {
      slot,
      value,
      mut device,
      common,
    } => {
      let work_app = app.clone();
      let root = host.root.clone();
      let warning = blocking(move || {
        commit_settings(&work_app, &root, &settings, vec![(slot, value)])?;
        Ok(save_checkpoint(&root, &mut device, &reviewed, common))
      })
      .await?;
      drop(credentials);
      let _ = app.emit_to("main", "apell-llm-settings-changed", ());
      crate::config_sync::kick(app);
      warning
    }
  };
  kick(app);
  Ok(json!({"warning": warning}))
}

async fn checkpoint_device_choice(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  stop: &BackgroundStop,
  reviewed: Stamp,
  vault: Secret,
) -> Result<Option<&'static str>, String> {
  let _credentials = stop.wait(host.credential_gate.lock()).await?;
  let settings = stop.wait(host.settings.lock()).await?.clone();
  host.account.require_current_access(access).await?;
  stop.check()?;
  let app = app.clone();
  let root = host.root.clone();
  blocking(move || {
    let mut device = Device::load(&root)?;
    // The CAS made a new cloud revision. Keep the new baseline only if the
    // local side still matches the device copy that was actually published.
    if !reviewed.local_is_current(&settings, &device, &vault, |connection| {
      local_key(&app, connection)
    })? {
      return Ok(Some(CONCURRENT_EDIT_WARNING));
    }
    Ok(save_checkpoint(
      &root,
      &mut device,
      &reviewed,
      reviewed.device_hash.clone(),
    ))
  })
  .await
}

fn save_checkpoint(
  root: &Path,
  device: &mut Device,
  reviewed: &Stamp,
  common: Option<String>,
) -> Option<&'static str> {
  device.record(
    reviewed.connection.connection_id.clone(),
    &reviewed.owner,
    common,
  );
  device.save(root).err().map(|_| TRACKING_WARNING)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::settings::credential_ref;
  use std::sync::Arc;

  fn fixture(session: &Arc<Mutex<()>>) -> (ReviewState, Device, Connection) {
    let mut state = ReviewState {
      session: Arc::downgrade(session),
      owner: Owner {
        account: "fixture-account".into(),
        vault_id: Uuid::new_v4(),
      },
      vault: crypto::random().unwrap(),
      revision: Some(Uuid::new_v4()),
      entries: BTreeMap::new(),
    };
    let connection = Connection {
      connection_id: "fixture-id".into(),
      name: "Reading assistant".into(),
      provider_id: "openai".into(),
      base_url: apell_core::providers::default_base_url("openai")
        .unwrap()
        .into(),
      model_id: "fixture-model".into(),
      credential: credential_ref("fixture-id"),
    };
    let keys = VaultKeys::new(&state.owner, &state.vault);
    let aad = keys.context(&connection).unwrap();
    let mut device = Device::default();
    device.record(
      connection.connection_id.clone(),
      &state.owner,
      Some(current_fingerprint(
        &keys.fingerprint,
        &aad,
        b"previous-key",
      )),
    );
    let Sealed { nonce, ciphertext } =
      crypto::seal_api_key(&keys.encryption, &aad, b"cloud-key").unwrap();
    state.entries.insert(
      connection.connection_id.clone(),
      Entry {
        connection_id: connection.connection_id.clone(),
        nonce,
        ciphertext,
      },
    );
    (state, device, connection)
  }

  fn saved_key() -> Option<Zeroizing<String>> {
    Some(Zeroizing::new("device-key".into()))
  }

  #[test]
  fn review_requires_owned_valid_copies_and_distinguishes_authenticated_absence() {
    let session = Arc::new(Mutex::new(()));
    let (mut state, mut device, connection) = fixture(&session);
    let id = &connection.connection_id;
    let keys = VaultKeys::new(&state.owner, &state.vault);
    let (stamp, _) = state
      .conflict(&device, &connection, &keys, |_| Ok(None))
      .unwrap()
      .unwrap();
    assert!(stamp.device_hash.is_none());
    assert!(stamp.cloud_hash.is_some());
    assert!(state
      .conflict(&device, &connection, &keys, |_| Err("read failed".into()))
      .is_err());

    let entry = state.entries.remove(id).unwrap();
    let (stamp, _) = state
      .conflict(&device, &connection, &keys, |_| Ok(saved_key()))
      .unwrap()
      .unwrap();
    assert!(stamp.device_hash.is_some());
    assert!(stamp.cloud_hash.is_none());
    state.entries.insert(
      id.clone(),
      Entry {
        nonce: "invalid".into(),
        ..entry
      },
    );
    assert!(state
      .conflict(&device, &connection, &keys, |_| Ok(saved_key()))
      .is_err());

    for assignment in [
      Assignment::Blocked,
      Assignment::Unpaired(state.owner.account.clone()),
      Assignment::Paired(Owner {
        vault_id: Uuid::new_v4(),
        ..state.owner.clone()
      }),
    ] {
      device.connections.get_mut(id).unwrap().assignment = assignment;
      assert!(state
        .conflict(&device, &connection, &keys, |_| {
          panic!("unowned credentials must not be inspected")
        })
        .unwrap()
        .is_none());
    }
  }

  #[test]
  fn review_handles_match_only_the_exact_session_revision_destination_and_copies() {
    let session = Arc::new(Mutex::new(()));
    let (state, device, connection) = fixture(&session);
    let (stamp, _) = state
      .conflict(
        &device,
        &connection,
        &VaultKeys::new(&state.owner, &state.vault),
        |_| Ok(saved_key()),
      )
      .unwrap()
      .unwrap();
    let mut reviews = Reviews::default();
    let initial = reviews.replace(vec![stamp.clone()]);
    let input = ConflictResolution {
      connection_id: connection.connection_id.clone(),
      review_id: Uuid::parse_str(initial[0]["review_id"].as_str().unwrap()).unwrap(),
      resolution: Choice::KeepDevice,
    };
    assert_eq!(
      initial,
      json!([{
        "connection_id": connection.connection_id,
        "review_id": input.review_id,
        "name": connection.name,
        "provider_id": connection.provider_id,
        "model_id": connection.model_id,
        "device_has_key": true,
        "cloud_has_key": true,
      }])
    );
    assert_eq!(initial, reviews.replace(vec![stamp.clone()]));
    assert!(reviews.take(&input).unwrap().matches(&stamp));
    assert!(reviews.take(&input).is_err());
    assert_ne!(initial, reviews.replace(vec![stamp.clone()]));
    let changes: [fn(&mut Stamp); 8] = [
      |x| x.owner.account = "another-account".into(),
      |x| x.owner.vault_id = Uuid::new_v4(),
      |x| x.session = Arc::downgrade(&Arc::new(Mutex::new(()))),
      |x| x.revision = Some(Uuid::new_v4()),
      |x| x.connection.credential.account = "other-slot".into(),
      |x| x.tracked.common = None,
      |x| x.device_hash = None,
      |x| x.cloud_hash = None,
    ];
    for change in changes {
      let mut changed = stamp.clone();
      change(&mut changed);
      assert!(!stamp.matches(&changed));
    }
    let mut changed = stamp;
    changed.device_hash = None;
    reviews.replace(vec![changed]);
    assert!(reviews.take(&input).is_err());
    reviews.clear();
    assert!(reviews.0.is_empty());
  }

  #[test]
  fn keeping_the_device_replaces_or_removes_only_the_selected_encrypted_entry() {
    let session = Arc::new(Mutex::new(()));
    let (mut state, device, connection) = fixture(&session);
    let id = &connection.connection_id;
    let keys = VaultKeys::new(&state.owner, &state.vault);
    let unrelated = Entry {
      connection_id: "other".into(),
      ..state.entries[id].clone()
    };
    state
      .entries
      .insert(unrelated.connection_id.clone(), unrelated.clone());
    let (_, copies) = state
      .conflict(&device, &connection, &keys, |_| Ok(saved_key()))
      .unwrap()
      .unwrap();
    set_cloud_entry(&mut state.entries, &keys, id, copies).unwrap();
    let stored = crypto::open_api_key(
      &keys.encryption,
      &state.vault,
      &keys.context(&connection).unwrap(),
      &state.entries[id].sealed(),
    )
    .unwrap();
    assert_eq!(stored.as_slice(), b"device-key");
    let (_, copies) = state
      .conflict(&device, &connection, &keys, |_| Ok(None))
      .unwrap()
      .unwrap();
    set_cloud_entry(&mut state.entries, &keys, id, copies).unwrap();
    assert!(state.entries.into_values().eq([unrelated]));
  }

  #[test]
  fn using_cloud_targets_only_the_reviewed_credential_even_when_cloud_has_no_key() {
    let session = Arc::new(Mutex::new(()));
    for cloud_has_key in [true, false] {
      let (mut state, device, connection) = fixture(&session);
      if !cloud_has_key {
        state.entries.clear();
      }
      let root = tempfile::tempdir().unwrap();
      device.save(root.path()).unwrap();
      let mut unrelated = connection.clone();
      unrelated.connection_id = "other".into();
      unrelated.credential = credential_ref(&unrelated.connection_id);
      let settings = Settings {
        active_connection_id: Some(unrelated.connection_id.clone()),
        connections: vec![connection.clone(), unrelated],
        ..Settings::default()
      };
      let keys = VaultKeys::new(&state.owner, &state.vault);
      let (reviewed, _) = state
        .conflict(&device, &connection, &keys, |_| Ok(saved_key()))
        .unwrap()
        .unwrap();
      let choice = state
        .prepare(root.path(), &settings, &reviewed, Choice::UseCloud, |_| {
          Ok(saved_key())
        })
        .unwrap();
      let Prepared::UseCloud {
        slot,
        value,
        common,
        ..
      } = choice
      else {
        panic!("the cloud choice must prepare a local write");
      };
      assert_eq!(slot, connection.credential.account);
      assert_eq!(common, reviewed.cloud_hash);
      assert_eq!(
        value,
        cloud_has_key.then(|| encode_key(&connection, b"cloud-key").unwrap())
      );
      assert_eq!(
        Device::load(root.path()).unwrap().connections[&connection.connection_id].common,
        reviewed.tracked.common
      );
    }
  }

  #[test]
  fn a_device_checkpoint_rejects_local_edits_and_reports_durable_write_failure() {
    let session = Arc::new(Mutex::new(()));
    let (state, mut device, connection) = fixture(&session);
    let (stamp, _) = state
      .conflict(
        &device,
        &connection,
        &VaultKeys::new(&state.owner, &state.vault),
        |_| Ok(saved_key()),
      )
      .unwrap()
      .unwrap();
    let settings = Settings {
      connections: vec![connection],
      ..Settings::default()
    };
    assert!(stamp
      .local_is_current(&settings, &device, &state.vault, |_| Ok(saved_key()))
      .unwrap());
    assert!(!stamp
      .local_is_current(&settings, &device, &state.vault, |_| Ok(None))
      .unwrap());
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(STATE_FILE)).unwrap();
    assert_eq!(
      save_checkpoint(root.path(), &mut device, &stamp, stamp.device_hash.clone()),
      Some(TRACKING_WARNING)
    );
  }
}
