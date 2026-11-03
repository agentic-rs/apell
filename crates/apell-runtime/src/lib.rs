//! A native, interpreted backend run. The caller owns its lifetime separately
//! from any UI attachments. One interpreter thread serializes guest turns.
mod actor;
mod vm;

use apell_guest::protocol::{Inbound, MAX_MESSAGE_BYTES, MAX_PENDING};
pub use apell_guest::HostError;
use serde_json::Value;
use std::{
  future::Future,
  io::{self, Write},
  pin::Pin,
  sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
  },
};
use tokio::sync::{broadcast, mpsc, oneshot, watch, OwnedSemaphorePermit, Semaphore};

pub type ServiceFuture = Pin<Box<dyn Future<Output = Result<Value, HostError>> + Send + 'static>>;

/// Bind this adapter to one verified applet and perform its permission checks
/// natively. The guest cannot select an applet identity or another backend run.
pub trait HostServices: Send + Sync + 'static {
  fn call(&self, method: String, params: Value, events: HostEvents) -> ServiceFuture;
}

#[derive(Clone)]
pub struct HostEvents {
  queue: Queue,
  request_id: String,
  generation: u64,
  overflow: Arc<AtomicBool>,
}
impl HostEvents {
  pub fn emit(&self, event: Value) -> Result<(), HostError> {
    let result = self
      .queue
      .send_event(self.request_id.clone(), self.generation, event);
    if result.is_err() {
      self.overflow.store(true, Ordering::Release);
    }
    result
  }
}

const MAILBOX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone)]
struct Queue {
  tx: mpsc::Sender<Message>,
  budget: Arc<Semaphore>,
  stopping: Arc<AtomicBool>,
}
struct Frame {
  bytes: Vec<u8>,
  _bytes: OwnedSemaphorePermit,
}
enum Message {
  Invoke {
    id: String,
    frame: Frame,
    reply: oneshot::Sender<Result<Value, HostError>>,
    slot: OwnedSemaphorePermit,
  },
  Event {
    id: String,
    generation: u64,
    frame: Frame,
  },
  Result {
    id: String,
    generation: u64,
    frame: Frame,
  },
  Stop,
}

impl Queue {
  fn frame(&self, bytes: Vec<u8>) -> Result<Frame, HostError> {
    let permit = self
      .budget
      .clone()
      .try_acquire_many_owned(bytes.len() as u32)
      .map_err(|_| HostError::new("The backend mailbox byte limit was reached."))?;
    Ok(Frame {
      bytes,
      _bytes: permit,
    })
  }
  fn try_send(&self, message: Message) -> Result<(), HostError> {
    if self.stopping.load(Ordering::Acquire) {
      return Err(stopped());
    }
    self.tx.try_send(message).map_err(|err| match err {
      mpsc::error::TrySendError::Full(_) => HostError::new("The backend mailbox is busy."),
      mpsc::error::TrySendError::Closed(_) => stopped(),
    })
  }
  fn send_event(&self, id: String, generation: u64, event: Value) -> Result<(), HostError> {
    let bytes = bounded_json(
      &Inbound::HostEvent {
        request_id: id.clone(),
        event,
      },
      MAX_MESSAGE_BYTES,
    )?;
    self.try_send(Message::Event {
      id,
      generation,
      frame: self.frame(bytes)?,
    })
  }
  fn stop(&self) {
    self.stopping.store(true, Ordering::Release);
    let _ = self.tx.try_send(Message::Stop);
  }
}

struct Inner {
  queue: Queue,
  invocations: Arc<Semaphore>,
  events: broadcast::WeakSender<Value>,
  ended: watch::Receiver<Option<HostError>>,
}
impl Drop for Inner {
  fn drop(&mut self) {
    self.queue.stop();
  }
}

#[derive(Clone)]
pub struct RuntimeHandle {
  inner: Arc<Inner>,
}

impl RuntimeHandle {
  pub async fn start(wasm: Vec<u8>, services: Arc<dyn HostServices>) -> Result<Self, HostError> {
    let runtime = tokio::runtime::Handle::try_current()
      .map_err(|_| HostError::new("The backend requires an active native async runtime."))?;
    let (tx, rx) = mpsc::channel(128);
    let queue = Queue {
      tx,
      budget: Arc::new(Semaphore::new(MAILBOX_BYTES)),
      stopping: Arc::new(AtomicBool::new(false)),
    };
    let (events, _) = broadcast::channel(32);
    let (ended_tx, ended) = watch::channel(None);
    let inner = Arc::new(Inner {
      queue: queue.clone(),
      invocations: Arc::new(Semaphore::new(MAX_PENDING)),
      events: events.downgrade(),
      ended,
    });
    let (started, ready) = oneshot::channel();
    std::thread::Builder::new()
      .name("apell-backend".into())
      .spawn(move || {
        let outcome = actor::run(wasm, services, runtime, queue.clone(), rx, events, started);
        queue.stop();
        ended_tx.send_replace(Some(outcome.err().unwrap_or_else(stopped)));
      })
      .map_err(|_| HostError::new("Could not start the backend interpreter thread."))?;
    ready.await.map_err(|_| stopped())??;
    Ok(Self { inner })
  }

  pub async fn invoke(&self, command: &str, params: Value) -> Result<Value, HostError> {
    if let Some(error) = self.inner.ended.borrow().as_ref() {
      return Err(error.clone());
    }
    validate_name(command)?;
    let slot = self
      .inner
      .invocations
      .clone()
      .try_acquire_owned()
      .map_err(|_| HostError::new("Too many backend commands are active."))?;
    let id = uuid::Uuid::new_v4().to_string();
    let bytes = bounded_json(
      &Inbound::Invoke {
        request_id: id.clone(),
        command: command.into(),
        params,
      },
      MAX_MESSAGE_BYTES,
    )?;
    let (reply, result) = oneshot::channel();
    self.inner.queue.try_send(Message::Invoke {
      id,
      frame: self.inner.queue.frame(bytes)?,
      reply,
      slot,
    })?;
    // Dropping the frontend future drops only its reply receiver. The command,
    // host calls and guest futures continue under this run's ownership.
    result.await.map_err(|_| stopped())?
  }

  pub fn subscribe(&self) -> broadcast::Receiver<Value> {
    self
      .inner
      .events
      .upgrade()
      .map_or_else(|| broadcast::channel(1).1, |events| events.subscribe())
  }

  /// The interpreter no longer accepts commands, even if its host has not yet
  /// processed the event stream closing.
  pub fn is_stopped(&self) -> bool {
    self.inner.queue.stopping.load(Ordering::Acquire)
  }

  pub async fn stop(&self) {
    self.inner.queue.stop();
    let mut ended = self.inner.ended.clone();
    while ended.borrow_and_update().is_none() {
      if ended.changed().await.is_err() {
        break;
      }
    }
  }
}

fn stopped() -> HostError {
  HostError::new("The applet backend has stopped.")
}

fn validate_name(name: &str) -> Result<(), HostError> {
  if name.is_empty()
    || name.len() > 128
    || !name
      .bytes()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
  {
    Err(HostError::new(
      "Invalid backend command or host method name.",
    ))
  } else {
    Ok(())
  }
}

fn bounded_json(value: &impl serde::Serialize, limit: usize) -> Result<Vec<u8>, HostError> {
  struct Writer {
    bytes: Vec<u8>,
    limit: usize,
  }
  impl Write for Writer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
      if self.bytes.len().saturating_add(buf.len()) > self.limit {
        return Err(io::Error::other("ABI byte limit"));
      }
      self.bytes.extend_from_slice(buf);
      Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
      Ok(())
    }
  }
  let mut out = Writer {
    bytes: Vec::new(),
    limit,
  };
  serde_json::to_writer(&mut out, value)
    .map_err(|_| HostError::new("The backend message is invalid or exceeds the byte limit."))?;
  Ok(out.bytes)
}
