use super::{args::Command, emit, http::Api, valid_user, CredentialStore, User};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use url::Url;

pub fn run(
  command: &Command,
  store: &Url,
  credentials: &CredentialStore,
  json_output: bool,
) -> Result<(), String> {
  if command == &Command::Logout && !credentials.contains(store)? {
    return emit(&json!({"ok":true}), json_output);
  }
  let credential = credentials.load(store)?;
  let api = Api::new(store, Some(&credential.token))?;
  if command == &Command::Logout {
    let reply = api.request(Method::POST, "/api/v1/auth/cli/logout", Some(&json!({})))?;
    if !reply.status.is_success() && reply.status != StatusCode::UNAUTHORIZED {
      return Err(reply.error());
    }
    credentials.remove(store)?;
    return emit(&json!({"ok":true}), json_output);
  }
  credential.validate()?;
  let result = execute(command, &api)?;
  emit(&result, json_output)
}

pub(super) fn execute(command: &Command, api: &Api) -> Result<Value, String> {
  const PACKAGES: &str = "/api/v1/account/packages";
  let empty = json!({});
  let path = |id: &str| format!("{PACKAGES}/{id}");
  match command {
    Command::Whoami => api.call(Method::GET, "/api/v1/auth/cli/me", None),
    Command::Packages => api.call(Method::GET, PACKAGES, None),
    Command::Register(id) => api.call(Method::POST, PACKAGES, Some(&json!({"applet_id":id}))),
    Command::Show(id) => api.call(Method::GET, &path(id), None),
    Command::OidcShow(id) => {
      let details = api.call(Method::GET, &path(id), None)?;
      Ok(
        json!({"applet_id":id,"publisher":details.get("publisher").ok_or("Invalid package response.")?}),
      )
    }
    Command::OidcSet(id, publisher) => api.call(
      Method::PUT,
      &format!("{}/publisher", path(id)),
      Some(&serde_json::to_value(publisher).map_err(|_| "Invalid publisher settings.")?),
    ),
    Command::OidcDisconnect(id) => api.call(
      Method::DELETE,
      &format!("{}/publisher", path(id)),
      Some(&empty),
    ),
    Command::Members(id) => {
      let details = api.call(Method::GET, &path(id), None)?;
      Ok(
        json!({"applet_id":id,"owner":details.get("owner").ok_or("Invalid package response.")?,"maintainers":details.get("maintainers").ok_or("Invalid package response.")?,"invitations":details.get("invitations").ok_or("Invalid package response.")?}),
      )
    }
    Command::Invite(id, user) => api.call(
      Method::POST,
      &format!("{}/members", path(id)),
      Some(&json!({"github_login":user})),
    ),
    Command::Remove(id, user) => {
      let details = api.call(Method::GET, &path(id), None)?;
      let user = member(&details, user)?;
      api.call(
        Method::DELETE,
        &format!("{}/members/{}", path(id), user.github_id),
        Some(&empty),
      )
    }
    Command::Transfer(id, user) => {
      let details = api.call(Method::GET, &path(id), None)?;
      let user = member(&details, user)?;
      api.call(
        Method::POST,
        &format!("{}/ownership", path(id)),
        Some(&json!({"github_id":user.github_id})),
      )
    }
    Command::Invitations => {
      let value = api.call(Method::GET, PACKAGES, None)?;
      Ok(json!({"invitations":value.get("invitations").ok_or("Invalid invitations response.")?}))
    }
    Command::Accept(id) => api.call(
      Method::POST,
      &format!("/api/v1/account/invitations/{id}/accept"),
      Some(&empty),
    ),
    Command::Decline(id) => api.call(
      Method::DELETE,
      &format!("/api/v1/account/invitations/{id}"),
      Some(&empty),
    ),
    Command::Login | Command::Logout => Err("Invalid account command.".into()),
  }
}

fn member(details: &Value, target: &str) -> Result<User, String> {
  let values = details
    .get("maintainers")
    .and_then(Value::as_array)
    .ok_or("Invalid package member list.")?;
  for value in values {
    let user: User =
      serde_json::from_value(value.clone()).map_err(|_| "Invalid package member.")?;
    if !valid_user(&user) {
      return Err("Invalid package member.".into());
    }
    if target.eq_ignore_ascii_case(&user.github_login) || target == user.github_id.to_string() {
      return Ok(user);
    }
  }
  Err("That user is not a current maintainer. Use apell package members list ID to inspect membership.".into())
}
