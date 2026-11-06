use super::*;
use apell_core::bundles::{Manifest, API_VERSION};
use std::{
  future::Future,
  sync::{Arc, Mutex},
  task::{Context, Waker},
  time::Duration,
};
use tokio::sync::oneshot;

const RUN: &str = "vault-fixture-run";

fn fixture() -> (tempfile::TempDir, Arc<Host>, Session) {
  let dir = tempfile::tempdir().unwrap();
  let host = Arc::new(Host::new(dir.path().into()).unwrap());
  let session = Session {
    manifest: Manifest {
      schema_version: 1,
      api_version: API_VERSION,
      applet_id: "vault-fixture".into(),
      name: "Vault fixture".into(),
      version: "1.0.0".into(),
      permissions: vec!["secrets".into()],
      network: true,
      lib: None,
    },
    alive: Arc::new(true.into()),
    operations: Arc::default(),
    cancellation: tokio::sync::watch::channel(false).0,
  };
  host
    .sessions
    .lock()
    .unwrap()
    .insert(RUN.into(), session.clone());
  (dir, host, session)
}

#[test]
fn secret_slots_cannot_escape_the_verified_applet_namespace() {
  let slot = secret_account("one", "identity").unwrap();
  assert_eq!(slot.len(), 64);
  assert!(slot.bytes().all(|byte| byte.is_ascii_hexdigit()));
  assert_ne!(
    secret_account("one", "identity").unwrap(),
    secret_account("two", "identity").unwrap()
  );
  for key in ["", ":another:identity", "../key", "with space", "\n"] {
    assert!(secret_account("one", key).is_err());
  }
  assert!(secret_account("one", &"a".repeat(129)).is_err());
}

#[tokio::test]
async fn startup_can_read_a_local_secret_while_a_provider_refresh_is_pending() {
  let (_dir, host, session) = fixture();
  let _provider_refresh = host.credential_gate.lock().await;
  let read = with_applet_vault(&host, session, || Ok(json!({"value":"fixture"})));
  let result = tokio::time::timeout(Duration::from_secs(5), read)
    .await
    .expect("local vault access waited for the provider refresh")
    .unwrap();
  assert_eq!(result, json!({"value":"fixture"}));
}

#[tokio::test]
async fn aborting_the_waiter_preserves_the_order_of_vault_writes() {
  let (_dir, host, session) = fixture();
  let writes = Arc::new(Mutex::new(Vec::new()));
  let (entered, started) = oneshot::channel();
  let (release, finish) = oneshot::channel();
  let first_host = host.clone();
  let first_session = session.clone();
  let first_writes = writes.clone();
  let waiter = tokio::spawn(async move {
    with_applet_vault(&first_host, first_session, move || {
      entered.send(()).unwrap();
      finish.blocking_recv().map_err(|_| "Fixture released.")?;
      first_writes.lock().unwrap().push("first");
      Ok(Value::Null)
    })
    .await
  });
  started.await.unwrap();
  waiter.abort();
  assert!(waiter.await.unwrap_err().is_cancelled());
  assert!(
    host.applet_secret_gate.try_lock().is_err(),
    "the worker lost its vault lock"
  );

  let second_writes = writes.clone();
  let mut second = Box::pin(with_applet_vault(&host, session, move || {
    second_writes.lock().unwrap().push("second");
    Ok(Value::Null)
  }));
  assert!(second
    .as_mut()
    .poll(&mut Context::from_waker(Waker::noop()))
    .is_pending());
  release.send(()).unwrap();
  second.await.unwrap();
  assert_eq!(*writes.lock().unwrap(), ["first", "second"]);
}

#[tokio::test]
async fn retiring_a_run_skips_its_queued_vault_write() {
  let (_dir, host, session) = fixture();
  let occupied = host.applet_secret_gate.lock().await;
  let mut queued = Box::pin(with_applet_vault(&host, session, || {
    panic!("a retired run reached the vault")
  }));
  assert!(queued
    .as_mut()
    .poll(&mut Context::from_waker(Waker::noop()))
    .is_pending());
  host.close(RUN).unwrap();
  drop(occupied);
  assert_eq!(queued.await.unwrap_err(), "Applet session is closed.");
}
