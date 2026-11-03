use crate::host::Host;
use tauri::{
  http::{Request, Response, StatusCode},
  AppHandle, Manager,
};

pub fn package_origin() -> &'static str {
  #[cfg(any(target_os = "android", target_os = "windows"))]
  {
    "http://apel.localhost"
  }
  #[cfg(not(any(target_os = "android", target_os = "windows")))]
  {
    "apel://localhost"
  }
}

pub fn entry_url(view_id: &str) -> String {
  format!("{}/{view_id}/index.html", package_origin())
}

fn csp() -> String {
  let origin = package_origin();
  format!("default-src 'none'; script-src 'unsafe-inline' 'wasm-unsafe-eval' {origin}; style-src 'unsafe-inline' {origin}; img-src data: {origin}; font-src {origin}; connect-src {origin}; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'")
}

fn asset_path(uri_path: &str) -> Result<(String, String), StatusCode> {
  let decoded = percent_encoding::percent_decode_str(uri_path)
    .decode_utf8()
    .map_err(|_| StatusCode::BAD_REQUEST)?;
  let (view, path) = decoded
    .strip_prefix('/')
    .and_then(|path| path.split_once('/'))
    .ok_or(StatusCode::BAD_REQUEST)?;
  if uuid::Uuid::parse_str(view).is_err()
    || path.len() > 4096
    || path == "manifest.json"
    || path.starts_with("lib/")
    || path.contains('\\')
    || path.chars().any(char::is_control)
    || path
      .split('/')
      .any(|part| part.is_empty() || matches!(part, "." | ".."))
  {
    return Err(StatusCode::NOT_FOUND);
  }
  Ok((view.into(), path.into()))
}

fn content_type(path: &str) -> &'static str {
  match path
    .rsplit('.')
    .next()
    .unwrap_or("")
    .to_ascii_lowercase()
    .as_str()
  {
    "html" | "htm" => "text/html; charset=utf-8",
    "js" | "mjs" => "text/javascript; charset=utf-8",
    "css" => "text/css; charset=utf-8",
    "json" | "map" => "application/json",
    "wasm" => "application/wasm",
    "svg" => "image/svg+xml",
    "png" => "image/png",
    "jpg" | "jpeg" => "image/jpeg",
    "gif" => "image/gif",
    "webp" => "image/webp",
    "ico" => "image/x-icon",
    "woff" => "font/woff",
    "woff2" => "font/woff2",
    "ttf" => "font/ttf",
    "txt" => "text/plain; charset=utf-8",
    _ => "application/octet-stream",
  }
}

pub fn respond(app: &AppHandle, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
  let result = (|| {
    if request.method() != "GET" && request.method() != "HEAD" {
      return Err(StatusCode::METHOD_NOT_ALLOWED);
    }
    let authority = request
      .uri()
      .authority()
      .map(|authority| authority.as_str())
      .unwrap_or("");
    if authority != "localhost"
      && !(cfg!(any(target_os = "android", target_os = "windows")) && authority == "apel.localhost")
    {
      return Err(StatusCode::NOT_FOUND);
    }
    let (view, path) = asset_path(request.uri().path())?;
    let host = app.state::<Host>();
    let bundle = host.view_bundle(&view).map_err(|_| StatusCode::NOT_FOUND)?;
    let bytes = bundle.read_asset(&path).ok_or(StatusCode::NOT_FOUND)?;
    Ok((
      path.to_owned(),
      if request.method() == "HEAD" {
        vec![]
      } else {
        bytes.to_vec()
      },
    ))
  })();
  match result {
    Ok((path, body)) => Response::builder()
      .status(StatusCode::OK)
      .header("Content-Type", content_type(&path))
      .header("Content-Security-Policy", csp())
      .header("Access-Control-Allow-Origin", "null")
      .header("X-Content-Type-Options", "nosniff")
      .header("Cache-Control", "no-store")
      .body(body)
      .expect("static response headers"),
    Err(status) => Response::builder()
      .status(status)
      .body(vec![])
      .expect("static response status"),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn assets_are_scoped_to_a_view_and_public_package_files() {
    let id = uuid::Uuid::new_v4();
    assert_eq!(
      asset_path(&format!("/{id}/assets/hello%20world.js")).unwrap(),
      (id.to_string(), "assets/hello world.js".into())
    );
    for path in [
      "manifest.json",
      "lib/main.wasm",
      "assets/../index.html",
      "%2e%2e/index.html",
      "assets//a",
      "assets%5cmain.js",
      "assets/%00",
      "assets/%FF",
    ] {
      assert!(asset_path(&format!("/{id}/{path}")).is_err(), "{path}");
    }
    assert!(asset_path("/not-a-view/index.html").is_err());
    assert!(asset_path(&format!("/{id}/")).is_err());
  }

  #[test]
  fn opaque_frontends_can_load_relative_modules_without_network_access() {
    let policy = csp();
    assert!(policy.contains(&format!("connect-src {}", package_origin())));
    assert!(!policy.contains("https:"));
    #[cfg(not(any(target_os = "android", target_os = "windows")))]
    assert!(!policy.contains("http:"));
  }
}
