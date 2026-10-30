use reqwest::{
  header::{HeaderMap, HeaderName, HeaderValue},
  Client, Method, Url,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const MAX_URL_BYTES: usize = 8192;
const MAX_REQUEST_HEADERS: usize = 64;
const MAX_REQUEST_HEADER_BYTES: usize = 32 * 1024;
const MAX_RESPONSE_HEADER_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HttpHeader {
  pub name: String,
  /// Values use the HTTP byte-string convention: U+0000–U+00FF map to bytes.
  pub value: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HttpRequest {
  pub url: String,
  #[serde(default = "default_method")]
  pub method: String,
  #[serde(default)]
  pub headers: Vec<HttpHeader>,
  #[serde(default)]
  pub body: Option<Vec<u8>>,
  #[serde(default = "default_timeout")]
  pub timeout_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HttpResponse {
  pub status: u16,
  pub headers: Vec<HttpHeader>,
  pub body: Vec<u8>,
}

fn default_method() -> String {
  "GET".into()
}

fn default_timeout() -> u64 {
  30_000
}

struct ValidatedRequest {
  url: Url,
  method: Method,
  headers: HeaderMap,
}

pub struct HttpService {
  client: Client,
  active: tokio::sync::Semaphore,
}

impl HttpService {
  pub fn new() -> Result<Self, String> {
    let client = crate::http::client_builder()?
      .redirect(reqwest::redirect::Policy::none())
      .connect_timeout(Duration::from_secs(20))
      .build()
      .map_err(|_| "Could not initialize HTTP networking.".to_owned())?;
    Ok(Self {
      client,
      active: tokio::sync::Semaphore::new(4),
    })
  }

  /// Dropping this future cancels the request; the host owns session lifetime.
  pub async fn request(&self, request: HttpRequest) -> Result<HttpResponse, String> {
    let validated = validate_request(&request)?;
    let _permit = self.active.try_acquire().map_err(|_| {
      "The HTTP service is busy. Try again when another request finishes.".to_owned()
    })?;
    let is_head = validated.method == Method::HEAD;
    let mut builder = self
      .client
      .request(validated.method, validated.url)
      .headers(validated.headers)
      .timeout(Duration::from_millis(request.timeout_ms));
    if let Some(body) = request.body {
      builder = builder.body(body);
    }
    // This client has no cookie jar or default authentication headers. Each
    // applet request supplies its own headers; host credentials never enter it.
    let mut response = builder.send().await.map_err(transport_error)?;
    let status = response.status().as_u16();
    let headers = response_headers(response.headers())?;
    if !is_head
      && response
        .content_length()
        .is_some_and(|size| size > MAX_BODY_BYTES as u64)
    {
      return Err("The HTTP response body exceeds 16 MiB.".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
      if body.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
        return Err("The HTTP response body exceeds 16 MiB.".into());
      }
      body.extend_from_slice(&chunk);
    }
    Ok(HttpResponse {
      status,
      headers,
      body,
    })
  }
}

fn validate_request(request: &HttpRequest) -> Result<ValidatedRequest, String> {
  if request.url.len() > MAX_URL_BYTES {
    return Err("The HTTP request URL is too long.".into());
  }
  let url = crate::resources::validate_url(&request.url)?;
  if url.fragment().is_some() {
    return Err("HTTP request URLs must not include fragments.".into());
  }
  let method = match request.method.to_ascii_uppercase().as_str() {
    "GET" => Method::GET,
    "HEAD" => Method::HEAD,
    "POST" => Method::POST,
    "PUT" => Method::PUT,
    "PATCH" => Method::PATCH,
    "DELETE" => Method::DELETE,
    "OPTIONS" => Method::OPTIONS,
    _ => return Err("The HTTP method is unsupported.".into()),
  };
  if !(100..=120_000).contains(&request.timeout_ms) {
    return Err("HTTP timeout must be between 100 and 120000 milliseconds.".into());
  }
  if request
    .body
    .as_ref()
    .is_some_and(|body| body.len() > MAX_BODY_BYTES)
  {
    return Err("The HTTP request body exceeds 16 MiB.".into());
  }
  if request.headers.len() > MAX_REQUEST_HEADERS {
    return Err("An HTTP request may contain at most 64 headers.".into());
  }
  let mut headers = HeaderMap::new();
  let mut size = 0usize;
  for header in &request.headers {
    size = size
      .saturating_add(header.name.len())
      .saturating_add(header.value.len())
      .saturating_add(4);
    if size > MAX_REQUEST_HEADER_BYTES {
      return Err("HTTP request headers exceed 32 KiB.".into());
    }
    let name = HeaderName::from_bytes(header.name.as_bytes())
      .map_err(|_| "An HTTP header name is invalid.".to_owned())?;
    if matches!(
      name.as_str(),
      "host"
        | "content-length"
        | "transfer-encoding"
        | "connection"
        | "upgrade"
        | "proxy-authorization"
        | "proxy-connection"
        | "accept-encoding"
        | "keep-alive"
        | "te"
        | "trailer"
    ) {
      return Err("An HTTP header is managed by the host transport and cannot be supplied.".into());
    }
    let bytes = header
      .value
      .chars()
      .map(|value| u8::try_from(value as u32))
      .collect::<Result<Vec<_>, _>>()
      .map_err(|_| "HTTP header values must contain only byte-string characters.".to_owned())?;
    let mut value =
      HeaderValue::from_bytes(&bytes).map_err(|_| "An HTTP header value is invalid.".to_owned())?;
    if matches!(name.as_str(), "authorization" | "cookie") {
      value.set_sensitive(true);
    }
    headers.append(name, value);
  }
  Ok(ValidatedRequest {
    url,
    method,
    headers,
  })
}

fn response_headers(headers: &HeaderMap) -> Result<Vec<HttpHeader>, String> {
  let mut size = 0usize;
  let mut result = Vec::new();
  for (name, value) in headers {
    size = size
      .saturating_add(name.as_str().len())
      .saturating_add(value.as_bytes().len())
      .saturating_add(4);
    if size > MAX_RESPONSE_HEADER_BYTES {
      return Err("HTTP response headers exceed 64 KiB.".into());
    }
    result.push(HttpHeader {
      name: name.as_str().into(),
      value: value
        .as_bytes()
        .iter()
        .map(|byte| char::from(*byte))
        .collect(),
    });
  }
  Ok(result)
}

fn transport_error(error: reqwest::Error) -> String {
  // Reqwest errors can contain URLs (including query tokens) and request data.
  if error.is_timeout() {
    "The HTTP request timed out.".into()
  } else {
    "The HTTP request failed or its response was interrupted.".into()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
      mpsc::{self, Receiver},
      Arc,
    },
    thread::{self, JoinHandle},
  };

  struct Server {
    url: String,
    received: Receiver<Vec<u8>>,
    worker: JoinHandle<()>,
  }

  impl Server {
    fn start(reply: impl FnOnce(&[u8]) -> Vec<u8> + Send + 'static) -> Self {
      Self::stream(move |request, stream| {
        let response = reply(request);
        let _ = stream.write_all(&response);
      })
    }

    fn stream(reply: impl FnOnce(&[u8], &mut TcpStream) + Send + 'static) -> Self {
      let listener = TcpListener::bind("127.0.0.1:0").unwrap();
      let url = format!("http://{}/request", listener.local_addr().unwrap());
      let (sender, received) = mpsc::channel();
      let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
          .set_read_timeout(Some(Duration::from_secs(5)))
          .unwrap();
        stream
          .set_write_timeout(Some(Duration::from_secs(5)))
          .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        loop {
          let count = stream.read(&mut buffer).unwrap();
          assert!(count > 0, "client closed before sending its request");
          request.extend_from_slice(&buffer[..count]);
          if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&request[..end]).unwrap();
            let length = headers
              .lines()
              .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name
                  .eq_ignore_ascii_case("content-length")
                  .then(|| value.trim().parse::<usize>().unwrap())
              })
              .unwrap_or(0);
            if request.len() >= end + 4 + length {
              break;
            }
          }
        }
        sender.send(request.clone()).unwrap();
        reply(&request, &mut stream);
      });
      Self {
        url,
        received,
        worker,
      }
    }

    fn finish(self) -> Vec<u8> {
      let request = self.received.recv_timeout(Duration::from_secs(5)).unwrap();
      self.worker.join().unwrap();
      request
    }
  }

  fn request(url: &str) -> HttpRequest {
    serde_json::from_value(serde_json::json!({ "url": url })).unwrap()
  }

  fn response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = format!(
      "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n",
      body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    bytes
  }

  #[tokio::test]
  async fn post_preserves_binary_body_and_repeated_headers() {
    let server = Server::start(|request| {
      let end = request
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .unwrap()
        + 4;
      response(
        "201 Created",
        "X-Reply: first\r\nX-Reply: second\r\n",
        &request[end..],
      )
    });
    let mut input = request(&server.url);
    input.method = "POST".into();
    input.body = Some(vec![0, 1, 127, 128, 255]);
    input.headers = vec![
      HttpHeader {
        name: "X-Applet".into(),
        value: "one".into(),
      },
      HttpHeader {
        name: "X-Applet".into(),
        value: "two".into(),
      },
    ];
    let output = HttpService::new().unwrap().request(input).await.unwrap();
    assert_eq!(output.status, 201);
    assert_eq!(output.body, vec![0, 1, 127, 128, 255]);
    assert_eq!(
      output
        .headers
        .iter()
        .filter(|header| header.name == "x-reply")
        .map(|header| header.value.as_str())
        .collect::<Vec<_>>(),
      vec!["first", "second"]
    );
    let sent = server.finish();
    let sent_headers = String::from_utf8_lossy(
      &sent[..sent
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .unwrap()],
    );
    assert!(sent_headers.starts_with("POST /request HTTP/1.1"));
    assert!(sent_headers.lines().any(|line| line == "x-applet: one"));
    assert!(sent_headers.lines().any(|line| line == "x-applet: two"));
    assert!(!sent_headers.to_ascii_lowercase().contains("authorization:"));
  }

  #[tokio::test]
  async fn error_statuses_are_returned_and_redirects_are_not_followed() {
    let service = HttpService::new().unwrap();
    let missing = Server::start(|_| response("404 Not Found", "", b"missing"));
    let output = service.request(request(&missing.url)).await.unwrap();
    assert_eq!(output.status, 404);
    assert_eq!(output.body, b"missing");
    missing.finish();

    let redirect =
      Server::start(|_| response("302 Found", "Location: /never-follow\r\n", b"redirect"));
    let output = service.request(request(&redirect.url)).await.unwrap();
    assert_eq!(output.status, 302);
    assert_eq!(output.body, b"redirect");
    assert!(output
      .headers
      .iter()
      .any(|header| header.name == "location" && header.value == "/never-follow"));
    redirect.finish();
  }

  #[tokio::test]
  async fn chunked_binary_responses_are_decoded_and_bounded() {
    let service = HttpService::new().unwrap();
    let binary = Server::start(|_| {
      let mut bytes =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\n".to_vec();
      bytes.extend_from_slice(&[0, 255]);
      bytes.extend_from_slice(b"\r\n3\r\nabc\r\n0\r\n\r\n");
      bytes
    });
    assert_eq!(
      service.request(request(&binary.url)).await.unwrap().body,
      vec![0, 255, b'a', b'b', b'c']
    );
    binary.finish();
    let oversized = Server::start(|_| {
      let mut bytes = format!(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:X}\r\n",
        MAX_BODY_BYTES + 1
      )
      .into_bytes();
      bytes.resize(bytes.len() + MAX_BODY_BYTES + 1, b'x');
      bytes.extend_from_slice(b"\r\n0\r\n\r\n");
      bytes
    });
    assert_eq!(
      service.request(request(&oversized.url)).await.unwrap_err(),
      "The HTTP response body exceeds 16 MiB."
    );
    oversized.finish();
  }

  #[tokio::test]
  async fn timeout_error_does_not_expose_url_or_header_values() {
    let server = Server::stream(|_, stream| {
      stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n")
        .unwrap();
      thread::sleep(Duration::from_millis(250));
      let _ = stream.write_all(b"late");
    });
    let mut input = request(&format!("{}?token=private-query-value", server.url));
    input.timeout_ms = 100;
    input.headers = vec![HttpHeader {
      name: "Authorization".into(),
      value: "Bearer private-header-value".into(),
    }];
    let error = HttpService::new()
      .unwrap()
      .request(input)
      .await
      .unwrap_err();
    assert_eq!(error, "The HTTP request timed out.");
    assert!(!error.contains("private"));
    server.finish();
  }

  #[tokio::test]
  async fn concurrency_is_bounded_and_canceled_futures_release_capacity() {
    let service = Arc::new(HttpService::new().unwrap());
    let mut active = Vec::new();
    for _ in 0..4 {
      let (release, wait) = mpsc::channel();
      let server = Server::start(move |_| {
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        response("200 OK", "", b"finished")
      });
      let client = service.clone();
      let input = request(&server.url);
      let task = tokio::spawn(async move { client.request(input).await });
      // Wait for the real server to receive the request before testing that
      // cancellation interrupts a pending network operation.
      tokio::time::timeout(Duration::from_secs(5), async {
        loop {
          if server.received.try_recv().is_ok() {
            break;
          }
          tokio::task::yield_now().await;
        }
      })
      .await
      .unwrap();
      active.push((task, release, server.worker));
    }
    let busy = service
      .request(request("https://unused.invalid/"))
      .await
      .unwrap_err();
    assert!(busy.contains("busy"));
    for (task, release, worker) in active {
      task.abort();
      assert!(task.await.unwrap_err().is_cancelled());
      release.send(()).unwrap();
      worker.join().unwrap();
    }
    let server = Server::start(|_| response("200 OK", "", b"capacity recovered"));
    assert_eq!(
      service.request(request(&server.url)).await.unwrap().body,
      b"capacity recovered"
    );
    server.finish();
  }

  #[test]
  fn validation_denies_unsafe_urls_methods_headers_and_oversized_requests() {
    for url in [
      "file:///tmp/input",
      "http://remote.invalid/",
      "https://user:secret@example.invalid/",
      "https://example.invalid/#fragment",
    ] {
      assert!(validate_request(&request(url)).is_err());
    }
    for method in ["TRACE", "CONNECT", "GET\r\nX-Injected: value"] {
      let mut input = request("https://example.invalid/");
      input.method = method.into();
      assert!(validate_request(&input).is_err());
    }
    for name in [
      "Host",
      "Content-Length",
      "Transfer-Encoding",
      "Connection",
      "Upgrade",
      "Proxy-Authorization",
      "Proxy-Connection",
      "Accept-Encoding",
      "TE",
      "Trailer",
      "Keep-Alive",
      "invalid name",
    ] {
      let mut input = request("https://example.invalid/");
      input.headers = vec![HttpHeader {
        name: name.into(),
        value: "value".into(),
      }];
      assert!(validate_request(&input).is_err());
    }
    let mut input = request("https://example.invalid/");
    input.headers = vec![HttpHeader {
      name: "X-Value".into(),
      value: "value\r\nInjected: true".into(),
    }];
    assert!(validate_request(&input).is_err());
    input.headers[0].value = "x".repeat(MAX_REQUEST_HEADER_BYTES);
    assert!(validate_request(&input).is_err());
    input.headers = vec![
      HttpHeader {
        name: "X-Value".into(),
        value: "value".into()
      };
      MAX_REQUEST_HEADERS + 1
    ];
    assert!(validate_request(&input).is_err());
    input.headers.clear();
    input.timeout_ms = 99;
    assert!(validate_request(&input).is_err());
    input.timeout_ms = 120_001;
    assert!(validate_request(&input).is_err());
    input.timeout_ms = default_timeout();
    input.body = Some(vec![0; MAX_BODY_BYTES + 1]);
    assert!(validate_request(&input).is_err());
  }

  #[test]
  fn response_headers_preserve_byte_values_and_enforce_size_limit() {
    let mut headers = HeaderMap::new();
    headers.append("x-bytes", HeaderValue::from_bytes(&[0x80, 0xff]).unwrap());
    let output = response_headers(&headers).unwrap();
    assert_eq!(output[0].value, "\u{80}\u{ff}");
    let mut input = request("https://example.invalid/");
    input.headers = output;
    let parsed = validate_request(&input).unwrap();
    assert_eq!(parsed.headers["x-bytes"].as_bytes(), &[0x80, 0xff]);
    headers.append(
      "x-large",
      HeaderValue::from_bytes(&vec![b'x'; MAX_RESPONSE_HEADER_BYTES]).unwrap(),
    );
    assert!(response_headers(&headers).is_err());
  }
}
