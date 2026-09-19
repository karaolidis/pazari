pub mod qmp;

use std::{
    collections::VecDeque,
    future::{self, Future},
    sync::{Mutex, MutexGuard},
};

use tokio::task;

use crate::runtime::{actuator::Actuator, error::RuntimeError, memory::HostMemory};

pub const MEMINFO: &str = "MemTotal:        2096756 kB\n\
                           MemFree:          131072 kB\n\
                           MemAvailable:    1729396 kB\n\
                           Buffers:           16384 kB\n\
                           Cached:          1600000 kB\n\
                           SwapCached:            0 kB\n\
                           SwapTotal:       1048572 kB\n\
                           SwapFree:        1048572 kB\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Completion {
    #[default]
    Immediate,

    Partial(u64),

    Stuck,
}

#[derive(Debug, Default)]
struct State {
    size: u64,
    requested: u64,
    calls: Vec<u64>,

    completed: VecDeque<u64>,
}

#[derive(Debug, Default)]
pub struct MockActuator {
    completion: Completion,
    refuse: bool,
    state: Mutex<State>,
}

impl MockActuator {
    #[must_use]
    pub fn new(size: u64) -> Self {
        Self {
            state: Mutex::new(State {
                size,
                requested: size,
                ..State::default()
            }),
            ..Self::default()
        }
    }

    #[must_use]
    pub const fn completion(mut self, completion: Completion) -> Self {
        self.completion = completion;
        self
    }

    #[must_use]
    pub const fn refusing(mut self) -> Self {
        self.refuse = true;
        self
    }

    #[must_use]
    pub fn calls(&self) -> Vec<u64> {
        self.locked().calls.clone()
    }

    fn locked(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("no test poisons the mock")
    }

    fn write(&self, bytes: u64) -> Result<(), RuntimeError> {
        if self.refuse {
            return Err(RuntimeError::Device {
                bytes,
                reason: "the mock refuses every size".to_owned(),
            });
        }

        let mut state = self.locked();

        state.calls.push(bytes);
        state.requested = bytes;

        let completed = match self.completion {
            Completion::Immediate => Some(bytes),
            Completion::Partial(size) => Some(size),
            Completion::Stuck => None,
        };

        if let Some(size) = completed
            && size != state.size
        {
            state.size = size;
            state.completed.push_back(size);
        }

        drop(state);

        Ok(())
    }

    fn take(&self) -> Option<u64> {
        self.locked().completed.pop_front()
    }
}

impl Actuator for MockActuator {
    fn size(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send {
        future::ready(Ok(self.locked().size))
    }

    fn requested(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send {
        future::ready(Ok(self.locked().requested))
    }

    fn set_requested(&self, bytes: u64) -> impl Future<Output = Result<(), RuntimeError>> + Send {
        future::ready(self.write(bytes))
    }

    async fn changed(&self) -> Result<u64, RuntimeError> {
        loop {
            if let Some(size) = self.take() {
                task::yield_now().await;

                return Ok(size);
            }

            future::pending::<()>().await;
        }
    }
}

#[derive(Debug, Default)]
pub struct MockMemory {
    available: Mutex<u64>,
}

impl MockMemory {
    #[must_use]
    pub fn new(available: u64) -> Self {
        Self {
            available: Mutex::new(available),
        }
    }

    pub fn set(&self, available: u64) {
        *self.available.lock().expect("no test poisons the mock") = available;
    }
}

impl HostMemory for MockMemory {
    fn available(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send {
        future::ready(Ok(*self
            .available
            .lock()
            .expect("no test poisons the mock")))
    }
}
