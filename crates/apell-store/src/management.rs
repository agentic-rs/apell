use crate::{
  auth::{Auth, User},
  packages::{Invitation, ManagedPackage, PackageDetails},
  query, AppState, Result, StoreError, TrustedPublisher,
};
use axum::{
  body::to_bytes,
  extract::{Path, Request, State},
  http::{header, HeaderMap, StatusCode},
  response::Response,
  routing::{delete, get, post, put},
  Json, Router,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{sync::Arc, time::Duration};

pub(crate) fn routes() -> Router<AppState> {
  Router::new()
    .route("/v1/account/packages", get(list).post(register))
    .route("/v1/account/packages/{id}", get(details))
    .route(
      "/v1/account/packages/{id}/publisher",
      put(publisher).delete(disconnect),
    )
    .route("/v1/account/packages/{id}/members", post(invite))
    .route(
      "/v1/account/packages/{id}/members/{user_id}",
      delete(remove),
    )
    .route(
      "/v1/account/packages/{id}/invitations/{invitation_id}",
      delete(cancel),
    )
    .route("/v1/account/packages/{id}/ownership", post(transfer))
    .route("/v1/account/packages/{id}/publish", post(publish))
    .route("/v1/account/invitations/{id}/accept", post(accept))
    .route("/v1/account/invitations/{id}", delete(decline))
}

pub(crate) fn authentication(state: &AppState) -> Result<Arc<Auth>> {
  state.auth.clone().ok_or_else(|| {
    StoreError::new(
      StatusCode::SERVICE_UNAVAILABLE,
      "login_unavailable",
      "GitHub sign-in is not configured.",
    )
  })
}

pub(crate) async fn session_user(
  state: &AppState,
  headers: &HeaderMap,
  mutation: bool,
) -> Result<User> {
  let auth = authentication(state)?;
  let headers = headers.clone();
  query(state, move |_| {
    if mutation {
      auth.require_mutation(&headers)
    } else {
      auth.user(&headers)
    }
  })
  .await
}

async fn input<T: DeserializeOwned>(state: &AppState, request: Request) -> Result<(User, T)> {
  let user = session_user(state, request.headers(), true).await?;
  if !request
    .headers()
    .get(header::CONTENT_TYPE)
    .and_then(|v| v.to_str().ok())
    .is_some_and(|v| {
      v.split(';')
        .next()
        .is_some_and(|mime| mime.trim() == "application/json")
    })
  {
    return Err(StoreError::invalid(
      "Use application/json for this request.",
    ));
  }
  let bytes = tokio::time::timeout(
    Duration::from_secs(10),
    to_bytes(request.into_body(), 16 * 1024),
  )
  .await
  .map_err(|_| StoreError::invalid("The request timed out."))?
  .map_err(|_| StoreError::invalid("The request exceeds its size limit."))?;
  let input =
    serde_json::from_slice(&bytes).map_err(|_| StoreError::invalid("Invalid request fields."))?;
  Ok((user, input))
}

#[derive(Serialize)]
struct AccountPackages {
  packages: Vec<ManagedPackage>,
  invitations: Vec<Invitation>,
}

async fn list(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<AccountPackages>> {
  let user = session_user(&state, &headers, false).await?;
  query(&state, move |store| {
    Ok(AccountPackages {
      packages: store.managed_packages(user.github_id)?,
      invitations: store.invitations_for(user.github_id)?,
    })
  })
  .await
  .map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Register {
  applet_id: String,
}

async fn register(
  State(state): State<AppState>,
  request: Request,
) -> Result<(StatusCode, Json<PackageDetails>)> {
  let (user, input): (_, Register) = input(&state, request).await?;
  let result = query(&state, move |store| {
    store.register_package(&input.applet_id, &user)
  })
  .await?;
  Ok((StatusCode::CREATED, Json(result)))
}

async fn details(
  State(state): State<AppState>,
  Path(id): Path<String>,
  headers: HeaderMap,
) -> Result<Json<PackageDetails>> {
  let user = session_user(&state, &headers, false).await?;
  query(&state, move |store| {
    store.package_details(&id, user.github_id)
  })
  .await
  .map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublisherInput {
  repository: String,
  workflow: String,
  allowed_refs: Vec<String>,
  event_names: Vec<String>,
  environment: Option<String>,
}

async fn publisher(
  State(state): State<AppState>,
  Path(id): Path<String>,
  request: Request,
) -> Result<Json<PackageDetails>> {
  let (user, input): (_, PublisherInput) = input(&state, request).await?;
  let actor_id = user.github_id;
  let check_id = id.clone();
  query(&state, move |store| {
    store.package_details(&check_id, actor_id)
  })
  .await?;
  if state.oidc.is_none() {
    return Err(StoreError::invalid(
      "Trusted publishing is not configured on this Store.",
    ));
  }
  let repository = authentication(&state)?
    .lookup_repository(&input.repository)
    .await?;
  let policy = TrustedPublisher {
    applet_id: id.clone(),
    repository_id: repository.repository_id,
    repository_owner_id: repository.repository_owner_id,
    repository: Some(repository.repository),
    workflow: input.workflow,
    allowed_refs: input.allowed_refs,
    event_names: input.event_names,
    environment: input.environment,
  };
  query(&state, move |store| {
    store.set_publisher(&id, actor_id, policy)?;
    store.package_details(&id, actor_id)
  })
  .await
  .map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn success() -> Json<serde_json::Value> {
  Json(serde_json::json!({"ok": true}))
}

async fn disconnect(
  State(state): State<AppState>,
  Path(id): Path<String>,
  request: Request,
) -> Result<Json<serde_json::Value>> {
  let (user, _): (_, Empty) = input(&state, request).await?;
  query(&state, move |store| {
    store.disconnect_publisher(&id, user.github_id)
  })
  .await?;
  Ok(success())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Invite {
  github_login: String,
}

async fn invite(
  State(state): State<AppState>,
  Path(id): Path<String>,
  request: Request,
) -> Result<Json<Invitation>> {
  let (user, input): (_, Invite) = input(&state, request).await?;
  let actor_id = user.github_id;
  let check_id = id.clone();
  query(&state, move |store| {
    store.package_details(&check_id, actor_id)
  })
  .await?;
  let target = authentication(&state)?
    .lookup_user(&input.github_login)
    .await?;
  query(&state, move |store| {
    store.upsert_user(&target)?;
    store.invite_maintainer(&id, actor_id, &target)
  })
  .await
  .map(Json)
}

async fn remove(
  State(state): State<AppState>,
  Path((id, target)): Path<(String, u64)>,
  request: Request,
) -> Result<Json<serde_json::Value>> {
  let (user, _): (_, Empty) = input(&state, request).await?;
  query(&state, move |store| {
    store.remove_maintainer(&id, user.github_id, target)
  })
  .await?;
  Ok(success())
}

async fn cancel(
  State(state): State<AppState>,
  Path((id, invitation_id)): Path<(String, String)>,
  request: Request,
) -> Result<Json<serde_json::Value>> {
  let (user, _): (_, Empty) = input(&state, request).await?;
  query(&state, move |store| {
    store.cancel_invitation(&id, user.github_id, &invitation_id)
  })
  .await?;
  Ok(success())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Transfer {
  github_id: u64,
}

async fn transfer(
  State(state): State<AppState>,
  Path(id): Path<String>,
  request: Request,
) -> Result<Json<Invitation>> {
  let (user, input): (_, Transfer) = input(&state, request).await?;
  query(&state, move |store| {
    store.invite_transfer(&id, user.github_id, input.github_id)
  })
  .await
  .map(Json)
}

async fn accept(
  State(state): State<AppState>,
  Path(id): Path<String>,
  request: Request,
) -> Result<Json<serde_json::Value>> {
  let (user, _): (_, Empty) = input(&state, request).await?;
  query(&state, move |store| {
    store.accept_invitation(&id, user.github_id)
  })
  .await?;
  Ok(success())
}

async fn decline(
  State(state): State<AppState>,
  Path(id): Path<String>,
  request: Request,
) -> Result<Json<serde_json::Value>> {
  let (user, _): (_, Empty) = input(&state, request).await?;
  query(&state, move |store| {
    store.decline_invitation(&id, user.github_id)
  })
  .await?;
  Ok(success())
}

async fn publish(
  State(state): State<AppState>,
  Path(id): Path<String>,
  request: Request,
) -> Result<Response> {
  crate::publish_request(state, request, Some(id)).await
}
