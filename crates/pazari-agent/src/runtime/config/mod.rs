use std::{
    num::NonZeroU64,
    path::{Path, PathBuf},
};

use pazari_types::{Interval, Swappiness};

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
        Self::under(Path::new("/"))
    }
}

fn rooted(root: &Path, absolute: &str) -> PathBuf {
    root.join(absolute.trim_start_matches('/'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    Cgroupfs,

    Systemd { unit: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReclaimConfig {
    pub chunk: NonZeroU64,
    pub swappiness: Option<Swappiness>,
}

/// What preflight makes of transparent hugepages set to `always`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThpAlways {
    Fail,
    Warn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub tick_interval: Interval,

    pub cgroup: PathBuf,
    pub backend: Backend,

    pub hotplug_debounce: Interval,

    pub reclaim: ReclaimConfig,

    pub thp_always: ThpAlways,

    pub paths: Paths,
}
