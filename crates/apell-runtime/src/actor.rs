use super::*;
use apell_guest::protocol::{Outbound, Outcome};
use std::{
  collections::HashMap,
  future::poll_fn,
  panic::{catch_unwind, AssertUnwindSafe},
  task::Poll,
};
use tokio::task::JoinHandle;

type Reply = oneshot::Sender<Result<Value, HostError>>;
struct HostJob {
  generation: u64,
  task: JoinHandle<()>,
}
struct Actor {
  vm: vm::Vm,
  services: Arc<dyn HostServices>,
  runtime: tokio::runtime::Handle,
  queue: Queue,
  events: broadcast::Sender<Value>,
  pending: HashMap<String, (Reply, OwnedSemaphorePermit)>,
  jobs: HashMap<String, HostJob>,
  generation: u64,
}

pub(super) fn run(
  wasm: Vec<u8>,
  services: Arc<dyn HostServices>,
  runtime: tokio::runtime::Handle,
  queue: Queue,
  mut inbox: mpsc::Receiver<Message>,
  events: broadcast::Sender<Value>,
  ready: oneshot::Sender<Result<(), HostError>>,
) -> Result<(), HostError> {
  let vm = match vm::Vm::new(&wasm) {
    Ok(vm) => vm,
    Err(error) => {
      let _ = ready.send(Err(error.clone()));
      return Err(error);
    }
  };
  drop(wasm);
  if ready.send(Ok(())).is_err() {
    return Ok(());
  }
  let mut actor = Actor {
    vm,
    services,
    runtime,
    queue,
    events,
    pending: HashMap::new(),
    jobs: HashMap::new(),
    generation: 0,
  };
  let outcome = (|| {
    while !actor.queue.stopping.load(Ordering::Acquire) {
      let Some(message) = inbox.blocking_recv() else {
        break;
      };
      if actor.queue.stopping.load(Ordering::Acquire) {
        break;
      }
      match message {
        Message::Invoke {
          id,
          frame,
          reply,
          slot,
        } => {
          actor.pending.insert(id, (reply, slot));
          actor.turn(frame)?;
        }
        Message::Event {
          id,
          generation,
          frame,
        } => {
          if actor
            .jobs
            .get(&id)
            .is_some_and(|job| job.generation == generation)
          {
            actor.turn(frame)?;
          }
        }
        Message::Result {
          id,
          generation,
          frame,
        } => {
          if actor
            .jobs
            .get(&id)
            .is_some_and(|job| job.generation == generation)
          {
            actor.jobs.remove(&id);
            actor.turn(frame)?;
          }
        }
        Message::Stop => break,
      }
    }
    Ok(())
  })();
  // Abort and join all service futures before acknowledging an explicit stop.
  inbox.close();
  let reason = outcome.as_ref().err().cloned().unwrap_or_else(stopped);
  for (_, (reply, _)) in actor.pending.drain() {
    let _ = reply.send(Err(reason.clone()));
  }
  for job in actor.jobs.values() {
    job.task.abort();
  }
  actor.runtime.block_on(async {
    for (_, job) in actor.jobs.drain() {
      let _ = job.task.await;
    }
  });
  outcome
}

impl Actor {
  fn turn(&mut self, frame: Frame) -> Result<(), HostError> {
    let emitted = self.vm.turn(&frame.bytes)?;
    drop(frame);
    for item in emitted {
      match item {
        Outbound::HostCall {
          request_id,
          method,
          params,
        } => self.host_call(request_id, method, params)?,
        Outbound::Reply {
          request_id,
          outcome,
        } => {
          let (reply, _slot) = self
            .pending
            .remove(&request_id)
            .ok_or_else(|| HostError::new("Backend replied to an unknown invocation."))?;
          let _ = reply.send(outcome.into());
        }
        Outbound::Event { event } => {
          vm::check_event(&event)?;
          let _ = self.events.send(event);
        }
      }
    }
    Ok(())
  }

  fn host_call(&mut self, id: String, method: String, params: Value) -> Result<(), HostError> {
    validate_name(&id)?;
    validate_name(&method)?;
    if self.jobs.len() >= MAX_PENDING || self.jobs.contains_key(&id) {
      return Err(HostError::new(
        "Too many host calls or a duplicate backend request ID.",
      ));
    }
    self.generation = self.generation.checked_add(1).ok_or_else(stopped)?;
    let generation = self.generation;
    let queue = self.queue.clone();
    let services = self.services.clone();
    let host_id = id.clone();
    let overflow = Arc::new(AtomicBool::new(false));
    let events = HostEvents {
      queue: queue.clone(),
      request_id: id.clone(),
      generation,
      overflow: overflow.clone(),
    };
    let task = self.runtime.spawn(async move {
      // A faulty host adapter must still settle the corresponding guest future.
      let interrupted = || HostError::new("The native host operation failed.");
      let result = match catch_unwind(AssertUnwindSafe(|| services.call(method, params, events))) {
        Ok(mut future) => {
          poll_fn(|ctx| {
            catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(ctx)))
              .unwrap_or_else(|_| Poll::Ready(Err(interrupted())))
          })
          .await
        }
        Err(_) => Err(interrupted()),
      };
      let result = if overflow.load(Ordering::Acquire) {
        Err(HostError::new(
          "The backend could not receive all host events.",
        ))
      } else {
        result
      };
      queue.finish(host_id, generation, result).await;
    });
    self.jobs.insert(id, HostJob { generation, task });
    Ok(())
  }
}

impl Queue {
  async fn finish(&self, id: String, generation: u64, result: Result<Value, HostError>) {
    let record = |outcome: Outcome| Inbound::HostResult {
      request_id: id.clone(),
      outcome,
    };
    let bytes =
      bounded_json(&record(result.into()), MAX_MESSAGE_BYTES).and_then(|bytes| self.frame(bytes));
    let frame = match bytes {
      Ok(frame) => frame,
      Err(error) => {
        // Only a small terminal error waits for mailbox capacity. A backlog of
        // completed large results cannot allocate another full mailbox per job.
        let bytes =
          bounded_json(&record(Outcome::Failure { error }), 4096).expect("bounded host failure");
        let Ok(permit) = self
          .budget
          .clone()
          .acquire_many_owned(bytes.len() as u32)
          .await
        else {
          return;
        };
        Frame {
          bytes,
          _bytes: permit,
        }
      }
    };
    let _ = self
      .tx
      .send(Message::Result {
        id,
        generation,
        frame,
      })
      .await;
  }
}
