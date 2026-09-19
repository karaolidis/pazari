use std::path::{Path, PathBuf};

use pazari_types::Swappiness;
use zbus::{Connection, proxy, zvariant::Value};

use crate::runtime::{
    cgroup::{
        Cgroup, Reclaimed,
        parse::{Limit, MemoryEvents, Pressure},
        raw::RawCgroup,
    },
    error::RuntimeError,
};

const MEMORY_HIGH: &str = "MemoryHigh";
const MEMORY_MAX: &str = "MemoryMax";

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
    pub fn unit(&self) -> &str {
        &self.unit
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        self.files.path()
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

    async fn set_oom_group(&self, group: bool) -> Result<(), RuntimeError> {
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

/// The cgroup systemd gives a unit under the unified hierarchy.
#[must_use]
pub fn unit_cgroup(root: &Path, unit: &str) -> PathBuf {
    let mut path = root.to_path_buf();

    if unit == "-.slice" {
        return path;
    }

    match unit.strip_suffix(".slice") {
        Some(name) => {
            let parts: Vec<&str> = name.split('-').collect();

            for depth in 1..parts.len() {
                path.push(format!("{}.slice", parts[..depth].join("-")));
            }
        }
        None => path.push("system.slice"),
    }

    path.push(unit);

    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> &'static Path {
        Path::new("/sys/fs/cgroup")
    }

    #[test]
    fn a_unit_without_a_slice_lands_in_the_system_slice() {
        assert_eq!(
            unit_cgroup(root(), "workload.service"),
            Path::new("/sys/fs/cgroup/system.slice/workload.service")
        );
        assert_eq!(
            unit_cgroup(root(), "workload.scope"),
            Path::new("/sys/fs/cgroup/system.slice/workload.scope")
        );
    }

    #[test]
    fn a_slice_nests_under_the_slices_its_name_spells_out() {
        assert_eq!(
            unit_cgroup(root(), "system.slice"),
            Path::new("/sys/fs/cgroup/system.slice")
        );
        assert_eq!(
            unit_cgroup(root(), "machine-qemu.slice"),
            Path::new("/sys/fs/cgroup/machine.slice/machine-qemu.slice")
        );
        assert_eq!(
            unit_cgroup(root(), "a-b-c.slice"),
            Path::new("/sys/fs/cgroup/a.slice/a-b.slice/a-b-c.slice")
        );
    }

    #[test]
    fn the_root_slice_is_the_hierarchy_root() {
        assert_eq!(unit_cgroup(root(), "-.slice"), root());
    }
}
