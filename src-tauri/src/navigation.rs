use tauri::Url;

pub fn trusted_origins(dev_url: Option<&Url>) -> Vec<String> {
  let mut origins = vec![
    "tauri://localhost".into(),
    "http://tauri.localhost".into(),
    "https://tauri.localhost".into(),
  ];
  if cfg!(debug_assertions) {
    if let Some(url) = dev_url {
      origins.push(origin(url));
    }
  }
  origins
}

fn origin(url: &Url) -> String {
  format!(
    "{}://{}{}",
    url.scheme(),
    url.host_str().unwrap_or(""),
    url
      .port()
      .map(|port| format!(":{port}"))
      .unwrap_or_default()
  )
}

pub fn allowed(url: &Url, origins: &[String]) -> bool {
  // WKWebView also asks about subframe navigation. These inert documents do
  // not receive the host key, and applets cannot navigate the top-level frame.
  matches!(url.as_str(), "about:blank" | "about:srcdoc") || origins.contains(&origin(url))
}

pub fn bootstrap(host_key: &str, origins: &[String]) -> Result<String, serde_json::Error> {
  Ok(format!(
    "if(window===window.top && {}.includes(location.protocol+'//'+location.host)){{Object.defineProperty(window,'__APELL_HOST_KEY__',{{value:{}}});}}",
    serde_json::to_string(origins)?, serde_json::to_string(host_key)?,
  ))
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn navigation_binds_authority_to_exact_origins() {
    let origins = trusted_origins(Some(&Url::parse("http://localhost:1420/").unwrap()));
    for url in [
      "tauri://localhost/index.html",
      "about:srcdoc",
      "about:blank",
      "http://tauri.localhost/index.html",
    ] {
      assert!(allowed(&Url::parse(url).unwrap(), &origins));
    }
    for url in [
      "http://localhost:9999/",
      "https://example.invalid/",
      "file:///tmp/host.html",
      "tauri://other/index.html",
    ] {
      assert!(!allowed(&Url::parse(url).unwrap(), &origins));
    }
  }
}
