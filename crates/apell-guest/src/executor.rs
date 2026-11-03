use super::{
  create_id,
  protocol::{HostError, Inbound, Outbound, Outcome, MAX_PENDING},
  send,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
  cell::{Cell, RefCell},
  collections::{BTreeMap, VecDeque},
  future::{poll_fn, Future},
  pin::Pin,
  sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
  },
  task::{Context, Poll, Wake, Waker},
};

struct Ready(AtomicBool);
impl Wake for Ready {
  fn wake(self: Arc<Self>) {
    self.wake_by_ref();
  }
  fn wake_by_ref(self: &Arc<Self>) {
    self.0.store(true, Ordering::Relaxed);
  }
}

struct Task {
  ready: Arc<Ready>,
  future: Pin<Box<dyn Future<Output = ()>>>,
}
type EventHandler = Box<dyn FnMut(Value)>;
struct Pending {
  result: Option<Result<Value, HostError>>,
  waker: Option<Waker>,
  on_event: Option<EventHandler>,
}
thread_local! {
  static TASKS: RefCell<VecDeque<Task>> = const { RefCell::new(VecDeque::new()) };
  static TASK_COUNT: Cell<usize> = const { Cell::new(0) };
  static CALLS: RefCell<BTreeMap<String, Pending>> = const { RefCell::new(BTreeMap::new()) };
}

/// Schedule work owned by this backend, including after the initiating view detaches.
pub fn spawn(future: impl Future<Output = ()> + 'static) -> Result<(), HostError> {
  TASK_COUNT.with(|count| {
    if count.get() >= 2 * MAX_PENDING {
      return Err(HostError::new("Too many backend tasks are active."));
    }
    count.set(count.get() + 1);
    Ok(())
  })?;
  TASKS.with(|tasks| {
    tasks.borrow_mut().push_back(Task {
      ready: Arc::new(Ready(AtomicBool::new(true))),
      future: Box::pin(future),
    })
  });
  Ok(())
}

fn pump() -> Result<(), HostError> {
  // A self-waking task must yield to the host eventually. Wasm fuel also bounds
  // code that does not return from poll at all.
  for _ in 0..1024 {
    let task = TASKS.with(|tasks| {
      let mut tasks = tasks.borrow_mut();
      let index = tasks
        .iter()
        .position(|task| task.ready.0.load(Ordering::Relaxed))?;
      tasks.remove(index)
    });
    let Some(mut task) = task else {
      return Ok(());
    };
    task.ready.0.store(false, Ordering::Relaxed);
    let waker = Waker::from(task.ready.clone());
    if task
      .future
      .as_mut()
      .poll(&mut Context::from_waker(&waker))
      .is_pending()
    {
      TASKS.with(|tasks| tasks.borrow_mut().push_back(task));
    } else {
      TASK_COUNT.with(|count| count.set(count.get() - 1));
    }
  }
  Err(HostError::new("The backend task did not yield."))
}

pub async fn call<T: DeserializeOwned>(method: &str, params: Value) -> Result<T, HostError> {
  call_with_events(method, params, |_| {}).await
}

pub async fn call_with_events<T: DeserializeOwned>(
  method: &str,
  params: Value,
  on_event: impl FnMut(Value) + 'static,
) -> Result<T, HostError> {
  let request_id = create_id();
  CALLS.with(|calls| {
    let mut calls = calls.borrow_mut();
    if calls.len() >= MAX_PENDING {
      return Err(HostError::new("Too many host calls are active."));
    }
    calls.insert(
      request_id.clone(),
      Pending {
        result: None,
        waker: None,
        on_event: Some(Box::new(on_event)),
      },
    );
    Ok(())
  })?;
  // This guard also releases the callback when the caller drops its future.
  struct Guard(String);
  impl Drop for Guard {
    fn drop(&mut self) {
      CALLS.with(|calls| calls.borrow_mut().remove(&self.0));
    }
  }
  let _guard = Guard(request_id.clone());
  send(&Outbound::HostCall {
    request_id: request_id.clone(),
    method: method.into(),
    params,
  })?;
  let value = poll_fn(|cx| {
    CALLS.with(|calls| {
      let mut calls = calls.borrow_mut();
      let pending = calls.get_mut(&request_id).expect("pending host call guard");
      if let Some(result) = pending.result.take() {
        Poll::Ready(result)
      } else {
        pending.waker = Some(cx.waker().clone());
        Poll::Pending
      }
    })
  })
  .await?;
  serde_json::from_value(value).map_err(HostError::new)
}

pub fn emit(event: Value) -> Result<(), HostError> {
  send(&Outbound::Event { event })
}

fn reply(request_id: String, outcome: Outcome) {
  let result = send(&Outbound::Reply {
    request_id: request_id.clone(),
    outcome,
  });
  if let Err(error) = result {
    // A large application result must still settle the original command.
    send(&Outbound::Reply {
      request_id,
      outcome: Outcome::Failure { error },
    })
    .expect("could not deliver bounded command failure");
  }
}

pub(super) fn dispatch<F, Fut>(input: Inbound, handle: F) -> Result<(), HostError>
where
  F: FnOnce(String, Value) -> Fut + 'static,
  Fut: Future<Output = Result<Value, HostError>> + 'static,
{
  match input {
    Inbound::Invoke {
      request_id,
      command,
      params,
    } => {
      let id = request_id.clone();
      if let Err(error) = spawn(async move {
        reply(id, handle(command, params).await.into());
      }) {
        reply(request_id, Outcome::Failure { error });
      }
    }
    Inbound::HostResult {
      request_id,
      outcome,
    } => CALLS.with(|calls| {
      if let Some(pending) = calls.borrow_mut().get_mut(&request_id) {
        pending.result = Some(outcome.into());
        if let Some(waker) = pending.waker.take() {
          waker.wake();
        }
      }
    }),
    Inbound::HostEvent { request_id, event } => {
      // Release the map borrow before invoking arbitrary applet code.
      let callback = CALLS.with(|calls| {
        calls
          .borrow_mut()
          .get_mut(&request_id)
          .and_then(|pending| pending.on_event.take())
      });
      if let Some(mut callback) = callback {
        callback(event);
        CALLS.with(|calls| {
          if let Some(pending) = calls.borrow_mut().get_mut(&request_id) {
            pending.on_event = Some(callback);
          }
        });
      }
    }
  }
  pump()
}
