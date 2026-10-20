pub mod qmp;

use std::{
    collections::VecDeque,
    future::{self, Future},
    io,
    path::PathBuf,
    sync::{Mutex, MutexGuard},
};

use pazari_types::{MemInfo, Pressure};
use tokio::task;

use crate::runtime::{
    actuator::{Actuator, Reading},
    error::RuntimeError,
    memory::{self, HostMemory},
};

pub const MEMINFO: &str = "MemTotal:        2096756 kB\n\
                           MemFree:          131072 kB\n\
                           MemAvailable:    1729396 kB\n\
                           Buffers:           16384 kB\n\
                           Cached:          1600000 kB\n\
                           SwapCached:            0 kB\n\
                           SwapTotal:       1048572 kB\n\
                           SwapFree:        1048572 kB\n";

pub const PRESSURE: &str = "some avg10=12.50 avg60=3.00 avg300=1.00 total=123456\n\
                            full avg10=0.25 avg60=0.10 avg300=0.05 total=6543\n";

pub const SWAPS: &str = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n\
                         /dev/zram0                              partition\t1048572\t\t0\t\t100\n\
                         /swap/swapfile                          file\t\t2097148\t\t0\t\t-2\n";

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

#[derive(Debug)]
pub struct MockActuator {
    base: u64,
    completion: Completion,
    refuse: bool,
    state: Mutex<State>,
}

impl MockActuator {
    #[must_use]
    pub fn new(size: u64) -> Self {
        Self {
            base: size,
            completion: Completion::Immediate,
            refuse: false,
            state: Mutex::new(State {
                size,
                requested: size,
                ..State::default()
            }),
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
    fn reading(&self) -> impl Future<Output = Result<Reading, RuntimeError>> + Send {
        let size = self.locked().size;

        future::ready(Ok(Reading::sized(size, self.base)))
    }

    fn requested(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send {
        future::ready(Ok(self.locked().requested))
    }

    fn set_requested(&self, bytes: u64) -> impl Future<Output = Result<(), RuntimeError>> + Send {
        future::ready(self.write(bytes))
    }

    async fn changed(&self) -> Result<Reading, RuntimeError> {
        loop {
            if let Some(size) = self.take() {
                task::yield_now().await;

                return Ok(Reading::sized(size, self.base));
            }

            future::pending::<()>().await;
        }
    }
}

#[must_use]
pub const fn meminfo(total: u64, available: u64, swap_total: u64) -> MemInfo {
    MemInfo {
        total,
        free: available,
        available,
        swap_total,
        swap_free: swap_total,
    }
}

#[derive(Debug, Default)]
pub struct MockMemory {
    meminfo: Mutex<MemInfo>,
    pressure: Option<Pressure>,
    swaps: Vec<String>,
    blind: bool,
}

impl MockMemory {
    #[must_use]
    pub fn new(meminfo: MemInfo) -> Self {
        Self {
            meminfo: Mutex::new(meminfo),
            ..Self::default()
        }
    }

    #[must_use]
    pub const fn pressured(mut self, some10: f64) -> Self {
        self.pressure = Some(Pressure {
            some10,
            full10: 0.0,
        });
        self
    }

    #[must_use]
    pub fn swapping(mut self, devices: &[&str]) -> Self {
        self.swaps = devices.iter().map(|device| (*device).to_owned()).collect();
        self
    }

    #[must_use]
    pub const fn unreadable_swaps(mut self) -> Self {
        self.blind = true;
        self
    }

    pub fn set(&self, meminfo: MemInfo) {
        *self.meminfo.lock().expect("no test poisons the mock") = meminfo;
    }
}

impl HostMemory for MockMemory {
    fn meminfo(&self) -> impl Future<Output = Result<MemInfo, RuntimeError>> + Send {
        future::ready(Ok(*self.meminfo.lock().expect("no test poisons the mock")))
    }

    fn pressure(&self) -> impl Future<Output = Result<Option<Pressure>, RuntimeError>> + Send {
        future::ready(Ok(self.pressure))
    }

    fn swaps(&self) -> impl Future<Output = Result<Vec<String>, RuntimeError>> + Send {
        future::ready(if self.blind {
            Err(RuntimeError::Read {
                path: PathBuf::from(memory::SWAPS),
                source: io::Error::from(io::ErrorKind::PermissionDenied),
            })
        } else {
            Ok(self.swaps.clone())
        })
    }
}
