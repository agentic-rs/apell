//! Run-owned, bounded binary WebSockets. There is no reconnect or send replay.
use crate::http_request::HttpHeader;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use futures_util::{
  stream::{SplitSink, SplitStream},
  SinkExt, StreamExt,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
  collections::HashMap,
  sync::{Arc, Mutex},
  time::Duration,
};
use tokio::{
  net::TcpStream,
  sync::{watch, Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore},
};
use tokio_tungstenite::{
  connect_async_tls_with_config,
  tungstenite::{
    client::IntoClientRequest,
    http::{HeaderName, HeaderValue},
    protocol::WebSocketConfig,
    Message,
  },
  MaybeTlsStream, WebSocketStream,
};
use url::Url;

pub const MAX_RECORD_BYTES: usize = 1024 * 1024;
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRequest {
  pub url: String,
  #[serde(default)]
  pub headers: Vec<HttpHeader>,
}

struct Connection {
  owner: String,
  writer: AsyncMutex<SplitSink<Socket, Message>>,
  reader: AsyncMutex<SplitStream<Socket>>,
  closed: watch::Sender<bool>,
  _slot: OwnedSemaphorePermit,
}

pub struct WebSocketService {
  connections: Mutex<HashMap<String, Arc<Connection>>>,
  slots: Arc<Semaphore>,
}

impl Default for WebSocketService {
  fn default() -> Self {
    Self::new()
  }
}

impl WebSocketService {
  pub fn new() -> Self {
    Self {
      connections: Mutex::new(HashMap::new()),
      slots: Arc::new(Semaphore::new(64)),
    }
  }

  pub async fn open(
    &self,
    owner: &str,
    request: OpenRequest,
    mut cancellation: watch::Receiver<bool>,
  ) -> Result<Value, String> {
    if *cancellation.borrow() {
      return Err(closed());
    }
    let request = validate(request)?;
    let slot = self
      .slots
      .clone()
      .try_acquire_owned()
      .map_err(|_| "The WebSocket service is busy.")?;
    let config = WebSocketConfig::default()
      .max_message_size(Some(MAX_RECORD_BYTES))
      .max_frame_size(Some(MAX_RECORD_BYTES));
    #[cfg(target_os = "android")]
    let connector = {
      let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
      let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
      ))
      .with_safe_default_protocol_versions()
      .map_err(|_| "Could not initialize secure networking.")?
      .with_root_certificates(roots)
      .with_no_client_auth();
      Some(tokio_tungstenite::Connector::Rustls(Arc::new(config)))
    };
    #[cfg(not(target_os = "android"))]
    let connector = None;
    // The WebSocket handshake does not follow redirects, attach cookies, or use
    // proxy environment variables. Only caller-supplied authentication is sent.
    let connected = tokio::select! {
      biased;
      _ = cancellation.changed() => return Err(closed()),
      result = tokio::time::timeout(Duration::from_secs(15), connect_async_tls_with_config(request, Some(config), false, connector)) => result,
    };
    let (socket, _) = connected
      .map_err(|_| "WebSocket connection timed out.")?
      .map_err(|_| "Could not connect to the WebSocket endpoint.")?;
    let (writer, reader) = socket.split();
    let connection = Arc::new(Connection {
      owner: owner.into(),
      writer: AsyncMutex::new(writer),
      reader: AsyncMutex::new(reader),
      closed: watch::channel(false).0,
      _slot: slot,
    });
    let mut connections = self
      .connections
      .lock()
      .map_err(|_| "WebSocket state is unavailable.")?;
    if *cancellation.borrow() {
      return Err(closed());
    }
    if connections
      .values()
      .filter(|connection| connection.owner == owner)
      .count()
      >= 16
    {
      return Err("This applet already owns sixteen WebSockets.".into());
    }
    let handle = uuid::Uuid::new_v4().to_string();
    connections.insert(handle.clone(), connection);
    Ok(json!({"socket_handle": handle}))
  }

  pub async fn send(&self, owner: &str, handle: &str, data: &str) -> Result<Value, String> {
    if data.len() > MAX_RECORD_BYTES.div_ceil(3) * 4 {
      return Err("WebSocket record exceeds 1 MiB.".into());
    }
    let bytes = URL_SAFE_NO_PAD
      .decode(data)
      .map_err(|_| "WebSocket data must be unpadded base64url.")?;
    if bytes.len() > MAX_RECORD_BYTES || URL_SAFE_NO_PAD.encode(&bytes) != data {
      return Err("Invalid WebSocket record encoding or size.".into());
    }
    let connection = self.connection(owner, handle)?;
    let mut stopped = connection.closed.subscribe();
    if *stopped.borrow() {
      return Err(closed());
    }
    let result = tokio::select! {
      biased;
      _ = stopped.changed() => return Err(closed()),
      result = tokio::time::timeout(Duration::from_secs(10), async { connection.writer.lock().await.send(Message::Binary(bytes.into())).await }) => result,
    };
    match result {
      Ok(Ok(())) => Ok(Value::Null),
      _ => {
        self.close(owner, handle)?;
        Err("WebSocket write failed; delivery may be uncertain. It was not retried.".into())
      }
    }
  }

  pub async fn receive(&self, owner: &str, handle: &str, timeout_ms: u64) -> Result<Value, String> {
    if !(100..=120_000).contains(&timeout_ms) {
      return Err("WebSocket timeout must be between 100 and 120000 milliseconds.".into());
    }
    let connection = self.connection(owner, handle)?;
    let mut stopped = connection.closed.subscribe();
    if *stopped.borrow() {
      return Err(closed());
    }
    let mut reader = connection
      .reader
      .try_lock()
      .map_err(|_| "A WebSocket receive is already pending.")?;
    let result = tokio::select! {
      biased;
      _ = stopped.changed() => return Err(closed()),
      result = tokio::time::timeout(Duration::from_millis(timeout_ms), async {
        loop {
          match reader.next().await {
            Some(Ok(Message::Binary(bytes))) => return Ok(Some(URL_SAFE_NO_PAD.encode(bytes))),
            Some(Ok(Message::Close(frame))) if frame.as_ref().is_none_or(|frame| matches!(u16::from(frame.code), 1000 | 1001)) => return Ok(None),
            Some(Ok(Message::Ping(_))) => connection.writer.lock().await.flush().await.map_err(|_| ())?,
            Some(Ok(Message::Pong(_))) => {},
            _ => return Err(()),
          }
        }
      }) => result,
    };
    match result {
      Ok(Ok(Some(data))) => Ok(json!({"data": data})),
      Ok(Ok(None)) => {
        self.close(owner, handle)?;
        Ok(json!({"data": null}))
      }
      _ => {
        self.close(owner, handle)?;
        Err("WebSocket receive failed or timed out; the connection was closed.".into())
      }
    }
  }

  fn connection(&self, owner: &str, handle: &str) -> Result<Arc<Connection>, String> {
    self
      .connections
      .lock()
      .map_err(|_| "WebSocket state is unavailable.")?
      .get(handle)
      .filter(|connection| connection.owner == owner && !*connection.closed.borrow())
      .cloned()
      .ok_or_else(closed)
  }

  pub fn close(&self, owner: &str, handle: &str) -> Result<Value, String> {
    let mut connections = self
      .connections
      .lock()
      .map_err(|_| "WebSocket state is unavailable.")?;
    if connections
      .get(handle)
      .is_some_and(|connection| connection.owner != owner)
    {
      return Err(closed());
    }
    if let Some(connection) = connections.remove(handle) {
      connection.closed.send_replace(true);
    }
    Ok(Value::Null)
  }

  pub fn close_session(&self, owner: &str) {
    if let Ok(mut connections) = self.connections.lock() {
      connections.retain(|_, connection| {
        if connection.owner == owner {
          connection.closed.send_replace(true);
          false
        } else {
          true
        }
      });
    }
  }
}

fn closed() -> String {
  "WebSocket is closed or does not belong to this applet run.".into()
}

fn validate(
  request: OpenRequest,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, String> {
  if request.url.len() > 8192 {
    return Err("WebSocket URL is too long.".into());
  }
  let url = Url::parse(&request.url).map_err(|_| "Invalid WebSocket URL.")?;
  let loopback = url.host_str().is_some_and(|host| {
    host == "localhost"
      || host
        .trim_matches(['[', ']'])
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
  });
  if (url.scheme() != "wss" && !(url.scheme() == "ws" && loopback))
    || url.host_str().is_none()
    || url.port() == Some(0)
    || !url.username().is_empty()
    || url.password().is_some()
    || url.fragment().is_some()
  {
    return Err(
      "WebSockets require WSS or loopback WS without embedded credentials or fragments.".into(),
    );
  }
  let mut wire = request
    .url
    .into_client_request()
    .map_err(|_| "Invalid WebSocket URL.")?;
  if request.headers.len() > 64 {
    return Err("Too many WebSocket headers.".into());
  }
  let mut total = 0usize;
  for header in request.headers {
    total = total
      .saturating_add(header.name.len())
      .saturating_add(header.value.len());
    if total > 32 * 1024 {
      return Err("WebSocket headers exceed 32 KiB.".into());
    }
    let name = HeaderName::from_bytes(header.name.as_bytes())
      .map_err(|_| "Invalid WebSocket header name.")?;
    if matches!(
      name.as_str(),
      "host"
        | "connection"
        | "upgrade"
        | "content-length"
        | "transfer-encoding"
        | "proxy-authorization"
        | "proxy-connection"
    ) || name.as_str().starts_with("sec-websocket-")
    {
      return Err("This WebSocket header is managed by the transport.".into());
    }
    let mut value = HeaderValue::from_bytes(
      &header
        .value
        .chars()
        .map(|value| u8::try_from(value as u32))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Invalid WebSocket header value.")?,
    )
    .map_err(|_| "Invalid WebSocket header value.")?;
    if matches!(name.as_str(), "authorization" | "cookie") {
      value.set_sensitive(true);
    }
    wire.headers_mut().append(name, value);
  }
  Ok(wire)
}

#[cfg(test)]
mod tests;
