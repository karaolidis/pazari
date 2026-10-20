pub mod qmp;

use std::{
    future::{self, Future},
    io,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
};

use pazari_types::{MemInfo, Pressure};
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use crate::runtime::{
    actuator::{Actuator, Reading},
    error::{ResizeError, RuntimeError},
    memory::{self, HostMemory},
    qmp::{QmpError, SocketOp},
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

/// Where the mock device claims to live.
const DEVICE: &str = "/run/pazari/mock.qmp";

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
    refuse: bool,
    write_failures: usize,

    opens: usize,
    open_refusals: usize,
    read_refusals: usize,
}

/// What the device does next, as [`Actuator::changed`] sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    Resized(u64),
    Lost,
}

#[derive(Debug)]
struct Shared {
    state: Mutex<State>,
    changes: mpsc::UnboundedSender<Change>,
    pending: AsyncMutex<mpsc::UnboundedReceiver<Change>>,
}

/// A memory device; clones are the same device.
#[derive(Debug, Clone)]
pub struct MockActuator {
    base: u64,
    completion: Completion,
    shared: Arc<Shared>,
}

impl MockActuator {
    #[must_use]
    pub fn new(size: u64) -> Self {
        let (changes, pending) = mpsc::unbounded_channel();

        Self {
            base: size,
            completion: Completion::Immediate,
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    size,
                    requested: size,
                    ..State::default()
                }),
                changes,
                pending: AsyncMutex::new(pending),
            }),
        }
    }

    #[must_use]
    pub const fn completion(mut self, completion: Completion) -> Self {
        self.completion = completion;
        self
    }

    #[must_use]
    pub fn refusing(self) -> Self {
        self.locked().refuse = true;
        self
    }

    /// The next writes are lost on the wire rather than refused.
    #[must_use]
    pub fn failing_writes(self, times: usize) -> Self {
        self.locked().write_failures = times;
        self
    }

    #[must_use]
    pub fn refusing_to_open(self, times: usize) -> Self {
        self.locked().open_refusals = times;
        self
    }

    #[must_use]
    pub fn refusing_to_be_read(self, times: usize) -> Self {
        self.locked().read_refusals = times;
        self
    }

    /// Every size asked for, taken or not.
    #[must_use]
    pub fn calls(&self) -> Vec<u64> {
        self.locked().calls.clone()
    }

    #[must_use]
    pub fn opens(&self) -> usize {
        self.locked().opens
    }

    /// Opens the device, unless it is still refusing to be opened.
    pub fn open(&self) -> Result<Self, RuntimeError> {
        let mut state = self.locked();

        state.opens = state.opens.saturating_add(1);

        if state.open_refusals > 0 {
            state.open_refusals -= 1;
            drop(state);

            return Err(socket(SocketOp::Connect, io::ErrorKind::ConnectionRefused));
        }

        drop(state);

        Ok(self.clone())
    }

    /// The device moves to `size` of its own accord.
    pub fn plug(&self, size: u64) {
        self.locked().size = size;
        self.change(Change::Resized(size));
    }

    /// The device's connection drops.
    pub fn kill(&self) {
        self.change(Change::Lost);
    }

    /// The device comes back at `size` the next time it is read.
    pub fn rebooted(&self, size: u64) {
        self.locked().size = size;
    }

    fn change(&self, change: Change) {
        self.shared
            .changes
            .send(change)
            .expect("the mock holds its own receiver");
    }

    fn locked(&self) -> MutexGuard<'_, State> {
        self.shared.state.lock().expect("no test poisons the mock")
    }

    fn read(&self) -> Result<Reading, RuntimeError> {
        let mut state = self.locked();

        if state.read_refusals > 0 {
            state.read_refusals -= 1;
            drop(state);

            return Err(socket(SocketOp::Read, io::ErrorKind::ConnectionReset));
        }

        let size = state.size;
        drop(state);

        Ok(Reading::sized(size, self.base))
    }

    fn write(&self, bytes: u64) -> Result<(), RuntimeError> {
        let mut state = self.locked();

        state.calls.push(bytes);

        if state.write_failures > 0 {
            state.write_failures -= 1;
            drop(state);

            return Err(socket(SocketOp::Write, io::ErrorKind::BrokenPipe));
        }

        if state.refuse {
            drop(state);

            return Err(ResizeError::Refused {
                bytes,
                reason: "the mock refuses every size".to_owned(),
            }
            .into());
        }

        state.requested = bytes;

        let completed = match self.completion {
            Completion::Immediate => Some(bytes),
            Completion::Partial(size) => Some(size),
            Completion::Stuck => None,
        };

        let moved = completed.filter(|size| *size != state.size);

        if let Some(size) = moved {
            state.size = size;
        }

        drop(state);

        if let Some(size) = moved {
            self.change(Change::Resized(size));
        }

        Ok(())
    }
}

impl Actuator for MockActuator {
    fn reading(&self) -> impl Future<Output = Result<Reading, RuntimeError>> + Send {
        future::ready(self.read())
    }

    fn requested(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send {
        future::ready(Ok(self.locked().requested))
    }

    fn set_requested(&self, bytes: u64) -> impl Future<Output = Result<(), RuntimeError>> + Send {
        future::ready(self.write(bytes))
    }

    async fn changed(&self) -> Result<Reading, RuntimeError> {
        let mut pending = self.shared.pending.lock().await;

        match pending.recv().await {
            Some(Change::Resized(size)) => Ok(Reading::sized(size, self.base)),
            Some(Change::Lost) => Err(QmpError::Closed {
                path: PathBuf::from(DEVICE),
            }
            .into()),
            None => future::pending().await,
        }
    }
}

fn socket(op: SocketOp, kind: io::ErrorKind) -> RuntimeError {
    QmpError::Socket {
        op,
        path: PathBuf::from(DEVICE),
        source: io::Error::from(kind),
    }
    .into()
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
