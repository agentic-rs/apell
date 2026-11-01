use super::{MAX_ERROR_BYTES, MAX_RESPONSE_BYTES};
use reqwest::Client;
use serde::de::DeserializeOwned;
use std::{
  fmt,
  time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokn_sdk::{
  session::{self, Clock, Events},
  transport::{Request, ResponseHead, Transport},
  BufferedResponse, Endpoint, PlatformFactory, PlatformResponse, RequestEvent, ResponseAdapter,
  Services,
};

const TRANSPORT_ERROR: &str = "Apell provider transport failed.";
const LIMIT_ERROR: &str = "Apell provider response exceeded its byte limit.";
const PROTOCOL_ERROR: &str = "Apell provider transport received an invalid request.";

/// Only classifications cross the platform boundary, never URLs or HTTP bodies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PlatformError {
  Transport,
  Limit,
  Protocol,
}

impl fmt::Display for PlatformError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str(match self {
      Self::Transport => TRANSPORT_ERROR,
      Self::Limit => LIMIT_ERROR,
      Self::Protocol => PROTOCOL_ERROR,
    })
  }
}

impl std::error::Error for PlatformError {}

impl PlatformError {
  fn from_session(error: session::Error) -> Self {
    // Tokn's public transport trait carries Display errors through its session
    // error. Recognize only our fixed markers and its bounded-reader marker;
    // never return provider-controlled diagnostic text to the applet.
    match error.message.as_str() {
      TRANSPORT_ERROR => Self::Transport,
      LIMIT_ERROR | "response exceeds byte limit" => Self::Limit,
      _ => Self::Protocol,
    }
  }
}

/// Native services only. Rules, credentials, and history remain caller-owned.
pub(super) struct ApellPlatform {
  client: Client,
  response_limit: usize,
  origin: Instant,
}

impl ApellPlatform {
  pub fn new(client: Client) -> Self {
    Self {
      client,
      response_limit: MAX_RESPONSE_BYTES,
      origin: Instant::now(),
    }
  }
}

impl PlatformFactory for ApellPlatform {
  type Pool = Client;
  type Exchange<'a> = ApellExchange;
  type Clock<'a> = ApellClock;
  type Events<'a> = NoEvents;

  fn pool(&self) -> Self::Pool {
    self.client.clone()
  }

  fn acquire<'a>(
    &'a self,
    pool: &Self::Pool,
  ) -> Result<Services<Self::Exchange<'a>, Self::Clock<'a>, Self::Events<'a>>, session::Error> {
    Ok(Services {
      transport: ApellExchange::new(pool.clone(), self.response_limit),
      clock: ApellClock(self.origin),
      events: NoEvents,
    })
  }
}

type NativeResponse = session::Response<ApellExchange, ApellClock, NoEvents>;
type NativeStream = session::EventStream<ApellExchange, ApellClock, NoEvents>;

impl ResponseAdapter for ApellPlatform {
  type Error = PlatformError;
  type Response<'a> = ApellResponse;
  type Stream<'a> = ApellStream;

  fn encode_error(_: serde_json::Error) -> Self::Error {
    PlatformError::Protocol
  }

  fn execution_error(error: session::Error, _: Endpoint) -> Self::Error {
    PlatformError::from_session(error)
  }

  fn finish_response<'a>(response: NativeResponse) -> Self::Response<'a>
  where
    Self: 'a,
  {
    ApellResponse(response)
  }
}

pub(super) struct ApellResponse(NativeResponse);

impl ApellResponse {
  pub fn status(&self) -> u16 {
    self.0.status
  }

  pub fn content_type(&self) -> Option<&str> {
    self
      .0
      .headers
      .get("content-type")
      .map(|value| value.as_str())
  }

  pub fn into_body(self) -> ApellBody {
    match self.0.body {
      session::ResponseBody::Buffered(bytes) => ApellBody::Buffered(bytes.to_vec()),
      session::ResponseBody::Stream(stream) => ApellBody::Stream(ApellStream(stream)),
    }
  }
}

impl PlatformResponse for ApellResponse {
  type Error = PlatformError;
  type Stream = ApellStream;

  fn into_json<T: DeserializeOwned>(self) -> Result<BufferedResponse<T>, Self::Error> {
    let (status, headers, data) = self.0.into_json().map_err(PlatformError::from_session)?;
    Ok(BufferedResponse {
      status,
      headers,
      data,
    })
  }

  fn into_stream(self) -> Result<Self::Stream, Self::Error> {
    match self.into_body() {
      ApellBody::Stream(stream) => Ok(stream),
      ApellBody::Buffered(_) => Err(PlatformError::Protocol),
    }
  }
}

pub(super) enum ApellBody {
  Buffered(Vec<u8>),
  Stream(ApellStream),
}

pub(super) struct ApellStream(Box<NativeStream>);

impl ApellStream {
  pub async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, PlatformError> {
    self
      .0
      .next_chunk()
      .await
      .map(|chunk| chunk.map(|bytes| bytes.to_vec()))
      .map_err(PlatformError::from_session)
  }
}

pub(super) struct ApellClock(Instant);

impl Clock for ApellClock {
  fn now(&self) -> Duration {
    self.0.elapsed()
  }

  fn unix_seconds(&self) -> u64 {
    SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default()
      .as_secs()
  }

  async fn sleep(&mut self, delay: Duration) {
    tokio::time::sleep(delay).await;
  }
}

pub(super) struct NoEvents;

impl Events for NoEvents {
  fn enabled(&self) -> bool {
    false
  }

  fn emit(&self, _: RequestEvent) {}
}

/// Each request owns its response and cumulative byte budget. Dropping a Tokn
/// session/stream closes this exchange; no detached request tasks are started.
pub(super) struct ApellExchange {
  client: Client,
  response: Option<reqwest::Response>,
  pending: Vec<u8>,
  position: usize,
  received: usize,
  limit: usize,
  error_response: bool,
  failure: Option<PlatformError>,
}

impl ApellExchange {
  fn new(client: Client, limit: usize) -> Self {
    Self {
      client,
      response: None,
      pending: Vec::new(),
      position: 0,
      received: 0,
      limit,
      error_response: false,
      failure: None,
    }
  }

  fn fail(&mut self, error: PlatformError) -> PlatformError {
    self.close_response();
    self.failure = Some(error);
    error
  }

  fn prepare_request(
    &self,
    mut request: Request,
  ) -> Result<reqwest::RequestBuilder, PlatformError> {
    request.headers.remove("host");
    request.headers.remove("content-length");
    let mut headers = reqwest::header::HeaderMap::with_capacity(request.headers.len());
    for (name, value) in request.headers {
      let name = reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes())
        .map_err(|_| PlatformError::Protocol)?;
      let mut value = reqwest::header::HeaderValue::from_str(value.as_str())
        .map_err(|_| PlatformError::Protocol)?;
      if name == reqwest::header::AUTHORIZATION
        || name == reqwest::header::PROXY_AUTHORIZATION
        || name == "x-api-key"
      {
        value.set_sensitive(true);
      }
      headers.append(name, value);
    }
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
      .map_err(|_| PlatformError::Protocol)?;
    Ok(
      self
        .client
        .request(method, request.url)
        .headers(headers)
        .body(request.body),
    )
  }
}

impl Transport for ApellExchange {
  type Error = PlatformError;

  async fn send(&mut self, request: Request) -> Result<ResponseHead, Self::Error> {
    self.close_response();
    self.received = 0;
    self.error_response = false;
    self.failure = None;
    let response = self
      .prepare_request(request)?
      .send()
      .await
      .map_err(|_| self.fail(PlatformError::Transport))?;
    let head = ResponseHead {
      status: response.status().as_u16(),
      headers: response.headers().into(),
    };
    self.error_response = !response.status().is_success();
    self.response = Some(response);
    Ok(head)
  }

  async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
    if buffer.is_empty() {
      return Ok(0);
    }
    if let Some(error) = self.failure {
      return Err(error);
    }
    while self.position == self.pending.len() {
      self.pending.clear();
      self.position = 0;
      let Some(response) = self.response.as_mut() else {
        return Ok(0);
      };
      let chunk = match response.chunk().await {
        Ok(chunk) => chunk,
        Err(_) if self.error_response => {
          // Once HTTP supplied an error status, body diagnostics are optional.
          // Keep that status even if the provider truncates its error payload.
          self.close_response();
          return Ok(0);
        }
        Err(_) => return Err(self.fail(PlatformError::Transport)),
      };
      let Some(chunk) = chunk else {
        self.response = None;
        return Ok(0);
      };
      let limit = if self.error_response {
        self.limit.min(MAX_ERROR_BYTES)
      } else {
        self.limit
      };
      let remaining = limit.saturating_sub(self.received);
      if !self.error_response && chunk.len() > remaining {
        return Err(self.fail(PlatformError::Limit));
      }
      let count = chunk.len().min(remaining);
      self.received += count;
      self.pending.extend_from_slice(&chunk[..count]);
      if self.error_response && self.received == limit {
        self.response = None;
      }
    }
    let count = buffer.len().min(self.pending.len() - self.position);
    buffer[..count].copy_from_slice(&self.pending[self.position..self.position + count]);
    self.position += count;
    Ok(count)
  }

  fn close_response(&mut self) {
    self.response = None;
    self.pending.clear();
    self.position = 0;
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
  };

  fn client() -> Client {
    Client::builder().no_proxy().build().unwrap()
  }

  fn request(url: &str) -> Request {
    Request {
      method: "GET",
      url: url.into(),
      headers: Default::default(),
      body: Vec::new().into(),
    }
  }

  #[test]
  fn validates_headers_before_network_and_marks_credentials_sensitive() {
    let exchange = ApellExchange::new(client(), 16);
    for (name, value) in [
      ("authorization", "Bearer fixture\r\ninjected: value"),
      ("invalid name", "value"),
    ] {
      let mut input = request("http://127.0.0.1:1");
      input.headers.insert(name, value);
      assert!(matches!(
        exchange.prepare_request(input),
        Err(PlatformError::Protocol)
      ));
    }
    let mut input = request("http://127.0.0.1:1");
    input
      .headers
      .insert("authorization", "Bearer fixture-secret");
    input.headers.insert("x-api-key", "fixture-secret");
    input.headers.insert("host", "different.invalid");
    input.headers.insert("content-length", "999");
    let prepared = exchange.prepare_request(input).unwrap().build().unwrap();
    assert!(prepared.headers()["authorization"].is_sensitive());
    assert!(prepared.headers()["x-api-key"].is_sensitive());
    assert!(!format!("{:?}", prepared.headers()).contains("fixture-secret"));
    assert!(!prepared.headers().contains_key("host"));
    assert_ne!(
      prepared
        .headers()
        .get("content-length")
        .map(|value| value.as_bytes()),
      Some(b"999".as_slice())
    );
  }

  async fn serve(bytes: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
      let (mut socket, _) = listener.accept().await.unwrap();
      let mut request = Vec::new();
      let mut buffer = [0; 1024];
      while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let count = socket.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0);
        request.extend_from_slice(&buffer[..count]);
      }
      let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
      );
      socket.write_all(headers.as_bytes()).await.unwrap();
      socket.write_all(&bytes).await.unwrap();
    });
    (url, task)
  }

  #[tokio::test]
  async fn small_reads_preserve_exact_budget_and_new_exchanges_reset_it() {
    tokio::time::timeout(Duration::from_secs(5), async {
      let mut exchange = ApellExchange::new(client(), 16);
      for bytes in [b"0123456789abcdef".to_vec(), b"replacement".to_vec()] {
        let (url, server) = serve(bytes.clone()).await;
        assert_eq!(exchange.send(request(&url)).await.unwrap().status, 200);
        let mut received = Vec::new();
        let mut buffer = [0; 3];
        loop {
          let count = exchange.read(&mut buffer).await.unwrap();
          if count == 0 {
            break;
          }
          received.extend_from_slice(&buffer[..count]);
        }
        assert_eq!(received, bytes);
        server.await.unwrap();
      }
    })
    .await
    .unwrap();
  }

  #[tokio::test]
  async fn overflowing_exchange_remains_an_error_until_replaced() {
    tokio::time::timeout(Duration::from_secs(5), async {
      let (url, server) = serve(vec![b'x'; 17]).await;
      let mut exchange = ApellExchange::new(client(), 16);
      exchange.send(request(&url)).await.unwrap();
      let mut buffer = [0; 3];
      loop {
        match exchange.read(&mut buffer).await {
          Ok(0) => panic!("Overflow must not become successful EOF."),
          Ok(_) => {}
          Err(error) => {
            assert_eq!(error, PlatformError::Limit);
            break;
          }
        }
      }
      assert_eq!(exchange.read(&mut buffer).await, Err(PlatformError::Limit));
      assert!(exchange.response.is_none());
      server.await.unwrap();
    })
    .await
    .unwrap();
  }

  #[test]
  fn owned_platform_and_client_support_native_shared_state() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ApellPlatform>();
    assert_send_sync::<tokn_sdk::Client<ApellPlatform>>();
  }
}
