use apell_core::{
  bundles::{atomic_write, BundleStore, Manifest},
  llm::{LlmConfig, LlmService},
  resources::ResourceStore,
  sqlite::SqliteService,
};
use serde_json::{json, Value};
use std::{
  collections::HashMap,
  path::PathBuf,
  sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
  },
};
use uuid::Uuid;

#[derive(Clone)]
pub struct Session {
  pub manifest: Manifest,
  pub alive: Arc<AtomicBool>,
  pub cancellation: tokio::sync::watch::Sender<bool>,
}

pub struct Host {
  pub host_key: String,
  pub bundles: Arc<BundleStore>,
  pub sqlite: Arc<SqliteService>,
  pub resources: Arc<ResourceStore>,
  pub llm: Arc<LlmService>,
  pub settings: tokio::sync::Mutex<LlmConfig>,
  pub sessions: Mutex<HashMap<String, Session>>,
  root: PathBuf,
}

impl Host {
  pub fn new(root: PathBuf) -> Result<Self, String> {
    std::fs::create_dir_all(&root).map_err(|_| "Could not create host storage.")?;
    let settings_path = root.join("llm.json");
    let settings = if settings_path.exists() {
      serde_json::from_slice(
        &std::fs::read(settings_path).map_err(|_| "Could not read LLM settings.")?,
      )
      .map_err(|_| "Invalid saved LLM settings.")?
    } else {
      LlmConfig {
        base_url: "demo".into(),
        model_id: "local-demo".into(),
      }
    };
    Ok(Self {
      host_key: format!("{}{}", Uuid::new_v4(), Uuid::new_v4()),
      bundles: Arc::new(BundleStore::new(root.join("bundles"))),
      sqlite: Arc::new(SqliteService::new(root.join("databases"))),
      resources: Arc::new(ResourceStore::new(root.join("resources"))?),
      llm: Arc::new(LlmService::new()?),
      settings: tokio::sync::Mutex::new(settings),
      sessions: Mutex::new(HashMap::new()),
      root,
    })
  }

  pub fn authorize(&self, host_key: &str) -> Result<(), String> {
    if host_key != self.host_key {
      return Err("Host authorization required.".into());
    }
    Ok(())
  }

  pub fn session(&self, session_id: &str, method: &str) -> Result<Session, String> {
    let sessions = self
      .sessions
      .lock()
      .map_err(|_| "Session state is unavailable.")?;
    let session = sessions
      .get(session_id)
      .ok_or("Applet session is closed.")?;
    let capability = method.split('.').next().unwrap_or("");
    if !session.manifest.permissions.iter().any(|p| p == capability) {
      return Err("Applet did not declare this permission.".into());
    }
    Ok(session.clone())
  }

  pub fn close(&self, session_id: &str) -> Result<(), String> {
    if let Some(session) = self
      .sessions
      .lock()
      .map_err(|_| "Session state is unavailable.")?
      .remove(session_id)
    {
      session.alive.store(false, Ordering::SeqCst);
      session.cancellation.send_replace(true);
    }
    self.llm.close_session(session_id);
    self.sqlite.close_session(session_id);
    Ok(())
  }

  pub fn save_settings(&self, settings: &LlmConfig) -> Result<(), String> {
    let data = serde_json::to_vec(settings).map_err(|_| "Could not encode settings.")?;
    atomic_write(&self.root.join("llm.json"), &data)
  }
}

pub fn string<'a>(params: &'a Value, field: &str) -> Result<&'a str, String> {
  params
    .get(field)
    .and_then(Value::as_str)
    .ok_or_else(|| format!("Missing {field}."))
}

pub fn public_config(config: &LlmConfig) -> Value {
  json!({"model_id": config.model_id, "mode": if config.base_url == "demo" { "demo" } else { "provider" }})
}

pub fn credential_account(config: &LlmConfig) -> String {
  apell_core::bundles::digest(config.base_url.as_bytes())
}
