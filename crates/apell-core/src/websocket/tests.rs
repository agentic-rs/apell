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
      assert!(matches!(
        socket.next().await,
        Some(Ok(Message::Close(None)))
      ));
      return;
    }
    while let Some(Ok(frame)) = socket.next().await {
      match frame {
        Message::Binary(bytes) => {
          socket.send(Message::Ping(vec![1, 2].into())).await.unwrap();
          socket.send(Message::Binary(bytes)).await.unwrap();
        }
        Message::Close(_) => {
          socket.flush().await.unwrap();
          break;
        }
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
  assert!(service.close("another-run", &handle).await.is_err());
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
  service.close("run", &handle).await.unwrap();
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

#[tokio::test]
async fn acknowledges_peer_close_with_the_same_code_and_reason() {
  use tokio_tungstenite::tungstenite::protocol::{frame::coding::CloseCode, CloseFrame};
  for code in [CloseCode::Normal, CloseCode::Away, CloseCode::Policy] {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/", listener.local_addr().unwrap());
    let peer = tokio::spawn(async move {
      let (stream, _) = listener.accept().await.unwrap();
      let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
      let frame = CloseFrame {
        code,
        reason: "finished".into(),
      };
      socket.close(Some(frame.clone())).await.unwrap();
      let reply = socket.next().await.unwrap().unwrap();
      assert_eq!(reply, Message::Close(Some(frame)));
    });
    let service = WebSocketService::new();
    let handle = connect(&service, endpoint).await;
    let result = service.receive("run", &handle, 1000).await;
    if code == CloseCode::Policy {
      assert!(result.is_err());
    } else {
      assert_eq!(result.unwrap(), json!({"data":null}));
    }
    tokio::time::timeout(Duration::from_secs(1), peer)
      .await
      .unwrap()
      .unwrap();
    assert!(service.connections.lock().unwrap().is_empty());
  }
}

#[tokio::test]
async fn explicit_close_wakes_send_and_receive_then_exchanges_close_frames() {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("ws://{}/", listener.local_addr().unwrap());
  let peer = tokio::spawn(async move {
    let (stream, _) = listener.accept().await.unwrap();
    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
    assert!(matches!(
      socket.next().await,
      Some(Ok(Message::Close(None)))
    ));
    socket.flush().await.unwrap();
  });
  let service = WebSocketService::new();
  let handle = connect(&service, endpoint).await;
  // Hold the writer to make a queued send deterministic; the peer sends no data.
  let connection = service.connection("run", &handle).unwrap();
  let writer = connection.transport.get().unwrap().writer.lock().await;
  let mut send = Box::pin(service.send("run", &handle, "YQ"));
  let mut receive = Box::pin(service.receive("run", &handle, 120_000));
  assert!(futures_util::poll!(&mut send).is_pending());
  assert!(futures_util::poll!(&mut receive).is_pending());
  let mut close = Box::pin(service.close("run", &handle));
  assert!(futures_util::poll!(&mut close).is_pending());
  assert!(send.await.is_err());
  assert!(receive.await.is_err());
  drop(writer);
  assert!(close.await.is_ok());
  peer.await.unwrap();
  assert!(service.connections.lock().unwrap().is_empty());
}

#[tokio::test]
async fn close_is_bounded_and_run_retirement_can_abort_it_immediately() {
  for retire in [false, true] {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/", listener.local_addr().unwrap());
    let (seen, read) = tokio::sync::oneshot::channel();
    let (release, hold) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
      let (stream, _) = listener.accept().await.unwrap();
      let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
      assert!(matches!(socket.next().await, Some(Ok(Message::Close(_)))));
      seen.send(()).unwrap();
      hold.await.unwrap(); // Keep TCP open without flushing the queued acknowledgment.
      drop(socket);
    });
    let service = Arc::new(WebSocketService::new());
    let handle = connect(&service, endpoint).await;
    let closer = tokio::spawn({
      let service = service.clone();
      async move { service.close("run", &handle).await }
    });
    read.await.unwrap();
    let deadline = if retire {
      service.close_session("run");
      Duration::from_millis(500)
    } else {
      CLOSE_TIMEOUT + Duration::from_secs(1)
    };
    let error = tokio::time::timeout(deadline, closer)
      .await
      .unwrap()
      .unwrap()
      .unwrap_err();
    if !retire {
      assert!(error.contains("timed out"));
    }
    release.send(()).unwrap();
    peer.await.unwrap();
    assert!(service.connections.lock().unwrap().is_empty());
  }
}

#[tokio::test]
async fn excess_open_never_reaches_the_peer_and_cancelled_opens_release_their_reservations() {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("ws://{}/", listener.local_addr().unwrap());
  let service = Arc::new(WebSocketService::new());
  let (_owner, cancellation) = watch::channel(false);
  let open = || {
    let service = service.clone();
    let request = OpenRequest {
      url: endpoint.clone(),
      headers: vec![],
    };
    let cancellation = cancellation.clone();
    tokio::spawn(async move { service.open("run", request, cancellation).await })
  };
  let mut pending = Vec::new();
  let mut accepted_streams = Vec::new();
  for _ in 0..16 {
    pending.push(open());
    accepted_streams.push(listener.accept().await.unwrap().0); // Hold each handshake open.
  }
  let rejected = tokio::time::timeout(Duration::from_secs(1), open())
    .await
    .unwrap()
    .unwrap()
    .unwrap_err();
  assert!(rejected.contains("sixteen"));
  assert!(
    tokio::time::timeout(Duration::from_millis(100), listener.accept())
      .await
      .is_err()
  );

  let cancelled = pending.pop().unwrap();
  cancelled.abort();
  assert!(cancelled.await.unwrap_err().is_cancelled());
  pending.push(open());
  accepted_streams.push(
    tokio::time::timeout(Duration::from_secs(1), listener.accept())
      .await
      .unwrap()
      .unwrap()
      .0,
  );

  service.close_session("run");
  for operation in pending {
    assert!(tokio::time::timeout(Duration::from_secs(1), operation)
      .await
      .unwrap()
      .unwrap()
      .is_err());
  }
  assert!(service.connections.lock().unwrap().is_empty());
  assert_eq!(service.slots.available_permits(), 64);
}
