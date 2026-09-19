use std::{
    collections::VecDeque,
    fs,
    future::{self, Future},
    io,
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::Mutex,
};

use pazari_types::Swappiness;
use tempfile::TempDir;
use tokio::task;

use crate::runtime::{
    cgroup::{
        Cgroup, Limit, MEMORY_CURRENT, MEMORY_EVENTS, MEMORY_HIGH, MEMORY_MAX, MEMORY_OOM_GROUP,
        MEMORY_PRESSURE, MEMORY_RECLAIM, MEMORY_SWAP_CURRENT, MEMORY_SWAP_MAX, MemoryEvents,
        Pressure, Reclaimed,
    },
    config::{Paths, RuntimeConfig},
    error::RuntimeError,
};

pub const MIB: u64 = 1 << 20;

pub const EVENTS: &str = "low 0\nhigh 37\nmax 2\noom 0\noom_kill 0\noom_group_kill 0\n";

pub const PRESSURE: &str = "some avg10=12.50 avg60=3.75 avg300=0.42 total=1234567\n\
                            full avg10=0.25 avg60=0.00 avg300=0.00 total=42\n";

pub const MEMINFO: &str = "MemTotal:        2096756 kB\n\
                           MemFree:          131072 kB\n\
                           MemAvailable:    1729396 kB\n\
                           Buffers:           16384 kB\n\
                           Cached:          1600000 kB\n\
                           SwapCached:            0 kB\n\
                           SwapTotal:       1048572 kB\n\
                           SwapFree:        1048572 kB\n\
                           HugePages_Total:       0\n";

pub const BLOCK_SIZE: &str = "8000000\n";

const FILES: [&str; 9] = [
    MEMORY_HIGH,
    MEMORY_MAX,
    MEMORY_OOM_GROUP,
    MEMORY_RECLAIM,
    MEMORY_EVENTS,
    MEMORY_CURRENT,
    MEMORY_PRESSURE,
    MEMORY_SWAP_CURRENT,
    MEMORY_SWAP_MAX,
];

pub fn bytes(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("a positive byte count")
}

pub fn cgroup_dir() -> TempDir {
    let dir = TempDir::new().expect("a temporary directory");

    for name in FILES {
        fs::write(dir.path().join(name), "").expect("an empty cgroup file");
    }

    dir
}

#[derive(Debug)]
pub struct Guest {
    pub config: RuntimeConfig,

    #[expect(dead_code, reason = "the tree lives as long as the fixture")]
    dir: TempDir,
}

impl Guest {
    pub fn new() -> Self {
        let dir = TempDir::new().expect("a temporary directory");
        let paths = Paths::under(dir.path());
        let cgroup = paths.cgroup_root.join("workload.slice");

        let guest = Self {
            config: RuntimeConfig {
                paths,
                ..RuntimeConfig::new(cgroup, bytes(2048 * MIB))
            },
            dir,
        };

        let paths = &guest.config.paths;

        Self::write(
            &paths.cgroup_root.join("cgroup.controllers"),
            "cpu memory pids\n",
        );
        Self::write(
            &paths.cgroup_root.join("cgroup.subtree_control"),
            "memory pids\n",
        );
        Self::write(&paths.meminfo, MEMINFO);
        Self::write(&paths.block_size, BLOCK_SIZE);
        Self::write(&paths.memmap_on_memory, "Y\n");
        Self::write(&paths.transparent_hugepage, "always [madvise] never\n");

        for name in FILES {
            Self::write(&guest.config.cgroup.join(name), "");
        }

        Self::write(&guest.config.cgroup.join(MEMORY_SWAP_MAX), "max\n");

        guest
    }

    pub fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("a fixture directory");
        }

        fs::write(path, text).expect("a fixture file");
    }

    pub fn remove(path: &Path) {
        fs::remove_file(path).expect("a fixture file to remove");
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    High(Limit),
    Max(Limit),
    OomGroup(bool),
    Reclaim {
        bytes: u64,
        swappiness: Option<Swappiness>,
    },
}

impl Call {
    const fn file(&self) -> &'static str {
        match self {
            Self::High(_) => MEMORY_HIGH,
            Self::Max(_) => MEMORY_MAX,
            Self::OomGroup(_) => MEMORY_OOM_GROUP,
            Self::Reclaim { .. } => MEMORY_RECLAIM,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Whole,
    Partial,
    Gone,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Reads {
    pub events: MemoryEvents,
    pub current: u64,
    pub swap: u64,
    pub pressure: Pressure,
}

#[derive(Debug, Default)]
pub struct FakeCgroup {
    reads: Reads,
    fail_at: Option<usize>,
    calls: Mutex<Vec<Call>>,
    answers: Mutex<VecDeque<Answer>>,
}

impl FakeCgroup {
    pub fn new(reads: Reads) -> Self {
        Self {
            reads,
            ..Self::default()
        }
    }

    pub fn scripted(answers: impl IntoIterator<Item = Answer>) -> Self {
        Self {
            answers: Mutex::new(answers.into_iter().collect()),
            ..Self::default()
        }
    }

    pub fn failing(at: usize) -> Self {
        Self {
            fail_at: Some(at),
            ..Self::default()
        }
    }

    pub fn calls(&self) -> Vec<Call> {
        self.locked_calls().clone()
    }

    pub fn reclaims(&self) -> usize {
        self.locked_calls()
            .iter()
            .filter(|call| matches!(call, Call::Reclaim { .. }))
            .count()
    }

    fn locked_calls(&self) -> std::sync::MutexGuard<'_, Vec<Call>> {
        self.calls.lock().expect("no test poisons the fake")
    }

    fn record(&self, call: Call) -> Result<(), RuntimeError> {
        let mut calls = self.locked_calls();

        if self.fail_at == Some(calls.len()) {
            return Err(refused(call.file()));
        }

        calls.push(call);
        drop(calls);

        Ok(())
    }
}

fn refused(file: &str) -> RuntimeError {
    RuntimeError::Write {
        path: PathBuf::from("/sys/fs/cgroup/fake").join(file),
        value: String::new(),
        source: io::Error::from(io::ErrorKind::PermissionDenied),
    }
}

impl Cgroup for FakeCgroup {
    fn set_high(&self, limit: Limit) -> impl Future<Output = Result<(), RuntimeError>> + Send {
        future::ready(self.record(Call::High(limit)))
    }

    fn set_max(&self, limit: Limit) -> impl Future<Output = Result<(), RuntimeError>> + Send {
        future::ready(self.record(Call::Max(limit)))
    }

    fn set_oom_group(&self, group: bool) -> impl Future<Output = Result<(), RuntimeError>> + Send {
        future::ready(self.record(Call::OomGroup(group)))
    }

    fn events(&self) -> impl Future<Output = Result<MemoryEvents, RuntimeError>> + Send {
        future::ready(Ok(self.reads.events))
    }

    fn current(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send {
        future::ready(Ok(self.reads.current))
    }

    fn swap_current(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send {
        future::ready(Ok(self.reads.swap))
    }

    fn pressure(&self) -> impl Future<Output = Result<Pressure, RuntimeError>> + Send {
        future::ready(Ok(self.reads.pressure))
    }

    async fn reclaim(
        &self,
        bytes: u64,
        swappiness: Option<Swappiness>,
    ) -> Result<Reclaimed, RuntimeError> {
        let answer = self
            .answers
            .lock()
            .expect("no test poisons the fake")
            .pop_front()
            .unwrap_or(Answer::Whole);

        if answer == Answer::Gone {
            return Err(refused(MEMORY_RECLAIM));
        }

        self.record(Call::Reclaim { bytes, swappiness })?;

        task::yield_now().await;

        Ok(if answer == Answer::Partial {
            Reclaimed::Partial
        } else {
            Reclaimed::Whole
        })
    }
}
