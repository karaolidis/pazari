use std::path::PathBuf;

use pazari_types::{Pressure, Swappiness};
use zbus::{Connection, proxy, zvariant::Value};

use crate::{
    engine::OomGroup,
    runtime::{
        cgroup::{
            Cgroup, Reclaimed,
            parse::{Limit, MemoryEvents},
            raw::RawCgroup,
        },
        error::RuntimeError,
    },
};

const MEMORY_HIGH: &str = "MemoryHigh";
const MEMORY_MAX: &str = "MemoryMax";
const INFINITY: &str = "infinity";

#[proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait Systemd1Manager {
    fn set_unit_properties(
        &self,
        name: &str,
        runtime: bool,
        properties: &[(&str, Value<'_>)],
    ) -> zbus::Result<()>;
}

/// Actuates the limits through systemd, and everything else through cgroupfs.
#[derive(Debug)]
pub struct SystemdCgroup {
    manager: Systemd1ManagerProxy<'static>,
    unit: String,
    files: RawCgroup,
}

impl SystemdCgroup {
    pub async fn connect(
        unit: impl Into<String>,
        cgroup: impl Into<PathBuf>,
    ) -> Result<Self, RuntimeError> {
        let connection = Connection::system()
            .await
            .map_err(|source| RuntimeError::Bus {
                source: Box::new(source),
            })?;

        let manager = Systemd1ManagerProxy::new(&connection)
            .await
            .map_err(|source| RuntimeError::Bus {
                source: Box::new(source),
            })?;

        Ok(Self {
            manager,
            unit: unit.into(),
            files: RawCgroup::new(cgroup),
        })
    }

    #[must_use]
    pub fn released(&self) -> String {
        format!("{} {MEMORY_HIGH} = {INFINITY}", self.unit)
    }

    async fn set(&self, property: &'static str, limit: Limit) -> Result<(), RuntimeError> {
        self.manager
            .set_unit_properties(
                &self.unit,
                true,
                &[(property, Value::U64(limit.or_infinity()))],
            )
            .await
            .map_err(|source| RuntimeError::Property {
                unit: self.unit.clone(),
                property,
                source: Box::new(source),
            })
    }
}

impl Cgroup for SystemdCgroup {
    async fn set_high(&self, limit: Limit) -> Result<(), RuntimeError> {
        self.set(MEMORY_HIGH, limit).await
    }

    async fn set_max(&self, limit: Limit) -> Result<(), RuntimeError> {
        self.set(MEMORY_MAX, limit).await
    }

    async fn set_oom_group(&self, group: OomGroup) -> Result<(), RuntimeError> {
        self.files.set_oom_group(group).await
    }

    async fn events(&self) -> Result<MemoryEvents, RuntimeError> {
        self.files.events().await
    }

    async fn current(&self) -> Result<u64, RuntimeError> {
        self.files.current().await
    }

    async fn swap_current(&self) -> Result<u64, RuntimeError> {
        self.files.swap_current().await
    }

    async fn pressure(&self) -> Result<Pressure, RuntimeError> {
        self.files.pressure().await
    }

    async fn reclaim(
        &self,
        bytes: u64,
        swappiness: Option<Swappiness>,
    ) -> Result<Reclaimed, RuntimeError> {
        self.files.reclaim(bytes, swappiness).await
    }
}
