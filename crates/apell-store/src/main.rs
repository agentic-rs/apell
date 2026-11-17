use apell_store::{router, Config};

#[tokio::main]
async fn main() -> std::process::ExitCode {
  match serve().await {
    Ok(()) => std::process::ExitCode::SUCCESS,
    Err(message) => {
      eprintln!("{message}");
      std::process::ExitCode::FAILURE
    }
  }
}

async fn serve() -> Result<(), String> {
  let config = Config::from_env()?;
  if !config.web_dir.join("index.html").is_file() {
    return Err(
      "Build the store website and set APELL_STORE_WEB_DIR to its output directory.".into(),
    );
  }
  let app = router(&config).map_err(|error| error.to_string())?;
  let listener = tokio::net::TcpListener::bind(config.listen)
    .await
    .map_err(|_| "Could not listen on the configured store address.")?;
  println!("Apell Store is ready.");
  axum::serve(listener, app)
    .with_graceful_shutdown(shutdown())
    .await
    .map_err(|_| "The store server stopped unexpectedly.".into())
}

async fn shutdown() {
  let interrupt = async {
    let _ = tokio::signal::ctrl_c().await;
  };
  #[cfg(unix)]
  let terminate = async {
    if let Ok(mut signal) =
      tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
      signal.recv().await;
    }
  };
  #[cfg(not(unix))]
  let terminate = std::future::pending::<()>();
  tokio::select! { _ = interrupt => {}, _ = terminate => {} }
}
