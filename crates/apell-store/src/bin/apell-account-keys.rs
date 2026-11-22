use apell_store::key_relay::{Config, Relay};
use std::{process::ExitCode, time::Duration};

#[tokio::main]
async fn main() -> ExitCode {
  match serve().await {
    Ok(()) => ExitCode::SUCCESS,
    Err(message) => {
      eprintln!("{message}");
      ExitCode::FAILURE
    }
  }
}

async fn serve() -> Result<(), String> {
  let config = Config::from_env()?;
  let listen = config.listen;
  let relay = Relay::open(config).map_err(|error| error.to_string())?;
  let listener = tokio::net::TcpListener::bind(listen)
    .await
    .map_err(|_| "Could not listen on the configured Account key exchange address.")?;
  let app = relay.router();
  let cleanup = tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
      interval.tick().await;
      // Expired pairs stay unavailable even while cleanup is delayed.
      let _ = relay.prune_expired().await;
    }
  });
  println!("Apell Account key exchange is ready.");
  let result = axum::serve(listener, app)
    .with_graceful_shutdown(shutdown())
    .await;
  cleanup.abort();
  result.map_err(|_| "The Account key exchange server stopped unexpectedly.".into())
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
