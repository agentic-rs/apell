use super::*;
use tokio::net::TcpListener;

async fn peer(mode: u8) -> (String, tokio::task::JoinHandle<()>) {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("ws://{}/records", listener.local_addr().unwrap());
  let task = tokio::spawn(async move {
    let (stream, _) = listener.accept().await.unwrap();
    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
    if mode == 1 {
      socket
        .send(Message::Text("not binary".into()))
        .await
        .unwrap();
      return;
    }
    if mode == 2 {
      socket.close(None).await.unwrap();
      return;
    }
    while let Some(Ok(frame)) = socket.next().await {
      match frame {
        Message::Binary(bytes) => {
          socket.send(Message::Ping(vec![1, 2].into())).await.unwrap();
          socket.send(Message::Binary(bytes)).await.unwrap();
        }
        Message::Close(_) => break,
        _ => {}
      }
    }
  });
  (endpoint, task)
}

async fn connect(service: &WebSocketService, endpoint: String) -> String {
  // Keep a sender alive: a dropped owner watch denotes cancellation.
  let (send, cancellation) = watch::channel(false);
  let opened = service
    .open(
      "run",
      OpenRequest {
        url: endpoint,
        headers: Vec::new(),
      },
      cancellation,
    )
    .await
    .unwrap();
  drop(send);
  opened["socket_handle"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn binary_records_are_scoped_bounded_and_never_replayed() {
  let service = WebSocketService::new();
  let (endpoint, peer) = peer(0).await;
  let handle = connect(&service, endpoint).await;
  let data = URL_SAFE_NO_PAD.encode(b"one record");
  assert!(service.send("another-run", &handle, &data).await.is_err());
  assert!(service.receive("another-run", &handle, 1000).await.is_err());
  assert!(service.close("another-run", &handle).is_err());
  service.send("run", &handle, &data).await.unwrap();
  assert_eq!(
    service.receive("run", &handle, 1000).await.unwrap(),
    json!({"data":data})
  );
  assert!(service.send("run", &handle, "bad=").await.is_err());
  assert!(service
    .send(
      "run",
      &handle,
      &URL_SAFE_NO_PAD.encode(vec![0; MAX_RECORD_BYTES + 1])
    )
    .await
    .is_err());
  service.close("run", &handle).unwrap();
  assert!(service.send("run", &handle, &data).await.is_err());
  peer.abort();
}

#[tokio::test]
async fn owner_retirement_wakes_pending_reads_and_releases_sockets() {
  let service = Arc::new(WebSocketService::new());
  let (endpoint, peer) = peer(0).await;
  let handle = connect(&service, endpoint).await;
  let pending = tokio::spawn({
    let service = service.clone();
    let handle = handle.clone();
    async move { service.receive("run", &handle, 120_000).await }
  });
  tokio::task::yield_now().await;
  service.close_session("run");
  assert!(tokio::time::timeout(Duration::from_secs(1), pending)
    .await
    .unwrap()
    .unwrap()
    .is_err());
  assert!(service.connections.lock().unwrap().is_empty());
  peer.abort();
}

#[tokio::test]
async fn clean_close_differs_from_invalid_text_or_timeout() {
  for mode in [0, 1, 2] {
    let service = WebSocketService::new();
    let (endpoint, peer) = peer(mode).await;
    let handle = connect(&service, endpoint).await;
    let result = service.receive("run", &handle, 100).await;
    if mode == 2 {
      assert_eq!(result.unwrap(), json!({"data":null}));
    } else {
      assert!(result.is_err());
    }
    assert!(service.connections.lock().unwrap().is_empty());
    peer.abort();
  }
}

#[test]
fn validates_transport_destinations_and_host_owned_headers() {
  for url in [
    "ws://example.invalid/",
    "wss://user@example.invalid/",
    "wss://example.invalid/#fragment",
    "http://localhost/",
    "ws://localhost:0/",
  ] {
    assert!(validate(OpenRequest {
      url: url.into(),
      headers: Vec::new()
    })
    .is_err());
  }
  for name in [
    "host",
    "connection",
    "upgrade",
    "sec-websocket-key",
    "proxy-authorization",
  ] {
    assert!(validate(OpenRequest {
      url: "wss://example.invalid/".into(),
      headers: vec![HttpHeader {
        name: name.into(),
        value: "value".into()
      }]
    })
    .is_err());
  }
  assert!(validate(OpenRequest {
    url: "ws://127.0.0.1:12345/?opaque=query".into(),
    headers: Vec::new()
  })
  .is_ok());
  assert!(validate(OpenRequest {
    url: "ws://[::1]:12345/".into(),
    headers: Vec::new()
  })
  .is_ok());
}
