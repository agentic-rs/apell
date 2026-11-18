use super::super::*;
use std::{
  io::{Cursor, Write},
  sync::{Arc, Mutex},
};
use tokio::{
  io::{AsyncReadExt, AsyncWriteExt},
  net::TcpListener,
  task::JoinHandle,
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

pub fn package(id: &str, version: &str, permissions: &[&str]) -> Vec<u8> {
  let manifest = serde_json::to_vec(&serde_json::json!({"schema_version":1,"api_version":2,"applet_id":id,"name":"Sample","version":version,"permissions":permissions})).unwrap();
  let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
  for (path, bytes) in [
    ("manifest.json", manifest.as_slice()),
    ("index.html", b"<h1>Sample</h1>".as_slice()),
  ] {
    zip
      .start_file(
        path,
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
      )
      .unwrap();
    zip.write_all(bytes).unwrap();
  }
  zip.finish().unwrap().into_inner()
}

pub fn release(bytes: &[u8], version: &str) -> StoreRelease {
  StoreRelease {
    version: version.into(),
    sha256: digest(bytes),
    size_bytes: bytes.len() as u64,
    published_at: "2026-01-01T00:00:00Z".into(),
    release_notes: "Sample release".into(),
    api_version: 2,
    permissions: vec!["sqlite".into()],
    network: true,
    has_backend: false,
    download_url: format!("/api/v1/applets/sample/versions/{version}/download"),
    prerelease: version.contains('-'),
  }
}
pub fn applet(release: &StoreRelease) -> StoreApplet {
  StoreApplet {
    applet_id: "sample".into(),
    name: "Sample".into(),
    description: "A sample applet".into(),
    project_url: None,
    icon_url: None,
    latest_release: release.clone(),
    latest_stable_release: (!release.prerelease).then(|| release.clone()),
    latest_nightly_release: release.prerelease.then(|| release.clone()),
  }
}
pub fn detail(release: &StoreRelease) -> StoreDetail {
  StoreDetail {
    applet: applet(release),
    releases: vec![release.clone()],
  }
}

#[derive(Clone)]
pub struct Reply {
  status: u16,
  headers: Vec<(String, String)>,
  body: Vec<u8>,
}
impl Reply {
  pub fn status(status: u16, body: Vec<u8>) -> Self {
    Self {
      status,
      headers: Vec::new(),
      body,
    }
  }
  pub fn ok(body: Vec<u8>) -> Self {
    Self::status(200, body)
  }
  pub fn json(value: &impl serde::Serialize) -> Self {
    Self::ok(serde_json::to_vec(value).unwrap()).header("Content-Type", "application/json")
  }
  pub fn header(mut self, name: &str, value: &str) -> Self {
    self.headers.push((name.into(), value.into()));
    self
  }
}
pub struct MockStore {
  pub origin: String,
  pub requests: Arc<Mutex<Vec<String>>>,
  task: JoinHandle<()>,
}
impl MockStore {
  pub async fn new(routes: Vec<(String, Reply)>) -> Self {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let task = tokio::spawn(async move {
      let mut routes = routes;
      loop {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
          let mut byte = [0u8];
          if stream.read_exact(&mut byte).await.is_err() {
            break;
          }
          request.push(byte[0]);
          if request.len() > 16_384 {
            break;
          }
        }
        let request = String::from_utf8(request).unwrap();
        let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
        captured.lock().unwrap().push(request);
        let missing = Reply::status(404, Vec::new());
        let reply = match routes.iter().position(|(route, _)| route == &path) {
          // Repeated routes return their queued replies, then retain the last.
          Some(index) if routes[index + 1..].iter().any(|(route, _)| route == &path) => {
            routes.remove(index).1
          }
          Some(index) => routes[index].1.clone(),
          None => missing,
        };
        let mut headers = format!("HTTP/1.1 {} Status\r\nConnection: close\r\n", reply.status);
        if !reply
          .headers
          .iter()
          .any(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        {
          headers.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
        }
        for (name, value) in &reply.headers {
          headers.push_str(&format!("{name}: {value}\r\n"));
        }
        headers.push_str("\r\n");
        if stream.write_all(headers.as_bytes()).await.is_ok() {
          let _ = stream.write_all(&reply.body).await;
        }
      }
    });
    Self {
      origin,
      requests,
      task,
    }
  }
  pub async fn release(selected: &StoreRelease, bytes: Vec<u8>) -> Self {
    Self::new(vec![
      (
        "/api/v1/applets/sample".into(),
        Reply::json(&detail(selected)),
      ),
      (selected.download_url.clone(), Reply::ok(bytes)),
    ])
    .await
  }
}
impl Drop for MockStore {
  fn drop(&mut self) {
    self.task.abort();
  }
}
