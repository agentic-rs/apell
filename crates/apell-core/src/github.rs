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
const INVALID_TARGET: &str =
  "Enter owner/repository, owner/repository@tag, or a GitHub repository or release URL.";

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubTarget {
  pub repository: String,
  #[serde(default)]
  pub release_tag: Option<String>,
}

impl GitHubTarget {
  pub fn parse(input: &str) -> Result<Self, String> {
    if input.len() > 2048 {
      return Err(INVALID_TARGET.into());
    }
    let input = input.trim();
    let (repository, release_tag) = if let Some((_, remainder)) = input.split_once("://") {
      let url = Url::parse(input).map_err(|_| INVALID_TARGET)?;
      let (authority, path) = remainder.split_once('/').ok_or(INVALID_TARGET)?;
      // Check the original authority and path too. URL parsing normalizes an
      // explicit :443 port, dot segments and backslashes before validation.
      if url.scheme() != "https"
        || !authority.eq_ignore_ascii_case("github.com")
        || url.host_str() != Some("github.com")
        || url.query().is_some()
        || url.fragment().is_some()
        || path.contains('\\')
      {
        return Err(INVALID_TARGET.into());
      }
      let mut segments = path.splitn(3, '/');
      let owner = segments.next().ok_or(INVALID_TARGET)?;
      let repository = segments.next().ok_or(INVALID_TARGET)?;
      let tag = match segments.next() {
        None | Some("") => None,
        Some(suffix) => Some(decode_tag(
          suffix.strip_prefix("releases/tag/").ok_or(INVALID_TARGET)?,
        )?),
      };
      (format!("{owner}/{repository}"), tag)
    } else {
      match input.split_once('@') {
        Some((repository, tag)) => (repository.to_owned(), Some(tag.to_owned())),
        None => (input.to_owned(), None),
      }
    };
    let target = Self {
      repository,
      release_tag,
    };
    target.validate()?;
    Ok(target)
  }

  fn validate(&self) -> Result<(), String> {
    if !valid_repository(&self.repository)
      || self
        .release_tag
        .as_deref()
        .is_some_and(|tag| !valid_release_tag(tag))
    {
      return Err(INVALID_TARGET.into());
    }
    Ok(())
  }
}

fn decode_tag(encoded: &str) -> Result<String, String> {
  let mut decoded = Vec::with_capacity(encoded.len());
  let mut bytes = encoded.bytes();
  while let Some(byte) = bytes.next() {
    if byte == b'%' {
      let high = bytes.next().and_then(|value| (value as char).to_digit(16));
      let low = bytes.next().and_then(|value| (value as char).to_digit(16));
      decoded.push((high.ok_or(INVALID_TARGET)? * 16 + low.ok_or(INVALID_TARGET)?) as u8);
    } else {
      decoded.push(byte);
    }
  }
  String::from_utf8(decoded).map_err(|_| INVALID_TARGET.into())
}

pub(crate) fn valid_release_tag(tag: &str) -> bool {
  !tag.is_empty()
    && tag.len() <= 256
    && !tag.ends_with('.')
    && !tag.contains("..")
    && !tag.contains("@{")
    && !tag
      .chars()
      .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c))
    && tag
      .split('/')
      .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}

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
  pub release_tag: Option<String>,
  pub prerelease: bool,
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
      release_tag: self.release_tag.clone(),
      prerelease: self.prerelease,
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

fn release(
  repo: Repository,
  release: Release,
  selected_tag: Option<&str>,
) -> Result<GitHubRelease, String> {
  if repo.private || repo.id == 0 || !valid_repository(&repo.full_name) {
    return Err("Choose a public GitHub repository.".into());
  }
  if release.draft || release.id == 0 || !valid_release_tag(&release.tag_name) {
    return Err("GitHub did not return a valid published release.".into());
  }
  if let Some(tag) = selected_tag {
    if release.tag_name != tag {
      return Err("GitHub returned a different release tag than requested.".into());
    }
  } else if release.prerelease {
    return Err(
      "GitHub did not return a stable release. Select a release tag to install a prerelease."
        .into(),
    );
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
    return Err("The selected release contains no .apel packages.".into());
  }
  assets.sort_by(|a, b| a.name.cmp(&b.name));
  Ok(GitHubRelease {
    repository_id: repo.id,
    repository: repo.full_name,
    release_id: release.id,
    tag: release.tag_name,
    release_tag: selected_tag.map(str::to_owned),
    prerelease: release.prerelease,
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
            "GitHub could not find the public repository, requested release, or package asset."
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
    self.metadata_url(url).await
  }

  async fn metadata_url<T: DeserializeOwned>(&self, url: Url) -> Result<T, String> {
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

  /// Resolve a latest stable release or an explicitly selected published tag.
  /// Callers choose an asset when the release contains several .apel packages.
  pub async fn resolve(&self, input: &str) -> Result<GitHubRelease, String> {
    self.resolve_target(&GitHubTarget::parse(input)?).await
  }

  pub async fn resolve_target(&self, target: &GitHubTarget) -> Result<GitHubRelease, String> {
    target.validate()?;
    tokio::time::timeout(Duration::from_secs(45), async {
      let repo = self.repository(&target.repository).await?;
      let mut url = Url::parse(&format!(
        "https://api.github.com/repos/{}/releases/",
        repo.full_name
      ))
      .map_err(|_| "Invalid GitHub resource.")?;
      {
        let mut path = url
          .path_segments_mut()
          .map_err(|_| "Invalid GitHub resource.")?;
        path.pop_if_empty();
        if let Some(tag) = &target.release_tag {
          path.push("tags").push(tag);
        } else {
          path.push("latest");
        }
      }
      let metadata = self.metadata_url(url).await?;
      self.verify_repo(&repo.full_name, repo.id).await?;
      release(repo, metadata, target.release_tag.as_deref())
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
  use std::sync::{Arc, Mutex};

  #[test]
  fn parses_repository_and_explicit_release_targets_without_losing_tag_characters() {
    for input in [
      "example/project",
      "https://github.com/example/project",
      "https://github.com/example/project/",
    ] {
      assert_eq!(
        GitHubTarget::parse(input).unwrap(),
        GitHubTarget {
          repository: "example/project".into(),
          release_tag: None,
        }
      );
    }
    for (input, tag) in [
      ("example/project@nightly", "nightly"),
      ("example/project@release/v2", "release/v2"),
      (
        "https://github.com/example/project/releases/tag/release%2Fv2",
        "release/v2",
      ),
      (
        "https://github.com/example/project/releases/tag/release/v2",
        "release/v2",
      ),
      (
        "https://github.com/example/project/releases/tag/v1%2Bpatch",
        "v1+patch",
      ),
      (
        "https://github.com/example/project/releases/tag/100%2525",
        "100%25",
      ),
      (
        "https://github.com/example/project/releases/tag/%E6%B5%8B%E8%AF%95",
        "测试",
      ),
    ] {
      assert_eq!(
        GitHubTarget::parse(input).unwrap(),
        GitHubTarget {
          repository: "example/project".into(),
          release_tag: Some(tag.into()),
        }
      );
    }
  }

  #[test]
  fn rejects_ambiguous_untrusted_and_malformed_release_targets() {
    for input in [
      "example/project@",
      "example/project@/tag",
      "example/project@tag/",
      "example/project@../tag",
      "example/project@tag name",
      "example/project@tag\nname",
      "example/project@tag?query",
      "example/project@tag\\path",
      "http://github.com/example/project",
      "https://api.github.com/example/project",
      "https://github.com.evil.invalid/example/project",
      "https://user@github.com/example/project",
      "https://@github.com/example/project",
      "https://github.com:443/example/project",
      "https://github.com:8443/example/project",
      "https://github.com/example/project?",
      "https://github.com/example/project#",
      "https://github.com/example/project/releases/latest",
      "https://github.com/example/project/releases/tag/",
      "https://github.com/example/project/../project",
      "https://github.com/example/project/releases/tag/%2E%2E/tag",
      "https://github.com/example/project/releases/tag/tag%00",
      "https://github.com/example/project/releases/tag/%FF",
      "https://github.com/example/project/releases/tag/tag%",
      "https://github.com/example/project/releases/tag/tag%2",
      "https://github.com/example/project/releases/tag/tag%GG",
      "https://github.com/example/project/releases/tag/tag?query",
      "https://github.com/example/project/releases/tag/tag#fragment",
      "https://github.com/example/project/releases/tag/tag%5Cpath",
    ] {
      assert!(
        GitHubTarget::parse(input).is_err(),
        "Accepted invalid target {input}"
      );
    }
    assert!(GitHubTarget::parse(&format!("example/project@{}", "a".repeat(257))).is_err());
    assert!(GitHubTarget::parse(&"a".repeat(2049)).is_err());
    assert!(valid_release_tag(&"a".repeat(256)));
  }

  fn release_data(tag: &str) -> serde_json::Value {
    serde_json::json!({
      "id":8, "tag_name":tag, "draft":false, "prerelease":true,
      "assets":[{"id":9,"name":"sample.apel","size":7,"state":"uploaded"}]
    })
  }

  async fn release_fixture(
    status: u16,
    metadata: serde_json::Value,
  ) -> (
    GitHubClient,
    Arc<Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
  ) {
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
          let count = stream.read(&mut buffer).await.unwrap();
          assert!(count > 0 && headers.len() < 16 * 1024);
          headers.extend_from_slice(&buffer[..count]);
        }
        let request = String::from_utf8(headers).unwrap();
        let target = request
          .lines()
          .next()
          .unwrap()
          .split_whitespace()
          .nth(1)
          .unwrap();
        received.lock().unwrap().push(target.to_owned());
        let (response_status, body) = if target == "/repos/example/project" {
          (
            200,
            serde_json::json!({"id":7,"private":false,"full_name":"example/project"}),
          )
        } else {
          (status, metadata.clone())
        };
        let body = body.to_string();
        let data = format!("HTTP/1.1 {response_status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        stream.write_all(data.as_bytes()).await.unwrap();
      }
    });
    let mut github = GitHubClient::new().unwrap();
    github.fixture_origin = Some(Url::parse(&format!("http://{address}/")).unwrap());
    (github, requests, server)
  }

  #[tokio::test]
  async fn explicit_tags_select_prereleases_and_encode_the_entire_tag_as_one_segment() {
    for (tag, encoded) in [
      ("nightly", "nightly"),
      ("release/v2", "release%2Fv2"),
      ("tag%2Fname", "tag%252Fname"),
    ] {
      let (github, requests, server) = release_fixture(200, release_data(tag)).await;
      let release = github
        .resolve(&format!("example/project@{tag}"))
        .await
        .unwrap();
      assert_eq!(release.release_tag.as_deref(), Some(tag));
      assert_eq!(release.tag, tag);
      assert!(release.prerelease);
      let source = release.source("sample.apel").unwrap();
      assert_eq!(source.release_tag.as_deref(), Some(tag));
      assert!(source.prerelease);
      assert_eq!(
        *requests.lock().unwrap(),
        [
          "/repos/example/project".to_owned(),
          format!("/repos/example/project/releases/tags/{encoded}"),
          "/repos/example/project".to_owned(),
        ]
      );
      server.abort();
    }
  }

  #[tokio::test]
  async fn tag_resolution_rejects_drafts_and_mismatched_release_tags() {
    let mut draft = release_data("nightly");
    draft["draft"] = true.into();
    for metadata in [draft, release_data("different-tag")] {
      let (github, requests, server) = release_fixture(200, metadata).await;
      assert!(github.resolve("example/project@nightly").await.is_err());
      assert!(requests
        .lock()
        .unwrap()
        .iter()
        .all(|path| !path.ends_with("/latest")));
      server.abort();
    }
  }

  #[tokio::test]
  async fn missing_or_prerelease_latest_never_falls_back_to_a_tag_or_release_list() {
    for status in [404, 200] {
      let (github, requests, server) = release_fixture(status, release_data("nightly")).await;
      let error = github.resolve("example/project").await.unwrap_err();
      if status == 404 {
        assert!(error.contains("requested release"));
      } else {
        assert!(error.contains("stable release"));
      }
      let requests = requests.lock().unwrap();
      assert_eq!(
        requests
          .iter()
          .filter(|path| path.contains("/releases"))
          .collect::<Vec<_>>(),
        [&"/repos/example/project/releases/latest".to_owned()]
      );
      server.abort();
    }
  }

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
    let resolved = release(repo(), serde_json::from_value(data()).unwrap(), None).unwrap();
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
    assert!(release(repo(), serde_json::from_value(unstable).unwrap(), None).is_err());
    let mut unverified = data();
    unverified["assets"][1]["digest"] = "unknown:value".into();
    assert!(release(repo(), serde_json::from_value(unverified).unwrap(), None).is_err());
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
