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

impl Cgroup for AnyCgroup {
    async fn set_high(&self, limit: Limit) -> Result<(), RuntimeError> {
        match self {
            Self::Raw(cgroup) => cgroup.set_high(limit).await,
            Self::Systemd(cgroup) => cgroup.set_high(limit).await,
        }
    }

    async fn set_max(&self, limit: Limit) -> Result<(), RuntimeError> {
        match self {
            Self::Raw(cgroup) => cgroup.set_max(limit).await,
            Self::Systemd(cgroup) => cgroup.set_max(limit).await,
        }
    }

    async fn set_oom_group(&self, group: bool) -> Result<(), RuntimeError> {
        match self {
            Self::Raw(cgroup) => cgroup.set_oom_group(group).await,
            Self::Systemd(cgroup) => cgroup.set_oom_group(group).await,
        }
    }

    async fn events(&self) -> Result<MemoryEvents, RuntimeError> {
        match self {
            Self::Raw(cgroup) => cgroup.events().await,
            Self::Systemd(cgroup) => cgroup.events().await,
        }
    }

    async fn current(&self) -> Result<u64, RuntimeError> {
        match self {
            Self::Raw(cgroup) => cgroup.current().await,
            Self::Systemd(cgroup) => cgroup.current().await,
        }
    }

    async fn swap_current(&self) -> Result<u64, RuntimeError> {
        match self {
            Self::Raw(cgroup) => cgroup.swap_current().await,
            Self::Systemd(cgroup) => cgroup.swap_current().await,
        }
    }

    async fn pressure(&self) -> Result<Pressure, RuntimeError> {
        match self {
            Self::Raw(cgroup) => cgroup.pressure().await,
            Self::Systemd(cgroup) => cgroup.pressure().await,
        }
    }

    async fn reclaim(
        &self,
        bytes: u64,
        swappiness: Option<Swappiness>,
    ) -> Result<Reclaimed, RuntimeError> {
        match self {
            Self::Raw(cgroup) => cgroup.reclaim(bytes, swappiness).await,
            Self::Systemd(cgroup) => cgroup.reclaim(bytes, swappiness).await,
        }
    }
}
