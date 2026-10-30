//! Applet-scoped SQLite connections. Applets own their SQL, never database paths.
use rusqlite::{
  hooks::{AuthAction, AuthContext, Authorization},
  limits::Limit,
  params_from_iter,
  types::{Value as DatabaseValue, ValueRef},
  Batch, Connection, OpenFlags,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
  collections::HashMap,
  path::PathBuf,
  sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
  },
  time::{Duration, Instant},
};
use uuid::Uuid;

const TRANSACTION_TIMEOUT: Duration = Duration::from_secs(30);
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_SQL_BYTES: usize = 64 * 1024;
const MAX_VALUE_BYTES: usize = 1024 * 1024;
const MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;
const MAX_ROWS: usize = 1000;
const MAX_HANDLES: usize = 64;
const MAX_SESSION_HANDLES: usize = 8;

type Result<T> = std::result::Result<T, String>;

pub struct SqliteService {
  root: PathBuf,
  state: Mutex<ServiceState>,
}

#[derive(Default)]
struct ServiceState {
  sessions: HashMap<String, String>,
  databases: HashMap<String, Database>,
}

struct Database {
  session_id: String,
  applet_id: String,
  connection: Connection,
  allow_transaction: Arc<AtomicBool>,
  transaction: Option<Transaction>,
}

struct Transaction {
  handle: String,
  started_at: Instant,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum SqlValue {
  Null,
  Integer(String),
  Real(f64),
  Text(String),
  Blob(Vec<u8>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenRequest {
  database_name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HandleRequest {
  database_handle: String,
  #[serde(default)]
  transaction_handle: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatementRequest {
  database_handle: String,
  #[serde(default)]
  transaction_handle: Option<String>,
  sql: String,
  #[serde(default)]
  parameters: Vec<SqlValue>,
}

impl SqliteService {
  pub fn new(root: PathBuf) -> Self {
    Self {
      root,
      state: Mutex::new(ServiceState::default()),
    }
  }

  /// Register a host-created session before exposing its applet bridge.
  pub fn register_session(&self, session_id: &str, applet_id: &str) -> Result<()> {
    validate_name(applet_id, 128, true)?;
    let mut state = self
      .state
      .lock()
      .map_err(|_| "The SQLite service is unavailable.".to_string())?;
    if session_id.is_empty() || state.sessions.contains_key(session_id) {
      return Err("The SQLite session is already registered or invalid.".into());
    }
    state.sessions.insert(session_id.into(), applet_id.into());
    Ok(())
  }

  /// Run on a blocking worker: SQLite and its filesystem access are synchronous.
  pub fn dispatch(
    &self,
    session_id: &str,
    applet_id: &str,
    method: &str,
    params: Value,
  ) -> Result<Value> {
    validate_name(applet_id, 128, true)?;
    let mut state = self
      .state
      .lock()
      .map_err(|_| "The SQLite service is unavailable.".to_string())?;
    if state.sessions.get(session_id).map(String::as_str) != Some(applet_id) {
      return Err("The applet session has closed or is not registered.".into());
    }
    let databases = &mut state.databases;
    reap(databases);
    match method {
      "sqlite.open" => {
        let request: OpenRequest = decode(params)?;
        validate_name(&request.database_name, 64, false)?;
        if databases.len() >= MAX_HANDLES
          || databases
            .values()
            .filter(|database| database.session_id == session_id)
            .count()
            >= MAX_SESSION_HANDLES
        {
          return Err("Close an existing database before opening another.".into());
        }
        let directory = self.root.join(applet_id);
        std::fs::create_dir_all(&directory)
          .map_err(|_| "Could not create applet database storage.".to_string())?;
        let path = directory.join(format!("{}.sqlite", request.database_name));
        let connection = Connection::open_with_flags(
          path,
          OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| "Could not open the applet database.".to_string())?;
        configure(&connection)?;
        let allow_transaction = Arc::new(AtomicBool::new(false));
        let allow_control = allow_transaction.clone();
        connection.authorizer(Some(move |context: AuthContext<'_>| {
          authorize(context, allow_control.load(Ordering::Relaxed))
        }));
        let handle = Uuid::new_v4().to_string();
        databases.insert(
          handle.clone(),
          Database {
            session_id: session_id.into(),
            applet_id: applet_id.into(),
            connection,
            allow_transaction,
            transaction: None,
          },
        );
        Ok(json!({ "database_handle": handle }))
      }
      "sqlite.query" | "sqlite.execute" => {
        let request: StatementRequest = decode(params)?;
        let database = owned_database(databases, &request.database_handle, session_id, applet_id)?;
        database.check_transaction(request.transaction_handle.as_deref())?;
        let result = run_statement(&database.connection, request, method == "sqlite.query");
        // Some SQLite errors roll back the entire transaction automatically.
        if database.transaction.is_some() && database.connection.is_autocommit() {
          database.transaction = None;
          return result.and(Err("The transaction was rolled back by SQLite.".into()));
        }
        result
      }
      "sqlite.begin" | "sqlite.commit" | "sqlite.rollback" | "sqlite.close" => {
        let request: HandleRequest = decode(params)?;
        let database = owned_database(databases, &request.database_handle, session_id, applet_id)?;
        if method == "sqlite.close" {
          // Dropping a connection rolls back its unfinished transaction.
          databases.remove(&request.database_handle);
          return Ok(Value::Null);
        }
        database.check_transaction(request.transaction_handle.as_deref())?;
        match method {
          "sqlite.begin" => {
            if database.transaction.is_some() {
              return Err("A transaction is already active on this database handle.".into());
            }
            database.control("BEGIN IMMEDIATE")?;
            let handle = Uuid::new_v4().to_string();
            database.transaction = Some(Transaction {
              handle: handle.clone(),
              started_at: Instant::now(),
            });
            Ok(json!({ "transaction_handle": handle }))
          }
          _ => {
            if database.transaction.is_none() {
              return Err("No transaction is active on this database handle.".into());
            }
            let result = database.control(if method == "sqlite.commit" {
              "COMMIT"
            } else {
              "ROLLBACK"
            });
            if database.connection.is_autocommit() {
              database.transaction = None;
            }
            result.map(|_| Value::Null)
          }
        }
      }
      _ => Err("Unknown SQLite method.".into()),
    }
  }

  pub fn close_session(&self, session_id: &str) {
    if let Ok(mut state) = self.state.lock() {
      state.sessions.remove(session_id);
      state
        .databases
        .retain(|_, database| database.session_id != session_id);
    }
  }

  /// Call periodically as well as on dispatch so idle applets release write locks.
  pub fn reap_expired(&self) {
    if let Ok(mut state) = self.state.lock() {
      reap(&mut state.databases);
    }
  }
}

impl Database {
  fn check_transaction(&self, handle: Option<&str>) -> Result<()> {
    match (&self.transaction, handle) {
      (None, None) => Ok(()),
      (Some(transaction), Some(handle)) if transaction.handle == handle => Ok(()),
      _ => Err("A valid transaction handle is required while a transaction is active.".into()),
    }
  }

  fn control(&self, sql: &str) -> Result<()> {
    self.connection.progress_handler(0, None::<fn() -> bool>);
    self.allow_transaction.store(true, Ordering::Relaxed);
    let result = self.connection.execute_batch(sql).map_err(sql_error);
    self.allow_transaction.store(false, Ordering::Relaxed);
    result
  }
}

fn owned_database<'a>(
  databases: &'a mut HashMap<String, Database>,
  handle: &str,
  session_id: &str,
  applet_id: &str,
) -> Result<&'a mut Database> {
  databases
    .get_mut(handle)
    .filter(|database| database.session_id == session_id && database.applet_id == applet_id)
    .ok_or_else(|| "The database handle is invalid or belongs to another session.".into())
}

fn reap(databases: &mut HashMap<String, Database>) {
  databases.retain(|_, database| {
    if database
      .transaction
      .as_ref()
      .is_some_and(|transaction| transaction.started_at.elapsed() >= TRANSACTION_TIMEOUT)
    {
      if database.control("ROLLBACK").is_err() {
        return false;
      }
      database.transaction = None;
    }
    true
  });
}

fn configure(connection: &Connection) -> Result<()> {
  connection
    .busy_timeout(Duration::from_millis(250))
    .map_err(sql_error)?;
  connection
    .execute_batch(
      "PRAGMA foreign_keys=ON;
       PRAGMA trusted_schema=OFF;
       PRAGMA temp_store=MEMORY;
       PRAGMA journal_mode=WAL;
       PRAGMA max_page_count=32768;",
    )
    .map_err(sql_error)?;
  for (limit, value) in [
    (Limit::SQLITE_LIMIT_LENGTH, MAX_VALUE_BYTES as i32),
    (Limit::SQLITE_LIMIT_SQL_LENGTH, MAX_SQL_BYTES as i32),
    (Limit::SQLITE_LIMIT_COLUMN, 256),
    (Limit::SQLITE_LIMIT_EXPR_DEPTH, 64),
    (Limit::SQLITE_LIMIT_COMPOUND_SELECT, 32),
    (Limit::SQLITE_LIMIT_ATTACHED, 0),
    (Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 256),
    (Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 16),
    (Limit::SQLITE_LIMIT_WORKER_THREADS, 0),
  ] {
    connection.set_limit(limit, value);
  }
  Ok(())
}

fn authorize(context: AuthContext<'_>, allow_transaction: bool) -> Authorization {
  use AuthAction::*;
  let allowed = match context.action {
    Attach { .. } | Detach { .. } | CreateVtable { .. } | DropVtable { .. } | Unknown { .. } => {
      false
    }
    Transaction { .. } | Savepoint { .. } => allow_transaction,
    Pragma {
      pragma_name,
      pragma_value,
    } => match pragma_name.to_ascii_lowercase().as_str() {
      "user_version" | "table_info" | "table_xinfo" | "index_list" | "index_info"
      | "index_xinfo" | "foreign_key_list" => true,
      "foreign_keys" | "schema_version" | "data_version" | "page_count" | "freelist_count"
      | "quick_check" | "integrity_check" => pragma_value.is_none(),
      _ => false,
    },
    Function { function_name } => !matches!(
      function_name.to_ascii_lowercase().as_str(),
      "load_extension" | "readfile" | "writefile" | "eval" | "fts3_tokenizer"
    ),
    _ => true,
  };
  if allowed {
    Authorization::Allow
  } else {
    Authorization::Deny
  }
}

fn run_statement(connection: &Connection, request: StatementRequest, query: bool) -> Result<Value> {
  if request.sql.is_empty() || request.sql.len() > MAX_SQL_BYTES || request.sql.contains('\0') {
    return Err("SQL must contain between 1 and 65536 bytes and no NUL characters.".into());
  }
  if request.parameters.len() > 256 {
    return Err("A statement accepts at most 256 parameters.".into());
  }
  let mut parameter_bytes = 0;
  let parameters = request
    .parameters
    .into_iter()
    .map(|value| to_database_value(value, &mut parameter_bytes))
    .collect::<Result<Vec<_>>>()?;
  let deadline = Instant::now() + QUERY_TIMEOUT;
  connection.progress_handler(1000, Some(move || Instant::now() >= deadline));
  let mut batch = Batch::new(connection, &request.sql);
  let mut statement = batch
    .next()
    .map_err(sql_error)?
    .ok_or_else(|| "The SQL contains no statement.".to_string())?;
  if batch.next().map_err(sql_error)?.is_some() {
    return Err("Send exactly one SQL statement per call.".into());
  }
  if query {
    if !statement.readonly() {
      return Err("sqlite.query only accepts read-only statements.".into());
    }
    let columns = statement
      .column_names()
      .into_iter()
      .map(str::to_owned)
      .collect::<Vec<_>>();
    let mut cursor = statement
      .query(params_from_iter(parameters.iter()))
      .map_err(sql_error)?;
    let mut rows = Vec::new();
    let mut result_bytes = 0;
    while let Some(row) = cursor.next().map_err(sql_error)? {
      if rows.len() >= MAX_ROWS {
        return Err("Query returned more than 1000 rows; use LIMIT and pagination.".into());
      }
      let mut values = Vec::with_capacity(columns.len());
      for index in 0..columns.len() {
        values.push(from_database_value(row.get_ref(index).map_err(sql_error)?)?);
      }
      // Include JSON overhead and byte-array expansion in the bridge limit.
      result_bytes += serde_json::to_vec(&values)
        .map_err(|_| "Could not encode SQL results.".to_string())?
        .len();
      if result_bytes > MAX_RESULT_BYTES {
        return Err("Query result exceeds 4 MiB; select fewer or smaller values.".into());
      }
      rows.push(values);
    }
    Ok(json!({ "columns": columns, "rows": rows }))
  } else {
    if statement.column_count() != 0 {
      return Err(
        "Use sqlite.query for statements returning columns; RETURNING is unsupported.".into(),
      );
    }
    let affected_rows = statement
      .execute(params_from_iter(parameters.iter()))
      .map_err(sql_error)?;
    Ok(json!({
      "affected_rows": affected_rows,
      "last_insert_row_id": connection.last_insert_rowid().to_string(),
    }))
  }
}

fn to_database_value(value: SqlValue, bytes: &mut usize) -> Result<DatabaseValue> {
  let (value, size) = match value {
    SqlValue::Null => (DatabaseValue::Null, 1),
    SqlValue::Integer(value) => (
      DatabaseValue::Integer(
        value
          .parse::<i64>()
          .map_err(|_| "SQL integers must be signed 64-bit decimal strings.".to_string())?,
      ),
      8,
    ),
    SqlValue::Real(value) if value.is_finite() => (DatabaseValue::Real(value), 8),
    SqlValue::Real(_) => return Err("SQL real values must be finite.".into()),
    SqlValue::Text(value) => {
      let size = value.len();
      (DatabaseValue::Text(value), size)
    }
    SqlValue::Blob(value) => {
      let size = value.len();
      (DatabaseValue::Blob(value), size)
    }
  };
  *bytes += size;
  if *bytes > MAX_VALUE_BYTES {
    return Err("SQL parameters exceed 1 MiB.".into());
  }
  Ok(value)
}

fn from_database_value(value: ValueRef<'_>) -> Result<SqlValue> {
  Ok(match value {
    ValueRef::Null => SqlValue::Null,
    ValueRef::Integer(value) => SqlValue::Integer(value.to_string()),
    ValueRef::Real(value) if value.is_finite() => SqlValue::Real(value),
    ValueRef::Real(_) => return Err("The query produced a non-finite real value.".into()),
    ValueRef::Text(value) => SqlValue::Text(
      std::str::from_utf8(value)
        .map_err(|_| "The query produced non-UTF-8 text; cast it to BLOB.".to_string())?
        .to_owned(),
    ),
    ValueRef::Blob(value) => SqlValue::Blob(value.to_vec()),
  })
}

fn validate_name(value: &str, max_length: usize, dots: bool) -> Result<()> {
  if value.is_empty()
    || value.len() > max_length
    || value == "."
    || value == ".."
    || !value.bytes().all(|byte| {
      byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') || (dots && byte == b'.')
    })
  {
    return Err("Storage names must contain only letters, digits, underscores, or hyphens.".into());
  }
  Ok(())
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
  serde_json::from_value(value).map_err(|_| "Invalid SQLite request parameters.".into())
}

fn sql_error(error: rusqlite::Error) -> String {
  match error {
    rusqlite::Error::SqliteFailure(code, _) => match code.code {
      rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked => {
        "The database is busy; finish the other transaction and retry.".into()
      }
      rusqlite::ErrorCode::OperationInterrupted => {
        "The SQL operation exceeded its time limit.".into()
      }
      rusqlite::ErrorCode::AuthorizationForStatementDenied => {
        "The SQL operation is not permitted.".into()
      }
      rusqlite::ErrorCode::ConstraintViolation => {
        "A database constraint rejected the operation.".into()
      }
      rusqlite::ErrorCode::DiskFull => "The applet database has reached its storage limit.".into(),
      _ => "SQLite could not execute the statement; check its syntax and schema.".into(),
    },
    rusqlite::Error::InvalidParameterCount(..) | rusqlite::Error::InvalidParameterName(..) => {
      "SQL parameters do not match the statement.".into()
    }
    _ => "SQLite could not execute the statement; check its syntax, parameters, and schema.".into(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  struct Fixture {
    service: SqliteService,
    directory: tempfile::TempDir,
    handle: String,
  }

  impl Fixture {
    fn new() -> Self {
      let directory = tempfile::tempdir().unwrap();
      let service = SqliteService::new(directory.path().to_owned());
      service.register_session("session", "test.chat").unwrap();
      let handle = service
        .dispatch(
          "session",
          "test.chat",
          "sqlite.open",
          json!({ "database_name": "chat" }),
        )
        .unwrap()["database_handle"]
        .as_str()
        .unwrap()
        .to_owned();
      Self {
        service,
        directory,
        handle,
      }
    }

    fn call(&self, method: &str, mut params: Value) -> Result<Value> {
      params["database_handle"] = json!(self.handle);
      self
        .service
        .dispatch("session", "test.chat", method, params)
    }

    fn execute(&self, sql: &str) -> Result<Value> {
      self.call("sqlite.execute", json!({ "sql": sql }))
    }

    fn query(&self, sql: &str) -> Result<Value> {
      self.call("sqlite.query", json!({ "sql": sql }))
    }
  }

  #[test]
  fn values_round_trip_without_losing_integer_precision() {
    let fixture = Fixture::new();
    fixture
      .execute("CREATE TABLE values_test (n INTEGER, t TEXT, b BLOB, r REAL, z)")
      .unwrap();
    fixture
      .call(
        "sqlite.execute",
        json!({
          "sql": "INSERT INTO values_test VALUES (?1, ?2, ?3, ?4, ?5)",
          "parameters": [
            { "type": "integer", "value": "9223372036854775807" },
            { "type": "text", "value": "hello" },
            { "type": "blob", "value": [0, 255, 42] },
            { "type": "real", "value": 1.25 },
            { "type": "null" },
          ],
        }),
      )
      .unwrap();
    let result = fixture.query("SELECT * FROM values_test").unwrap();
    assert_eq!(
      result["rows"][0],
      json!([
        { "type": "integer", "value": "9223372036854775807" },
        { "type": "text", "value": "hello" },
        { "type": "blob", "value": [0, 255, 42] },
        { "type": "real", "value": 1.25 },
        { "type": "null" },
      ])
    );
  }

  #[test]
  fn transactions_require_their_handle_and_support_read_modify_write() {
    let fixture = Fixture::new();
    fixture
      .execute("CREATE TABLE counter (value INTEGER)")
      .unwrap();
    fixture.execute("INSERT INTO counter VALUES (1)").unwrap();
    let transaction =
      fixture.call("sqlite.begin", json!({})).unwrap()["transaction_handle"].clone();
    assert!(fixture.query("SELECT * FROM counter").is_err());
    let result = fixture
      .call(
        "sqlite.query",
        json!({
          "transaction_handle": transaction,
          "sql": "SELECT value FROM counter",
        }),
      )
      .unwrap();
    assert_eq!(result["rows"][0][0]["value"], "1");
    fixture
      .call(
        "sqlite.execute",
        json!({
          "transaction_handle": transaction,
          "sql": "UPDATE counter SET value=2",
        }),
      )
      .unwrap();
    fixture
      .call(
        "sqlite.rollback",
        json!({ "transaction_handle": transaction }),
      )
      .unwrap();
    assert_eq!(
      fixture.query("SELECT value FROM counter").unwrap()["rows"][0][0]["value"],
      "1"
    );
    let transaction =
      fixture.call("sqlite.begin", json!({})).unwrap()["transaction_handle"].clone();
    fixture
      .call(
        "sqlite.execute",
        json!({
          "transaction_handle": transaction,
          "sql": "UPDATE counter SET value=3",
        }),
      )
      .unwrap();
    fixture
      .call(
        "sqlite.commit",
        json!({ "transaction_handle": transaction }),
      )
      .unwrap();
    assert_eq!(
      fixture.query("SELECT value FROM counter").unwrap()["rows"][0][0]["value"],
      "3"
    );
  }

  #[test]
  fn session_close_rolls_back_and_handles_cannot_cross_scope() {
    let fixture = Fixture::new();
    fixture
      .execute("CREATE TABLE records (id INTEGER)")
      .unwrap();
    for (session, applet) in [("other", "test.chat"), ("session", "other.app")] {
      assert!(fixture
        .service
        .dispatch(
          session,
          applet,
          "sqlite.query",
          json!({
            "database_handle": fixture.handle,
            "sql": "SELECT * FROM records",
          })
        )
        .is_err());
    }
    let transaction =
      fixture.call("sqlite.begin", json!({})).unwrap()["transaction_handle"].clone();
    fixture
      .call(
        "sqlite.execute",
        json!({
          "transaction_handle": transaction,
          "sql": "INSERT INTO records VALUES (1)",
        }),
      )
      .unwrap();
    fixture.service.close_session("session");
    assert!(fixture.query("SELECT * FROM records").is_err());
    assert!(fixture
      .service
      .dispatch(
        "session",
        "test.chat",
        "sqlite.open",
        json!({
          "database_name": "late_open",
        })
      )
      .is_err());
    let connection =
      Connection::open(fixture.directory.path().join("test.chat/chat.sqlite")).unwrap();
    assert_eq!(
      connection
        .query_row("SELECT count(*) FROM records", [], |row| row
          .get::<_, i64>(0))
        .unwrap(),
      0
    );
  }

  #[test]
  fn expired_transaction_rolls_back_without_waiting_for_next_applet_request() {
    let fixture = Fixture::new();
    fixture
      .execute("CREATE TABLE records (id INTEGER)")
      .unwrap();
    let transaction =
      fixture.call("sqlite.begin", json!({})).unwrap()["transaction_handle"].clone();
    fixture
      .call(
        "sqlite.execute",
        json!({
          "transaction_handle": transaction,
          "sql": "INSERT INTO records VALUES (1)",
        }),
      )
      .unwrap();
    fixture
      .service
      .state
      .lock()
      .unwrap()
      .databases
      .get_mut(&fixture.handle)
      .unwrap()
      .transaction
      .as_mut()
      .unwrap()
      .started_at = Instant::now() - TRANSACTION_TIMEOUT;
    fixture.service.reap_expired();
    assert!(fixture
      .call(
        "sqlite.commit",
        json!({ "transaction_handle": transaction })
      )
      .is_err());
    assert!(fixture.query("SELECT * FROM records").unwrap()["rows"]
      .as_array()
      .unwrap()
      .is_empty());
  }

  #[test]
  fn schema_migration_is_allowed_but_file_access_and_control_sql_are_denied() {
    let fixture = Fixture::new();
    fixture.execute("PRAGMA user_version=3").unwrap();
    assert_eq!(
      fixture.query("PRAGMA user_version").unwrap()["rows"][0][0]["value"],
      "3"
    );
    fixture
      .execute("CREATE TABLE records (id INTEGER)")
      .unwrap();
    assert!(
      !fixture.query("PRAGMA table_info(records)").unwrap()["rows"]
        .as_array()
        .unwrap()
        .is_empty()
    );
    for sql in [
      "ATTACH DATABASE 'outside.sqlite' AS outside",
      "DETACH DATABASE main",
      "VACUUM INTO 'outside.sqlite'",
      "PRAGMA writable_schema=ON",
      "PRAGMA temp_store_directory='.'",
      "PRAGMA journal_mode=OFF",
      "PRAGMA foreign_keys=OFF",
      "PRAGMA schema_version=123",
      "BEGIN",
      "COMMIT",
      "SAVEPOINT sneaky",
      "SELECT load_extension('example')",
      "SELECT writefile('outside', 'data')",
      "CREATE VIRTUAL TABLE x USING fts5(content)",
      "CREATE TABLE first (id); CREATE TABLE second (id)",
    ] {
      assert!(fixture.execute(sql).is_err(), "must deny: {sql}");
    }
    assert!(fixture.query("PRAGMA user_version=99").is_err());
    assert!(fixture
      .query("INSERT INTO records VALUES (1) RETURNING id")
      .is_err());
    assert!(fixture.query("SELECT 1; SELECT 2").is_err());
    assert!(fixture.query("SELECT * FROM pragma_database_list").is_err());
    assert!(fixture.query("SELECT * FROM pragma_journal_mode").is_err());
    assert!(!fixture.directory.path().join("outside.sqlite").exists());
    assert!(fixture
      .service
      .dispatch(
        "session",
        "test.chat",
        "sqlite.open",
        json!({
          "database_name": "../escape",
        })
      )
      .is_err());
    assert!(fixture
      .service
      .dispatch(
        "session",
        "../escape",
        "sqlite.open",
        json!({
          "database_name": "chat",
        })
      )
      .is_err());
  }

  #[test]
  fn query_results_are_bounded() {
    let fixture = Fixture::new();
    assert!(fixture.query(
      "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1001) SELECT x FROM n",
    ).unwrap_err().contains("1000 rows"));
    assert!(fixture.query("SELECT zeroblob(1048577)").is_err());
    assert!(fixture
      .query("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n) SELECT sum(x) FROM n",)
      .unwrap_err()
      .contains("time limit"));
  }
}
