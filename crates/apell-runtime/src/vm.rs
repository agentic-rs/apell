//! The Store and its linear memory never leave the backend's interpreter thread.
use crate::{bounded_json, HostError};
use apell_guest::protocol::{Outbound, MAX_MESSAGE_BYTES};
use time::{macros::format_description, OffsetDateTime};
use wasmi::{
  Caller, Config, EnforcedLimits, Engine, Linker, Memory, Module, Store, StoreLimits,
  StoreLimitsBuilder, TypedFunc,
};

const MAX_MODULE_BYTES: usize = 16 * 1024 * 1024;
const MAX_MEMORY: usize = 64 * 1024 * 1024;
const TURN_FUEL: u64 = 32_000_000;

struct VmData {
  limits: StoreLimits,
  outbox: Vec<Outbound>,
  sent_bytes: usize,
  imports: usize,
}
impl VmData {
  fn count_import(&mut self) -> Result<(), wasmi::Error> {
    self.imports += 1;
    if self.imports > 256 {
      return Err(wasmi::Error::new("backend import limit exceeded"));
    }
    Ok(())
  }
}

pub(crate) struct Vm {
  store: Store<VmData>,
  memory: Memory,
  alloc: TypedFunc<u32, u32>,
  free: TypedFunc<(u32, u32), ()>,
  dispatch: TypedFunc<(u32, u32), i32>,
}

impl Vm {
  pub fn new(wasm: &[u8]) -> Result<Self, HostError> {
    if wasm.is_empty() || wasm.len() > MAX_MODULE_BYTES {
      return Err(HostError::new(
        "Backend WASM must be between 1 byte and 16 MiB.",
      ));
    }
    let mut config = Config::default();
    config
      .consume_fuel(true)
      .allow_start_fn(false)
      .ignore_custom_sections(true)
      .enforced_limits(EnforcedLimits::strict())
      .set_max_recursion_depth(256)
      .set_max_stack_height(1024 * 1024)
      .wasm_multi_memory(false);
    let engine = Engine::new(&config);
    let module = Module::new(&engine, wasm)
      .map_err(|error| HostError::new(format!("Invalid backend WASM: {error}")))?;
    let mut store = Store::new(
      &engine,
      VmData {
        limits: StoreLimitsBuilder::new()
          .instances(1)
          .memories(1)
          .tables(1)
          .memory_size(MAX_MEMORY)
          .table_elements(65_536)
          .build(),
        outbox: Vec::new(),
        sent_bytes: 0,
        imports: 0,
      },
    );
    store.limiter(|data| &mut data.limits);
    store.set_fuel(TURN_FUEL).map_err(HostError::new)?;
    let mut linker = Linker::new(&engine);
    linker
      .func_wrap(
        "apell",
        "send",
        |mut caller: Caller<'_, VmData>, ptr: u32, len: u32| -> Result<i32, wasmi::Error> {
          let len = len as usize;
          caller.data_mut().count_import()?;
          if len == 0
            || len > MAX_MESSAGE_BYTES
            || caller.data().sent_bytes.saturating_add(len) > 2 * MAX_MESSAGE_BYTES
            || caller.data().outbox.len() >= 128
          {
            return Err(wasmi::Error::new("backend outbox limit exceeded"));
          }
          let memory = exported_memory(&caller)?;
          let data = memory.data(&caller);
          let record = data
            .get(
              ptr as usize
                ..(ptr as usize)
                  .checked_add(len)
                  .ok_or_else(|| wasmi::Error::new("invalid backend pointer"))?,
            )
            .ok_or_else(|| wasmi::Error::new("invalid backend pointer"))?;
          let record = serde_json::from_slice(record)
            .map_err(|_| wasmi::Error::new("invalid backend message"))?;
          caller.data_mut().sent_bytes += len;
          caller.data_mut().outbox.push(record);
          Ok(0)
        },
      )
      .map_err(HostError::new)?;
    linker
      .func_wrap(
        "apell",
        "host_value",
        |mut caller: Caller<'_, VmData>,
         kind: i32,
         ptr: u32,
         capacity: u32|
         -> Result<i32, wasmi::Error> {
          caller.data_mut().count_import()?;
          let value = match kind {
            1 => uuid::Uuid::new_v4().to_string(),
            2 => OffsetDateTime::now_utc()
              .format(format_description!(
                "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
              ))
              .map_err(|_| wasmi::Error::new("could not read current time"))?,
            _ => return Err(wasmi::Error::new("unknown host value")),
          };
          if value.len() > capacity as usize {
            return Err(wasmi::Error::new("backend host value buffer is too small"));
          }
          exported_memory(&caller)?
            .write(&mut caller, ptr as usize, value.as_bytes())
            .map_err(|_| wasmi::Error::new("invalid backend pointer"))?;
          Ok(value.len() as i32)
        },
      )
      .map_err(HostError::new)?;
    let instance = linker
      .instantiate_and_start(&mut store, &module)
      .map_err(|error| HostError::new(format!("Could not instantiate backend: {error}")))?;
    let memory = instance
      .get_memory(&store, "memory")
      .ok_or_else(|| HostError::new("Backend must export its linear memory."))?;
    let alloc = instance
      .get_typed_func(&store, "apell_alloc")
      .map_err(HostError::new)?;
    let free = instance
      .get_typed_func(&store, "apell_free")
      .map_err(HostError::new)?;
    let dispatch = instance
      .get_typed_func(&store, "apell_dispatch")
      .map_err(HostError::new)?;
    Ok(Self {
      store,
      memory,
      alloc,
      free,
      dispatch,
    })
  }

  pub fn turn(&mut self, bytes: &[u8]) -> Result<Vec<Outbound>, HostError> {
    let len = u32::try_from(bytes.len()).map_err(HostError::new)?;
    self.store.set_fuel(TURN_FUEL).map_err(HostError::new)?;
    self.store.data_mut().sent_bytes = 0;
    self.store.data_mut().imports = 0;
    let ptr = self.alloc.call(&mut self.store, len).map_err(trap)?;
    if ptr == 0 {
      return Err(HostError::new("Backend allocation failed."));
    }
    self
      .memory
      .write(&mut self.store, ptr as usize, bytes)
      .map_err(HostError::new)?;
    let status = self
      .dispatch
      .call(&mut self.store, (ptr, len))
      .map_err(trap)?;
    if status != 0 {
      return Err(HostError::new("Backend rejected its ABI dispatch."));
    }
    self.free.call(&mut self.store, (ptr, len)).map_err(trap)?;
    Ok(std::mem::take(&mut self.store.data_mut().outbox))
  }
}

fn exported_memory(caller: &Caller<'_, VmData>) -> Result<Memory, wasmi::Error> {
  caller
    .get_export("memory")
    .and_then(|item| item.into_memory())
    .ok_or_else(|| wasmi::Error::new("missing backend linear memory"))
}

fn trap(error: wasmi::Error) -> HostError {
  HostError::new(format!("Backend execution stopped: {error}"))
}

// Validate event size before retaining it in the bounded broadcast ring.
pub(crate) fn check_event(event: &serde_json::Value) -> Result<(), HostError> {
  bounded_json(event, 256 * 1024).map(|_| ())
}
