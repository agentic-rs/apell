pub mod auth;
mod config;
mod error;
mod management;
mod model;
mod oidc;
pub mod packages;
mod storage;

use config::validate_token;
pub use config::Config;
pub use error::{Result, StoreError};
pub use model::{Applet, AppletDetail, Catalog, PublishMetadata, PublishResult, Release};
pub use oidc::{OidcConfig, TrustedPublisher};
pub use storage::Store;

use axum::{
  body::Body,
  extract::{DefaultBodyLimit, FromRequest, Multipart, Path, Request, State},
  http::{header, HeaderMap, HeaderValue, StatusCode},
  middleware::{self, Next},
  response::{IntoResponse, Response},
  routing::{any, get, post},
  Json, Router,
};
use futures_util::StreamExt;
use model::{MAX_ICON_BYTES, MAX_METADATA_BYTES};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
use subtle::ConstantTimeEq;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::io::ReaderStream;
use tower_http::services::{ServeDir, ServeFile};

const MAX_REQUEST_BYTES: usize =
  apell_package::MAX_BUNDLE_BYTES + MAX_ICON_BYTES + MAX_METADATA_BYTES + 64 * 1024;

#[derive(Clone)]
pub(crate) struct AppState {
  store: Store,
  token_hash: [u8; 32],
  oidc: Option<oidc::Oidc>,
  auth: Option<Arc<auth::Auth>>,
  uploads: Arc<Semaphore>,
  queries: Arc<Semaphore>,
  downloads: Arc<Semaphore>,
}

pub fn router(config: &Config) -> Result<Router> {
  let store = initialized_store(config)?;
  let oidc = config
    .oidc
    .clone()
    .map(|policy| oidc::Oidc::new(policy, store.clone()))
    .transpose()?;
  router_with_store(config, store, oidc)
}

fn initialized_store(config: &Config) -> Result<Store> {
  let store = Store::open(&config.data_dir)?;
  let publishers = config
    .oidc
    .as_ref()
    .map_or(&[][..], |oidc| oidc.publishers.as_slice());
  store.bootstrap_packages(publishers, config.bootstrap_owner.as_ref())?;
  Ok(store)
}

#[cfg(test)]
fn router_with_identity(config: &Config, oidc: Option<oidc::Oidc>) -> Result<Router> {
  router_with_store(config, initialized_store(config)?, oidc)
}

fn router_with_store(config: &Config, store: Store, oidc: Option<oidc::Oidc>) -> Result<Router> {
  validate_token(&config.publish_token).map_err(StoreError::invalid)?;
  let auth = config
    .oauth
    .clone()
    .map(|oauth| auth::Auth::new(oauth, store.clone()).map(Arc::new))
    .transpose()?;
  let state = AppState {
    store,
    token_hash: Sha256::digest(config.publish_token.as_bytes()).into(),
    oidc,
    auth,
    uploads: Arc::new(Semaphore::new(1)),
    queries: Arc::new(Semaphore::new(4)),
    downloads: Arc::new(Semaphore::new(8)),
  };
  let api = Router::new()
    .route("/", any(not_found))
    .route("/v1/applets", get(catalog))
    .route("/v1/applets/{id}", get(detail))
    .route(
      "/v1/applets/{id}/versions/{version}/download",
      get(download),
    )
    .route("/v1/publish", post(publish))
    .route("/v1/auth/github-actions", post(exchange_identity))
    .merge(auth::routes())
    .merge(management::routes())
    .fallback(not_found);
  let media = Router::new()
    .route("/", any(not_found))
    .route("/{file}", get(icon))
    .fallback(not_found);
  Ok(
    Router::new()
      .route(
        "/healthz",
        get(|| async { Json(serde_json::json!({ "status": "ok" })) }),
      )
      .nest("/api", api)
      .nest("/media", media)
      .route("/api/", any(not_found))
      .route("/media/", any(not_found))
      .fallback_service(
        ServeDir::new(&config.web_dir)
          .not_found_service(ServeFile::new(config.web_dir.join("index.html"))),
      )
      .method_not_allowed_fallback(|| async {
        StoreError::new(
          StatusCode::METHOD_NOT_ALLOWED,
          "method_not_allowed",
          "This HTTP method is not supported.",
        )
      })
      .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
      .layer(middleware::from_fn(response_headers))
      .with_state(state),
  )
}

async fn response_headers(request: Request, next: Next) -> Response {
  let authentication = request.uri().path().starts_with("/api/v1/auth/")
    || request.uri().path().starts_with("/api/v1/account/");
  let mut response = next.run(request).await;
  let headers = response.headers_mut();
  headers.insert(
    header::X_CONTENT_TYPE_OPTIONS,
    HeaderValue::from_static("nosniff"),
  );
  headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
  headers.insert(
    header::REFERRER_POLICY,
    HeaderValue::from_static("no-referrer"),
  );
  headers.insert(
    "permissions-policy",
    HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
  );
  headers.insert(
    "cross-origin-resource-policy",
    HeaderValue::from_static("same-origin"),
  );
  headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"));
  if authentication {
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
  } else if !headers.contains_key(header::CACHE_CONTROL) {
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
  }
  response
}

async fn not_found() -> StoreError {
  StoreError::missing()
}

async fn query<T: Send + 'static>(
  state: &AppState,
  operation: impl FnOnce(Store) -> Result<T> + Send + 'static,
) -> Result<T> {
  let permit = state
    .queries
    .clone()
    .try_acquire_owned()
    .map_err(|_| StoreError::busy())?;
  let store = state.store.clone();
  tokio::task::spawn_blocking(move || {
    let _permit = permit;
    operation(store)
  })
  .await
  .map_err(|_| StoreError::internal())?
}

async fn catalog(State(state): State<AppState>) -> Result<Json<Catalog>> {
  query(&state, |store| store.catalog()).await.map(Json)
}

async fn detail(
  State(state): State<AppState>,
  Path(id): Path<String>,
) -> Result<Json<AppletDetail>> {
  query(&state, move |store| store.detail(&id))
    .await
    .map(Json)
}

fn authenticate(headers: &HeaderMap, expected: &[u8; 32]) -> Result<()> {
  let unauthorized = || {
    StoreError::new(
      StatusCode::UNAUTHORIZED,
      "unauthorized",
      "A valid publishing token is required.",
    )
  };
  if headers.get_all(header::AUTHORIZATION).iter().count() != 1 {
    return Err(unauthorized());
  }
  let value = headers
    .get(header::AUTHORIZATION)
    .and_then(|value| value.to_str().ok())
    .ok_or_else(unauthorized)?;
  let token = value.strip_prefix("Bearer ").ok_or_else(unauthorized)?;
  if validate_token(token).is_err() {
    return Err(unauthorized());
  }
  let hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
  if !bool::from(hash.ct_eq(expected)) {
    return Err(unauthorized());
  }
  Ok(())
}

async fn exchange_identity(State(state): State<AppState>, request: Request) -> Result<Response> {
  let oidc = state.oidc.as_ref().ok_or_else(|| {
    StoreError::new(
      StatusCode::UNAUTHORIZED,
      "unauthorized",
      "Trusted publishing is not configured.",
    )
  })?;
  Ok(Json(oidc.exchange(request).await?).into_response())
}

async fn publish(State(state): State<AppState>, request: Request) -> Result<Response> {
  publish_request(state, request, None).await
}

async fn publish_request(
  state: AppState,
  request: Request,
  expected_id: Option<String>,
) -> Result<Response> {
  use packages::PublishAuthority;
  if expected_id.is_none()
    && state.auth.is_none()
    && !request.headers().contains_key(header::AUTHORIZATION)
  {
    authenticate(request.headers(), &state.token_hash)?;
  }
  let (grant, authority) =
    if expected_id.is_some() || !request.headers().contains_key(header::AUTHORIZATION) {
      let user = management::session_user(&state, request.headers(), true).await?;
      if let Some(id) = expected_id.clone() {
        let actor_id = user.github_id;
        query(&state, move |store| store.package_details(&id, actor_id)).await?;
      }
      (None, PublishAuthority::User(user.github_id))
    } else {
      match authenticate(request.headers(), &state.token_hash) {
        Ok(()) => (None, PublishAuthority::Administrator),
        Err(error) => {
          let grant = state
            .oidc
            .as_ref()
            .ok_or(error)?
            .authenticate(request.headers())?;
          let authority = grant.authority();
          (Some(grant), authority)
        }
      }
    };
  let check_authority = authority.clone();
  query(&state, move |store| {
    store.authorize_publish(&check_authority)
  })
  .await?;
  let permit = state
    .uploads
    .clone()
    .try_acquire_owned()
    .map_err(|_| StoreError::busy())?;
  let parsed = tokio::time::timeout(Duration::from_secs(120), read_upload(request, &state))
    .await
    .map_err(|_| {
      StoreError::new(
        StatusCode::REQUEST_TIMEOUT,
        "upload_timeout",
        "The upload took too long.",
      )
    })??;
  let store = state.store.clone();
  let result = tokio::task::spawn_blocking(move || {
    let _permit = permit;
    if let Some(grant) = grant {
      grant.validate_package(&parsed.package, oidc::now())?;
    }
    if let Some(id) = expected_id {
      let package = apell_package::parse(&parsed.package).map_err(StoreError::invalid)?;
      if package.manifest.applet_id != id {
        return Err(StoreError::invalid(
          "This archive belongs to a different package.",
        ));
      }
    }
    store.publish_authorized(parsed.package, parsed.metadata, parsed.icon, authority)
  })
  .await
  .map_err(|_| StoreError::internal())??;
  Ok(
    (
      if result.created {
        StatusCode::CREATED
      } else {
        StatusCode::OK
      },
      Json(result),
    )
      .into_response(),
  )
}

struct Upload {
  package: Vec<u8>,
  metadata: PublishMetadata,
  icon: Option<Vec<u8>>,
}

async fn read_upload(request: Request, state: &AppState) -> Result<Upload> {
  let mut multipart = Multipart::from_request(request, state).await.map_err(|_| {
    StoreError::invalid("Use multipart/form-data with package and metadata fields.")
  })?;
  let mut package = None;
  let mut metadata = None;
  let mut icon = None;
  while let Some(mut field) = multipart.next_field().await.map_err(upload_error)? {
    let name = field
      .name()
      .ok_or_else(|| StoreError::invalid("Every upload field needs a name."))?
      .to_owned();
    let limit = match name.as_str() {
      "package" if package.is_none() => apell_package::MAX_BUNDLE_BYTES,
      "metadata" if metadata.is_none() => MAX_METADATA_BYTES,
      "icon" if icon.is_none() => MAX_ICON_BYTES,
      _ => {
        return Err(StoreError::invalid(
          "Upload contains an unknown or duplicate field.",
        ))
      }
    };
    let mut bytes = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(upload_error)? {
      if bytes.len().saturating_add(chunk.len()) > limit {
        return Err(StoreError::new(
          StatusCode::PAYLOAD_TOO_LARGE,
          "upload_too_large",
          "An upload field exceeds its size limit.",
        ));
      }
      bytes.extend_from_slice(&chunk);
    }
    match name.as_str() {
      "package" => package = Some(bytes),
      "metadata" => {
        metadata = Some(
          serde_json::from_slice::<PublishMetadata>(&bytes).map_err(|_| {
            StoreError::invalid(
            "Metadata must be valid JSON with description, optional project_url and release_notes.",
          )
          })?,
        )
      }
      "icon" => icon = Some(bytes),
      _ => unreachable!(),
    }
  }
  Ok(Upload {
    package: package.ok_or_else(|| StoreError::invalid("A package field is required."))?,
    metadata: metadata.ok_or_else(|| StoreError::invalid("A metadata field is required."))?,
    icon,
  })
}

fn upload_error(error: axum::extract::multipart::MultipartError) -> StoreError {
  if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
    StoreError::new(
      StatusCode::PAYLOAD_TOO_LARGE,
      "upload_too_large",
      "Upload exceeds its size limit.",
    )
  } else {
    StoreError::invalid("The multipart upload is malformed or incomplete.")
  }
}

async fn download(
  State(state): State<AppState>,
  Path((id, version)): Path<(String, String)>,
  headers: HeaderMap,
) -> Result<Response> {
  let permit = state
    .downloads
    .clone()
    .try_acquire_owned()
    .map_err(|_| StoreError::busy())?;
  let store = state.store.clone();
  let download_id = id.clone();
  let (file, release, permit) = tokio::task::spawn_blocking(move || {
    let (file, release) = store.download(&download_id, &version)?;
    Ok::<_, StoreError>((file, release, permit))
  })
  .await
  .map_err(|_| StoreError::internal())??;
  let filename = format!("{id}-{}.apel", release.version);
  file_response(
    file,
    release.size_bytes,
    &release.sha256,
    "application/octet-stream",
    Some(&filename),
    permit,
    &headers,
  )
}

async fn icon(
  State(state): State<AppState>,
  Path(file): Path<String>,
  headers: HeaderMap,
) -> Result<Response> {
  let hash = file
    .strip_suffix(".png")
    .ok_or_else(StoreError::missing)?
    .to_owned();
  let permit = state
    .downloads
    .clone()
    .try_acquire_owned()
    .map_err(|_| StoreError::busy())?;
  let store = state.store.clone();
  let expected = hash.clone();
  let (file, size, permit) = tokio::task::spawn_blocking(move || {
    let (file, size) = store.icon(&expected)?;
    Ok::<_, StoreError>((file, size, permit))
  })
  .await
  .map_err(|_| StoreError::internal())??;
  file_response(file, size, &hash, "image/png", None, permit, &headers)
}

fn file_response(
  file: std::fs::File,
  size: u64,
  hash: &str,
  content_type: &'static str,
  filename: Option<&str>,
  permit: OwnedSemaphorePermit,
  request_headers: &HeaderMap,
) -> Result<Response> {
  let etag = format!("\"{hash}\"");
  let not_modified = request_headers
    .get(header::IF_NONE_MATCH)
    .and_then(|value| value.to_str().ok())
    == Some(etag.as_str());
  let mut response = if not_modified {
    drop(permit);
    StatusCode::NOT_MODIFIED.into_response()
  } else {
    let stream =
      ReaderStream::with_capacity(tokio::fs::File::from_std(file), 64 * 1024).map(move |chunk| {
        let _keep_permit_until_body_is_dropped = &permit;
        chunk
      });
    Response::new(Body::from_stream(stream))
  };
  let headers = response.headers_mut();
  headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
  headers.insert(
    header::CACHE_CONTROL,
    HeaderValue::from_static("public, max-age=31536000, immutable"),
  );
  headers.insert(
    header::ETAG,
    HeaderValue::from_str(&etag).map_err(|_| StoreError::internal())?,
  );
  if !not_modified {
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
  }
  if let Some(filename) = filename {
    headers.insert(
      header::CONTENT_DISPOSITION,
      HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
        .map_err(|_| StoreError::internal())?,
    );
  }
  Ok(response)
}
