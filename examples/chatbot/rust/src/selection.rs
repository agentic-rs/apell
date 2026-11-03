use crate::storage;
use apell_guest::HostError;
use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Target {
  pub connection_id: String,
  pub model_id: String,
}

impl Target {
  fn valid(&self) -> bool {
    !self.connection_id.is_empty()
      && self.connection_id != "local-demo"
      && !self.model_id.trim().is_empty()
  }
}

#[derive(Deserialize)]
struct Provenance {
  connection_id: String,
  requested_model_id: String,
}

pub async fn load(id: &str, transaction: Option<&str>) -> Result<Option<Target>, HostError> {
  let rows = storage::query(
    "SELECT model_target FROM conversations WHERE conversation_id = ?1",
    vec![storage::text(id)],
    transaction,
  )
  .await?;
  let row = rows
    .first()
    .ok_or_else(|| HostError::new("This conversation no longer exists."))?;
  if let Some(value) = storage::optional_cell(row, 0)? {
    return Ok(
      serde_json::from_str::<Target>(&value)
        .ok()
        .filter(Target::valid),
    );
  }

  // Older chats stored the host's attribution on each assistant turn. The
  // requested model is the route key even when a provider reports an alias.
  let rows = storage::query(
    "SELECT DISTINCT provenance FROM messages WHERE conversation_id = ?1 AND role = 'assistant'",
    vec![storage::text(id)],
    transaction,
  )
  .await?;
  let mut inferred = None;
  for row in rows {
    let Some(provenance) =
      storage::optional_cell(&row, 0)?.and_then(|s| serde_json::from_str::<Provenance>(&s).ok())
    else {
      return Ok(None);
    };
    let target = Target {
      connection_id: provenance.connection_id,
      model_id: provenance.requested_model_id,
    };
    if !target.valid()
      || inferred
        .as_ref()
        .is_some_and(|previous| previous != &target)
    {
      return Ok(None);
    }
    inferred = Some(target);
  }
  Ok(inferred)
}

async fn store(id: &str, target: &Target, transaction: &str) -> Result<(), HostError> {
  storage::execute(
    "UPDATE conversations SET model_target = ?1 WHERE conversation_id = ?2",
    vec![
      storage::text(serde_json::to_string(target).map_err(HostError::new)?),
      storage::text(id),
    ],
    Some(transaction),
  )
  .await
}

pub async fn choose(id: &str, target: Target) -> Result<String, HostError> {
  if !target.valid() {
    return Err(HostError::new("Choose a provider and model."));
  }
  let transaction = storage::begin().await?;
  let result = async {
    let previous = load(id, Some(&transaction)).await?;
    let same_target = previous.is_some_and(|previous| previous == target);
    let populated = !storage::query(
      "SELECT 1 FROM messages WHERE conversation_id = ?1 LIMIT 1",
      vec![storage::text(id)],
      Some(&transaction),
    )
    .await?
    .is_empty();
    let selected_id = if populated && !same_target {
      create(Some(&target), Some(&transaction)).await?
    } else {
      store(id, &target, &transaction).await?;
      id.to_owned()
    };
    storage::commit(&transaction).await?;
    Ok(selected_id)
  }
  .await;
  if result.is_err() {
    storage::rollback(&transaction).await;
  }
  result
}

pub async fn create(
  target: Option<&Target>,
  transaction: Option<&str>,
) -> Result<String, HostError> {
  if target.is_some_and(|target| !target.valid()) {
    return Err(HostError::new("Choose a provider and model."));
  }
  let id = apell_guest::create_id();
  let encoded = target
    .map(serde_json::to_string)
    .transpose()
    .map_err(HostError::new)?;
  storage::execute(
    "INSERT INTO conversations (conversation_id, title, created_at, updated_at, model_target) VALUES (?1, 'New conversation', ?2, ?2, ?3)",
    vec![storage::text(&id), storage::text(apell_guest::now()), storage::optional_text(encoded)], transaction
  ).await?;
  Ok(id)
}
