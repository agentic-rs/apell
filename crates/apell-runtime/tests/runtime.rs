use apell_runtime::{HostError, HostEvents, HostServices, RuntimeHandle, ServiceFuture};
use serde_json::{json, Value};
use std::{
  path::Path,
  process::Command,
  sync::{
    atomic::{AtomicBool, Ordering},
    Arc, OnceLock,
  },
  time::Duration,
};
use tokio::sync::{Notify, Semaphore};

fn fixture() -> Vec<u8> {
  static BUILT: OnceLock<Vec<u8>> = OnceLock::new();
  BUILT
    .get_or_init(|| {
      let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
      let target = crate_dir.join("../../target/apell-runtime-fixture");
      let result = Command::new(env!("CARGO"))
        .args([
          "build",
          "--release",
          "--locked",
          "--offline",
          "--target",
          "wasm32-unknown-unknown",
          "--manifest-path",
        ])
        .arg(crate_dir.join("tests/fixtures/counter/Cargo.toml"))
        .arg("--target-dir")
        .arg(&target)
        .env_remove("CARGO_MAKEFLAGS")
        .env_remove("MAKEFLAGS")
        .status()
        .unwrap();
      assert!(
        result.success(),
        "fixture requires the wasm32-unknown-unknown Rust target"
      );
      std::fs::read(target.join("wasm32-unknown-unknown/release/apell_runtime_fixture.wasm"))
        .unwrap()
    })
    .clone()
}

struct Services {
  entered: Arc<Notify>,
  release: Arc<Semaphore>,
  dropped: Arc<AtomicBool>,
}
impl Default for Services {
  fn default() -> Self {
    Self {
      entered: Arc::new(Notify::new()),
      release: Arc::new(Semaphore::new(0)),
      dropped: Arc::new(AtomicBool::new(false)),
    }
  }
}
impl HostServices for Services {
  fn call(&self, method: String, params: Value, events: HostEvents) -> ServiceFuture {
    let entered = self.entered.clone();
    let release = self.release.clone();
    let dropped = self.dropped.clone();
    Box::pin(async move {
      match method.as_str() {
        "test.delay" => {
          struct Mark(Arc<AtomicBool>);
          impl Drop for Mark {
            fn drop(&mut self) {
              self.0.store(true, Ordering::Release);
            }
          }
          let _mark = Mark(dropped);
          entered.notify_one();
          release.acquire().await.unwrap().forget();
          events.emit(json!({"text":"progress"}))?;
          Ok(json!("host completed"))
        }
        "test.echo" if params == "error" => Err(HostError {
          message: "interrupted".into(),
          partial_content: "visible text".into(),
        }),
        "test.echo" => Ok(params),
        _ => Err(HostError::new("Denied fixture service.")),
      }
    })
  }
}

#[tokio::test]
async fn rust_guest_keeps_memory_and_jobs_across_frontend_detach_and_reattach() {
  let services = Arc::new(Services::default());
  let run = RuntimeHandle::start(fixture(), services.clone())
    .await
    .unwrap();
  let old_view = run.clone();
  assert_eq!(old_view.invoke("increment", Value::Null).await.unwrap(), 1);
  assert_eq!(
    old_view.invoke("start", Value::Null).await.unwrap(),
    "started"
  );
  tokio::time::timeout(Duration::from_secs(8), services.entered.notified())
    .await
    .expect("guest host call was not submitted");
  drop(old_view);

  let new_view = run.clone();
  let mut events = new_view.subscribe();
  assert_eq!(new_view.invoke("increment", Value::Null).await.unwrap(), 2);
  assert_eq!(
    new_view.invoke("snapshot", Value::Null).await.unwrap(),
    json!({"counter":2,"last":null})
  );
  services.release.add_permits(1);
  assert_eq!(
    tokio::time::timeout(Duration::from_secs(8), events.recv())
      .await
      .unwrap()
      .unwrap(),
    json!({"name":"progress","payload":{"text":"progress"}})
  );
  assert_eq!(
    tokio::time::timeout(Duration::from_secs(8), events.recv())
      .await
      .unwrap()
      .unwrap(),
    json!({"name":"finished","payload":"host completed"})
  );
  assert_eq!(
    new_view.invoke("snapshot", Value::Null).await.unwrap(),
    json!({"counter":2,"last":"host completed"})
  );
  run.stop().await;
  let fresh = RuntimeHandle::start(fixture(), services).await.unwrap();
  assert_eq!(
    fresh.invoke("snapshot", Value::Null).await.unwrap(),
    json!({"counter":0,"last":null})
  );
  fresh.stop().await;
}

#[tokio::test]
async fn losing_the_invoke_receiver_does_not_cancel_backend_work() {
  let services = Arc::new(Services::default());
  let run = RuntimeHandle::start(fixture(), services.clone())
    .await
    .unwrap();
  let old_view = run.clone();
  let invoke = tokio::spawn(async move { old_view.invoke("wait", Value::Null).await });
  tokio::time::timeout(Duration::from_secs(8), services.entered.notified())
    .await
    .expect("guest host call was not submitted");
  invoke.abort();
  let _ = invoke.await;
  let mut events = run.subscribe();
  services.release.add_permits(1);
  tokio::time::timeout(Duration::from_secs(5), async {
    while events.recv().await.unwrap()["name"] != "finished" {}
  })
  .await
  .unwrap();
  assert_eq!(
    run.invoke("snapshot", Value::Null).await.unwrap()["last"],
    "host completed"
  );
  run.stop().await;
}

#[tokio::test]
async fn stop_joins_pending_host_work_and_settles_waiting_commands() {
  let services = Arc::new(Services::default());
  let run = RuntimeHandle::start(fixture(), services.clone())
    .await
    .unwrap();
  let view = run.clone();
  let mut subscription = run.subscribe();
  let waiting = tokio::spawn(async move { view.invoke("wait", Value::Null).await });
  tokio::time::timeout(Duration::from_secs(8), services.entered.notified())
    .await
    .expect("guest host call was not submitted");
  run.stop().await;
  assert!(services.dropped.load(Ordering::Acquire));
  assert!(waiting.await.unwrap().is_err());
  assert!(matches!(
    subscription.recv().await,
    Err(tokio::sync::broadcast::error::RecvError::Closed)
  ));
  assert!(run.invoke("snapshot", Value::Null).await.is_err());
}

#[tokio::test]
async fn null_results_typed_errors_and_host_values_round_trip_through_real_wasm() {
  let run = RuntimeHandle::start(fixture(), Arc::new(Services::default()))
    .await
    .unwrap();
  for value in [Value::Null, json!({"unicode":"例","nested":[1,null]})] {
    assert_eq!(run.invoke("echo", value.clone()).await.unwrap(), value);
  }
  let error = run.invoke("echo", json!("error")).await.unwrap_err();
  assert_eq!(error.message, "interrupted");
  assert_eq!(error.partial_content, "visible text");
  let metadata = run.invoke("metadata", Value::Null).await.unwrap();
  assert!(uuid::Uuid::parse_str(metadata["id"].as_str().unwrap()).is_ok());
  assert!(metadata["now"].as_str().unwrap().ends_with('Z'));
  run.stop().await;
}

#[tokio::test]
async fn traps_and_limits_are_confined_to_one_interpreter() {
  let services = Arc::new(Services::default());
  // Wasm declares no filesystem, process, socket, or ambient WASI imports.
  for source in [
    "(module (import \"wasi_snapshot_preview1\" \"fd_read\" (func)))",
    "(module (memory (export \"memory\") 2048))",
    "(module (memory (export \"memory\") 1))",
  ] {
    assert!(
      RuntimeHandle::start(wat::parse_str(source).unwrap(), services.clone())
        .await
        .is_err()
    );
  }
  let failed = RuntimeHandle::start(fixture(), services.clone())
    .await
    .unwrap();
  let healthy = RuntimeHandle::start(fixture(), services).await.unwrap();
  let error = failed.invoke("stall", Value::Null).await.unwrap_err();
  assert!(
    error.message.to_lowercase().contains("fuel"),
    "unexpected error: {error}"
  );
  assert_eq!(healthy.invoke("increment", Value::Null).await.unwrap(), 1);
  failed.stop().await;
  healthy.stop().await;
}
