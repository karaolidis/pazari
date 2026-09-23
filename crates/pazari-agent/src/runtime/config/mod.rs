use std::{
    num::NonZeroU64,
    path::{Path, PathBuf},
    time::Duration,
};

use pazari_types::Interval;

pub const CGROUP_ROOT: &str = "/sys/fs/cgroup";

pub const MEMINFO: &str = "/proc/meminfo";

pub const SELF_CGROUP: &str = "/proc/self/cgroup";

/// The virtio-mem block size, in lowercase hexadecimal with no `0x`.
pub const BLOCK_SIZE: &str = "/sys/devices/system/memory/block_size_bytes";

pub const AUTO_ONLINE_BLOCKS: &str = "/sys/devices/system/memory/auto_online_blocks";

pub const VIRTIO_DEVICES: &str = "/sys/bus/virtio/devices";

/// Holds a device id in hexadecimal, under each directory of `VIRTIO_DEVICES`.
pub const VIRTIO_DEVICE_ID: &str = "device";

pub const VIRTIO_BALLOON: u32 = 5;

/// The transparent hugepage mode, with the selected one in brackets.
pub const TRANSPARENT_HUGEPAGE: &str = "/sys/kernel/mm/transparent_hugepage/enabled";

/// 64 MiB: large enough that releasing a few hundred megabytes is a handful of
/// writes, small enough that a cancelled reclaim stops inside one of them.
pub const RECLAIM_CHUNK: u64 = 64 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub cgroup_root: PathBuf,
    pub self_cgroup: PathBuf,
    pub meminfo: PathBuf,
    pub block_size: PathBuf,
    pub auto_online_blocks: PathBuf,
    pub virtio_devices: PathBuf,
    pub transparent_hugepage: PathBuf,
}

impl Paths {
    #[must_use]
    pub fn under(root: &Path) -> Self {
        Self {
            cgroup_root: rooted(root, CGROUP_ROOT),
            self_cgroup: rooted(root, SELF_CGROUP),
            meminfo: rooted(root, MEMINFO),
            block_size: rooted(root, BLOCK_SIZE),
            auto_online_blocks: rooted(root, AUTO_ONLINE_BLOCKS),
            virtio_devices: rooted(root, VIRTIO_DEVICES),
            transparent_hugepage: rooted(root, TRANSPARENT_HUGEPAGE),
        }
    }
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            cgroup_root: PathBuf::from(CGROUP_ROOT),
            self_cgroup: PathBuf::from(SELF_CGROUP),
            meminfo: PathBuf::from(MEMINFO),
            block_size: PathBuf::from(BLOCK_SIZE),
            auto_online_blocks: PathBuf::from(AUTO_ONLINE_BLOCKS),
            virtio_devices: PathBuf::from(VIRTIO_DEVICES),
            transparent_hugepage: PathBuf::from(TRANSPARENT_HUGEPAGE),
        }
    }
}

fn rooted(root: &Path, absolute: &str) -> PathBuf {
    root.join(absolute.trim_start_matches('/'))
}

/// Which adapter writes `memory.high` and `memory.max`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    Cgroupfs,

    Systemd { unit: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub cgroup: PathBuf,

    pub backend: Backend,

    pub boot_mem: NonZeroU64,

    pub tick_interval: Interval,

    pub hotplug_debounce: Interval,

    pub reclaim_chunk: NonZeroU64,

    pub thp_always_fatal: bool,

    pub paths: Paths,
}

impl RuntimeConfig {
    #[must_use]
    pub fn new(cgroup: impl Into<PathBuf>, boot_mem: NonZeroU64) -> Self {
        Self {
            cgroup: cgroup.into(),
            backend: Backend::Cgroupfs,
            boot_mem,
            tick_interval: Interval::try_new(Duration::from_secs(1))
                .expect("a positive interval under a day"),
            hotplug_debounce: Interval::try_new(Duration::from_millis(250))
                .expect("a positive interval under a day"),
            reclaim_chunk: NonZeroU64::new(RECLAIM_CHUNK).expect("a positive byte count"),
            thp_always_fatal: true,
            paths: Paths::default(),
        }
    }

    #[must_use]
    pub fn unit(&self) -> Option<&str> {
        match &self.backend {
            Backend::Cgroupfs => None,
            Backend::Systemd { unit } => Some(unit),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> RuntimeConfig {
        RuntimeConfig::new(
            "/sys/fs/cgroup/workload.slice",
            NonZeroU64::new(2 << 30).expect("a positive byte count"),
        )
    }

    #[test]
    fn the_defaults_name_the_real_kernel_interfaces() {
        let config = config();

        assert_eq!(config.paths, Paths::default());
        assert_eq!(config.paths.meminfo, Path::new("/proc/meminfo"));
        assert_eq!(config.reclaim_chunk.get(), RECLAIM_CHUNK);
        assert!(config.thp_always_fatal);
        assert_eq!(config.unit(), None);
    }

    #[test]
    fn a_fixture_tree_carries_the_same_layout() {
        let paths = Paths::under(Path::new("/tmp/fixture"));

        assert_eq!(paths.cgroup_root, Path::new("/tmp/fixture/sys/fs/cgroup"));
        assert_eq!(paths.meminfo, Path::new("/tmp/fixture/proc/meminfo"));
        assert_eq!(
            paths.self_cgroup,
            Path::new("/tmp/fixture/proc/self/cgroup")
        );
        assert_eq!(
            paths.auto_online_blocks,
            Path::new("/tmp/fixture/sys/devices/system/memory/auto_online_blocks")
        );
        assert_eq!(
            paths.virtio_devices,
            Path::new("/tmp/fixture/sys/bus/virtio/devices")
        );
    }

    #[test]
    fn the_systemd_backend_names_its_unit() {
        let config = RuntimeConfig {
            backend: Backend::Systemd {
                unit: "workload.slice".to_owned(),
            },
            ..config()
        };

        assert_eq!(config.unit(), Some("workload.slice"));
    }
}
