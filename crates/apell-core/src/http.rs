use reqwest::{Client, ClientBuilder};

/// Keep TLS and byte-preserving response behavior shared by native HTTP clients.
pub(crate) fn client_builder() -> Result<ClientBuilder, String> {
  // Dependency features must not silently decode applet HTTP bodies or alter
  // downloaded bytes used for resource and bundle integrity checks.
  let builder = Client::builder()
    .no_gzip()
    .no_brotli()
    .no_deflate()
    .no_zstd();
  #[cfg(target_os = "android")]
  {
    // Reqwest's default Android verifier needs a separately initialized JVM
    // component. Use rustls with bundled roots so every native client works
    // before any WebView or plugin lifecycle callback. Verification stays on.
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
      rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| "Could not initialize secure networking.".to_owned())?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(builder.tls_backend_preconfigured(config))
  }
  #[cfg(not(target_os = "android"))]
  {
    Ok(builder)
  }
}
