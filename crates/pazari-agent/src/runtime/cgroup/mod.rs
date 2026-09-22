pub mod parse;
pub mod raw;
pub mod systemd;

use std::future::Future;

use pazari_types::{Pressure, Swappiness};

pub use crate::runtime::cgroup::{
    parse::{Limit, MemoryEvents},
    raw::RawCgroup,
    systemd::SystemdCgroup,
};
use crate::runtime::{
    config::{Backend, RuntimeConfig},
    error::RuntimeError,
};

pub const MEMORY_HIGH: &str = "memory.high";
pub const MEMORY_MAX: &str = "memory.max";
pub const MEMORY_OOM_GROUP: &str = "memory.oom.group";
pub const MEMORY_RECLAIM: &str = "memory.reclaim";
pub const MEMORY_EVENTS: &str = "memory.events";
pub const MEMORY_CURRENT: &str = "memory.current";
pub const MEMORY_PRESSURE: &str = "memory.pressure";
pub const MEMORY_SWAP_CURRENT: &str = "memory.swap.current";
pub const MEMORY_SWAP_MAX: &str = "memory.swap.max";

/// What one `memory.reclaim` write achieved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reclaimed {
    Whole,

    /// `EAGAIN`: the kernel freed some of it.
    Partial,
}

pub trait Cgroup: Send + Sync {
    fn set_high(&self, limit: Limit) -> impl Future<Output = Result<(), RuntimeError>> + Send;

    fn set_max(&self, limit: Limit) -> impl Future<Output = Result<(), RuntimeError>> + Send;

    fn set_oom_group(&self, group: bool) -> impl Future<Output = Result<(), RuntimeError>> + Send;

    fn events(&self) -> impl Future<Output = Result<MemoryEvents, RuntimeError>> + Send;

    fn current(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send;

    fn swap_current(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send;

    fn pressure(&self) -> impl Future<Output = Result<Pressure, RuntimeError>> + Send;

    fn reclaim(
        &self,
        bytes: u64,
        swappiness: Option<Swappiness>,
    ) -> impl Future<Output = Result<Reclaimed, RuntimeError>> + Send;
}

#[derive(Debug)]
pub enum AnyCgroup {
    Raw(RawCgroup),
    Systemd(SystemdCgroup),
}

impl AnyCgroup {
    pub async fn open(config: &RuntimeConfig) -> Result<Self, RuntimeError> {
        Ok(match &config.backend {
            Backend::Cgroupfs => Self::Raw(RawCgroup::new(&config.cgroup)),
            Backend::Systemd { unit } => {
                Self::Systemd(SystemdCgroup::connect(unit.clone(), &config.cgroup).await?)
            }
        })
    }

    #[must_use]
    pub fn released(&self) -> String {
        match self {
            Self::Raw(cgroup) => cgroup.released(),
            Self::Systemd(cgroup) => cgroup.released(),
        }
    }
}

/// Forwards each [`Cgroup`] method to whichever backend is behind the enum.
macro_rules! forward {
    ($( $name:ident($($arg:ident: $ty:ty),*) -> $ret:ty; )*) => {
        $(
            async fn $name(&self, $($arg: $ty),*) -> Result<$ret, RuntimeError> {
                match self {
                    Self::Raw(cgroup) => cgroup.$name($($arg),*).await,
                    Self::Systemd(cgroup) => cgroup.$name($($arg),*).await,
                }
            }
        )*
    };
}

impl Cgroup for AnyCgroup {
    forward! {
        set_high(limit: Limit) -> ();
        set_max(limit: Limit) -> ();
        set_oom_group(group: bool) -> ();
        events() -> MemoryEvents;
        current() -> u64;
        swap_current() -> u64;
        pressure() -> Pressure;
        reclaim(bytes: u64, swappiness: Option<Swappiness>) -> Reclaimed;
    }
}
