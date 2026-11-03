//! Public GitHub release discovery. Only release metadata chooses the asset ID;
//! URLs, credentials, and redirect destinations are never supplied by applets.
use crate::bundles::{digest, valid_hash, GitHubSource, MAX_BUNDLE_BYTES};
use reqwest::{
  header::{ACCEPT, LOCATION},
  Client, Response,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{collections::HashSet, time::Duration};
use url::Url;

const MAX_METADATA: usize = 2 * 1024 * 1024;
const MAX_REDIRECTS: usize = 4;

pub fn valid_repository(name: &str) -> bool {
  let Some((owner, repo)) = name.split_once('/') else {
    return false;
  };
  !owner.is_empty()
    && owner.len() <= 39
    && owner
      .bytes()
      .all(|c| c.is_ascii_alphanumeric() || c == b'-')
    && !owner.starts_with('-')
    && !owner.ends_with('-')
    && !repo.is_empty()
    && repo.len() <= 100
    && repo != "."
    && repo != ".."
    && repo
      .bytes()
      .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}

pub(crate) fn valid_asset_name(name: &str) -> bool {
  name.len() > 5
    && name.len() <= 255
    && name.ends_with(".apel")
    && !name.starts_with('.')
    && !name.chars().any(|c| c.is_control() || "/\\:".contains(c))
}

#[derive(Clone, Debug, Serialize)]
pub struct ReleaseAsset {
  pub asset_id: u64,
  pub name: String,
  pub size_bytes: u64,
  pub sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GitHubRelease {
  pub repository_id: u64,
  pub repository: String,
  pub release_id: u64,
  pub tag: String,
  pub assets: Vec<ReleaseAsset>,
}

impl GitHubRelease {
  fn asset(&self, name: &str) -> Result<&ReleaseAsset, String> {
    self
      .assets
      .iter()
      .find(|asset| asset.name == name)
      .ok_or_else(|| "That package asset is not in the selected release.".into())
  }

  pub fn source(&self, asset_name: &str) -> Result<GitHubSource, String> {
    let asset = self.asset(asset_name)?;
    let source = GitHubSource {
      repository_id: self.repository_id,
      repository: self.repository.clone(),
      asset_name: asset.name.clone(),
    };
    source.validate()?;
    Ok(source)
  }
}

#[derive(Deserialize)]
struct Repository {
  id: u64,
  full_name: String,
  private: bool,
}
#[derive(Deserialize)]
struct Release {
  id: u64,
  tag_name: String,
  draft: bool,
  prerelease: bool,
  assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
  id: u64,
  name: String,
  size: u64,
  state: String,
  digest: Option<String>,
}

fn release(repo: Repository, release: Release) -> Result<GitHubRelease, String> {
  if repo.private || repo.id == 0 || !valid_repository(&repo.full_name) {
    return Err("Choose a public GitHub repository.".into());
  }
  if release.draft
    || release.prerelease
    || release.id == 0
    || release.tag_name.is_empty()
    || release.tag_name.len() > 256
    || release.tag_name.chars().any(char::is_control)
  {
    return Err("GitHub did not return a stable release.".into());
  }
  if release.assets.len() > 1024 {
    return Err("The GitHub release contains too many assets.".into());
  }
  let mut names = HashSet::new();
  let mut assets = Vec::new();
  for asset in release.assets {
    if asset.state != "uploaded" || !asset.name.ends_with(".apel") {
      continue;
    }
    if asset.id == 0 || !valid_asset_name(&asset.name) || !names.insert(asset.name.to_lowercase()) {
      return Err("GitHub returned an invalid or ambiguous package asset.".into());
    }
    let sha256 = asset
      .digest
      .map(|value| {
        value
          .strip_prefix("sha256:")
          .filter(|hash| valid_hash(hash))
          .map(str::to_ascii_lowercase)
          .ok_or("GitHub returned an unsupported asset digest.")
      })
      .transpose()?;
    assets.push(ReleaseAsset {
      asset_id: asset.id,
      name: asset.name,
      size_bytes: asset.size,
      sha256,
    });
  }
  if assets.is_empty() {
    return Err("The latest stable release contains no .apel packages.".into());
  }
  assets.sort_by(|a, b| a.name.cmp(&b.name));
  Ok(GitHubRelease {
    repository_id: repo.id,
    repository: repo.full_name,
    release_id: release.id,
    tag: release.tag_name,
    assets,
  })
}

fn allowed_url(url: &Url, download: bool) -> bool {
  url.scheme() == "https"
    && url.port_or_known_default() == Some(443)
    && url.username().is_empty()
    && url.password().is_none()
    && url.fragment().is_none()
    && (url.host_str() == Some("api.github.com")
      || download
        && matches!(
          url.host_str(),
          Some(
            "github.com"
              | "release-assets.githubusercontent.com"
              | "objects.githubusercontent.com"
              | "github-releases.githubusercontent.com"
          )
        ))
}

pub struct GitHubClient {
  client: Client,
  #[cfg(test)]
  fixture_origin: Option<Url>,
}

impl GitHubClient {
  pub fn new() -> Result<Self, String> {
    let client = crate::http::client_builder()?
      .redirect(reqwest::redirect::Policy::none())
      .connect_timeout(Duration::from_secs(15))
      .read_timeout(Duration::from_secs(30))
      .timeout(Duration::from_secs(120))
      .user_agent("Apell")
      .build()
      .map_err(|_| "Could not initialize GitHub networking.".to_owned())?;
    Ok(Self {
      client,
      #[cfg(test)]
      fixture_origin: None,
    })
  }

  async fn response(&self, mut url: Url, download: bool) -> Result<Response, String> {
    for hop in 0..=MAX_REDIRECTS {
      if !allowed_url(&url, download) {
        return Err("GitHub redirected to an unsupported destination.".into());
      }
      #[cfg(test)]
      let request_url = self.fixture_origin.as_ref().map_or_else(
        || url.clone(),
        |origin| {
          let mut fixture = origin.clone();
          fixture.set_path(url.path());
          fixture.set_query(url.query());
          fixture
        },
      );
      #[cfg(not(test))]
      let request_url = url.clone();
      let response = self
        .client
        .get(request_url)
        .header(
          ACCEPT,
          if download {
            "application/octet-stream"
          } else {
            "application/vnd.github+json"
          },
        )
        .send()
        .await
        .map_err(|_| "Could not reach GitHub.".to_owned())?;
      let status = response.status().as_u16();
      if status == 200 {
        return Ok(response);
      }
      if !matches!(status, 301 | 302 | 303 | 307 | 308) {
        return Err(match status {
          403 | 429 => "GitHub denied the request or its public API rate limit was reached.".into(),
          404 => {
            "GitHub could not find the public repository, latest stable release, or package asset."
              .into()
          }
          _ => format!("GitHub returned HTTP {status}."),
        });
      }
      if hop == MAX_REDIRECTS {
        return Err("Too many GitHub redirects.".into());
      }
      let target = response
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.len() <= 16 * 1024)
        .ok_or("GitHub returned an invalid redirect.")?;
      url = url
        .join(target)
        .map_err(|_| "GitHub returned an invalid redirect.")?;
    }
    unreachable!()
  }

  async fn metadata<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
    let url = Url::parse(&format!("https://api.github.com/{path}"))
      .map_err(|_| "Invalid GitHub resource.")?;
    let response = self.response(url, false).await?;
    let bytes = bounded_body(response, MAX_METADATA, |_, _| {}).await?;
    serde_json::from_slice(&bytes).map_err(|_| "GitHub returned invalid metadata.".into())
  }

  async fn repository(&self, name: &str) -> Result<Repository, String> {
    if !valid_repository(name) {
      return Err("Enter a GitHub repository as owner/repository.".into());
    }
    let repo: Repository = self.metadata(&format!("repos/{name}")).await?;
    if repo.private || repo.id == 0 || !valid_repository(&repo.full_name) {
      return Err("Choose a public GitHub repository.".into());
    }
    Ok(repo)
  }

  async fn verify_repo(&self, name: &str, id: u64) -> Result<(), String> {
    if self.repository(name).await?.id != id {
      return Err("The GitHub repository identity changed. The applet was preserved.".into());
    }
    Ok(())
  }

  /// Resolve the latest published non-prerelease. Callers present a choice
  /// when more than one .apel is attached and may select a single asset directly.
  pub async fn resolve(&self, repository: &str) -> Result<GitHubRelease, String> {
    tokio::time::timeout(Duration::from_secs(45), async {
      let repo = self.repository(repository).await?;
      let metadata = self
        .metadata(&format!("repos/{}/releases/latest", repo.full_name))
        .await?;
      self.verify_repo(&repo.full_name, repo.id).await?;
      release(repo, metadata)
    })
    .await
    .map_err(|_| "GitHub release lookup timed out.".to_owned())?
  }

  /// Fetch only a numeric asset ID from this resolved release. Never follow a
  /// caller-supplied URL or forward provider credentials to GitHub/CDN hosts.
  pub async fn download(
    &self,
    release: &GitHubRelease,
    asset_name: &str,
    progress: impl Fn(u64, Option<u64>),
  ) -> Result<Vec<u8>, String> {
    tokio::time::timeout(Duration::from_secs(120), async {
      let asset = release.asset(asset_name)?;
      if asset.size_bytes == 0 || asset.size_bytes > MAX_BUNDLE_BYTES as u64 {
        return Err("The release package is empty or exceeds 24 MiB.".into());
      }
      self
        .verify_repo(&release.repository, release.repository_id)
        .await?;
      let url = Url::parse(&format!(
        "https://api.github.com/repos/{}/releases/assets/{}",
        release.repository, asset.asset_id
      ))
      .map_err(|_| "Invalid GitHub package asset.")?;
      let response = self.response(url, true).await?;
      let bytes = bounded_body(response, asset.size_bytes as usize, |received, _| {
        progress(received, Some(asset.size_bytes))
      })
      .await?;
      if bytes.len() as u64 != asset.size_bytes
        || asset
          .sha256
          .as_ref()
          .is_some_and(|sha| sha != &digest(&bytes))
      {
        return Err("The GitHub package size or digest does not match its release.".into());
      }
      self
        .verify_repo(&release.repository, release.repository_id)
        .await?;
      Ok(bytes)
    })
    .await
    .map_err(|_| "GitHub package download timed out.".to_owned())?
  }
}

async fn bounded_body(
  mut response: Response,
  limit: usize,
  progress: impl Fn(u64, Option<u64>),
) -> Result<Vec<u8>, String> {
  let total = response.content_length();
  if total.is_some_and(|n| n > limit as u64) {
    return Err("GitHub response exceeds its size limit.".into());
  }
  let mut bytes = Vec::new();
  while let Some(chunk) = response
    .chunk()
    .await
    .map_err(|_| "GitHub download was interrupted.".to_owned())?
  {
    if chunk.len() > limit.saturating_sub(bytes.len()) {
      return Err("GitHub response exceeds its size limit.".into());
    }
    bytes.extend_from_slice(&chunk);
    progress(bytes.len() as u64, total);
  }
  Ok(bytes)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn discovers_and_downloads_a_selected_release_asset_through_a_bounded_redirect() {
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let received = requests.clone();
    let server = tokio::spawn(async move {
      loop {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut headers = Vec::new();
        while !headers.windows(4).any(|end| end == b"\r\n\r\n") {
          let mut buffer = [0; 1024];
          let n = stream.read(&mut buffer).await.unwrap();
          if n == 0 {
            break;
          }
          headers.extend_from_slice(&buffer[..n]);
          assert!(headers.len() < 16 * 1024);
        }
        let request = String::from_utf8(headers).unwrap();
        let target = request
          .lines()
          .next()
          .unwrap()
          .split_whitespace()
          .nth(1)
          .unwrap();
        let (status, extra, body) = match target {
          "/repos/example/project" => ("200 OK", "", r#"{"id":7,"private":false,"full_name":"example/project"}"#.to_owned()),
          "/repos/example/project/releases/latest" => ("200 OK", "", serde_json::json!({"id":8,"tag_name":"v1","draft":false,"prerelease":false,"assets":[{"id":9,"name":"sample.apel","size":7,"digest":format!("sha256:{}",digest(b"fixture")),"state":"uploaded"}]}).to_string()),
          "/repos/example/project/releases/assets/9" => ("302 Found", "Location: https://release-assets.githubusercontent.com/download?temporary=value\r\n", String::new()),
          "/download?temporary=value" => ("200 OK", "", "fixture".into()),
          _ => panic!("Unexpected release fixture route"),
        };
        received.lock().unwrap().push(request.to_ascii_lowercase());
        let data = format!(
          "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
          body.len()
        );
        stream.write_all(data.as_bytes()).await.unwrap();
      }
    });
    let mut github = GitHubClient::new().unwrap();
    github.fixture_origin = Some(Url::parse(&format!("http://{address}/")).unwrap());
    let mut release = github.resolve("example/project").await.unwrap();
    assert_eq!(release.release_id, 8);
    assert_eq!(release.source("sample.apel").unwrap().repository_id, 7);
    assert_eq!(
      github
        .download(&release, "sample.apel", |n, total| assert!(
          n <= total.unwrap()
        ))
        .await
        .unwrap(),
      b"fixture"
    );
    release.assets[0].sha256 = Some("0".repeat(64));
    assert!(github
      .download(&release, "sample.apel", |_, _| {})
      .await
      .unwrap_err()
      .contains("digest"));
    let requests = requests.lock().unwrap();
    assert!(requests.iter().any(|r| r
      .starts_with("get /repos/example/project/releases/assets/9 ")
      && r.contains("accept: application/octet-stream")));
    assert!(requests
      .iter()
      .all(|r| !r.contains("authorization:") && !r.contains("cookie:")));
    server.abort();
  }

  #[test]
  fn release_selection_uses_only_uploaded_package_assets_from_a_stable_public_release() {
    let repo = || Repository {
      id: 10,
      full_name: "example/project".into(),
      private: false,
    };
    let data = || {
      serde_json::json!({"id":20,"tag_name":"v1","draft":false,"prerelease":false,"assets":[
      {"id":1,"name":"source.zip","size":10,"state":"uploaded"},
      {"id":2,"name":"b.apel","size":20,"state":"uploaded","digest":null},
      {"id":3,"name":"a.apel","size":30,"state":"uploaded","digest":format!("sha256:{}","A".repeat(64))},
      {"id":4,"name":"pending.apel","size":0,"state":"starter"}]})
    };
    let resolved = release(repo(), serde_json::from_value(data()).unwrap()).unwrap();
    assert_eq!(
      resolved
        .assets
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>(),
      ["a.apel", "b.apel"]
    );
    assert_eq!(resolved.assets[0].sha256, Some("a".repeat(64)));
    assert!(resolved.assets[1].sha256.is_none());
    assert_eq!(resolved.source("b.apel").unwrap().repository_id, 10);
    assert!(resolved.source("elsewhere.apel").is_err());
    let mut unstable = data();
    unstable["prerelease"] = true.into();
    assert!(release(repo(), serde_json::from_value(unstable).unwrap()).is_err());
    let mut unverified = data();
    unverified["assets"][1]["digest"] = "unknown:value".into();
    assert!(release(repo(), serde_json::from_value(unverified).unwrap()).is_err());
  }

  #[test]
  fn redirects_and_repository_inputs_cannot_select_credentials_or_arbitrary_hosts() {
    for invalid in [
      "owner/../repo",
      "https://github.com/example/project",
      "owner/repo?query",
      "-owner/repo",
      "owner/..",
    ] {
      assert!(!valid_repository(invalid));
    }
    assert!(valid_repository("example/project-test_1"));
    for url in [
      "http://github.com/file",
      "https://github.com.evil.invalid/file",
      "https://user@github.com/file",
      "https://127.0.0.1/file",
      "https://release-assets.githubusercontent.com:8443/file",
      "file:///tmp/file",
    ] {
      assert!(!allowed_url(&Url::parse(url).unwrap(), true));
    }
    let signed =
      Url::parse("https://release-assets.githubusercontent.com/file?signature=value").unwrap();
    assert!(allowed_url(&signed, true));
    assert!(!allowed_url(&signed, false));
  }
}
