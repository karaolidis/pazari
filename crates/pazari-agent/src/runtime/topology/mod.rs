pub mod uevent;

use std::{
    num::NonZeroU64,
    path::{Path, PathBuf},
};

pub use crate::runtime::topology::uevent::{Hotplug, HotplugListener};
use crate::runtime::{config::Paths, error::RuntimeError};

/// Bytes in the kilobyte `/proc/meminfo` counts in.
const KB: u64 = 1024;

/// The `/proc/meminfo` fields the agent reads, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemInfo {
    /// `MemTotal`.
    pub total: u64,

    /// `MemAvailable`.
    pub available: u64,

    /// `SwapTotal`.
    pub swap_total: u64,
}

/// One-shot reads of the guest's memory geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    meminfo: PathBuf,
    block_size: PathBuf,
}

impl Topology {
    #[must_use]
    pub fn new(paths: &Paths) -> Self {
        Self {
            meminfo: paths.meminfo.clone(),
            block_size: paths.block_size.clone(),
        }
    }

    pub async fn meminfo(&self) -> Result<MemInfo, RuntimeError> {
        let text = tokio::fs::read_to_string(&self.meminfo)
            .await
            .map_err(|source| RuntimeError::Read {
                path: self.meminfo.clone(),
                source,
            })?;

        parse_meminfo(&self.meminfo, &text)
    }

    pub async fn plugged(&self) -> Result<u64, RuntimeError> {
        Ok(self.meminfo().await?.total)
    }

    pub async fn block_size(&self) -> Result<NonZeroU64, RuntimeError> {
        let text = tokio::fs::read_to_string(&self.block_size)
            .await
            .map_err(|source| RuntimeError::Read {
                path: self.block_size.clone(),
                source,
            })?;

        parse_block_size(&self.block_size, &text)
    }
}

impl Default for Topology {
    fn default() -> Self {
        Self::new(&Paths::default())
    }
}

/// Reads `/proc/meminfo`, whose every line is `Key:` then a count in kB.
fn parse_meminfo(path: &Path, text: &str) -> Result<MemInfo, RuntimeError> {
    let mut total = None;
    let mut available = None;
    let mut swap_total = None;

    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };

        let slot = match key {
            "MemTotal" => &mut total,
            "MemAvailable" => &mut available,
            "SwapTotal" => &mut swap_total,
            _ => continue,
        };

        let kilobytes: u64 = rest
            .split_whitespace()
            .next()
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| RuntimeError::Malformed {
                path: path.to_path_buf(),
                value: line.trim().to_owned(),
                expected: "a count of kB",
            })?;

        *slot = Some(kilobytes.saturating_mul(KB));
    }

    Ok(MemInfo {
        total: total.ok_or_else(|| RuntimeError::Absent {
            path: path.to_path_buf(),
            key: "MemTotal",
        })?,
        available: available.ok_or_else(|| RuntimeError::Absent {
            path: path.to_path_buf(),
            key: "MemAvailable",
        })?,
        swap_total: swap_total.unwrap_or(0),
    })
}

fn parse_block_size(path: &Path, text: &str) -> Result<NonZeroU64, RuntimeError> {
    const EXPECTED: &str = "a positive hexadecimal byte count";

    let text = text.trim();

    u64::from_str_radix(text, 16)
        .ok()
        .and_then(NonZeroU64::new)
        .ok_or_else(|| RuntimeError::Malformed {
            path: path.to_path_buf(),
            value: text.to_owned(),
            expected: EXPECTED,
        })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::runtime::testing::{BLOCK_SIZE, MEMINFO};

    fn path() -> &'static Path {
        Path::new("/proc/meminfo")
    }

    #[test]
    fn meminfo_reads_the_three_fields_in_bytes() {
        assert_eq!(
            parse_meminfo(path(), MEMINFO).expect("a real /proc/meminfo"),
            MemInfo {
                total: 2_096_756 * KB,
                available: 1_729_396 * KB,
                swap_total: 1_048_572 * KB,
            }
        );
    }

    #[test]
    fn a_guest_without_swap_has_no_swap_total_line() {
        let info = parse_meminfo(path(), "MemTotal:       1024 kB\nMemAvailable:    512 kB\n")
            .expect("swap is optional");

        assert_eq!(info.swap_total, 0);
        assert_eq!(info.total, 1024 * KB);
    }

    #[test]
    fn a_meminfo_without_the_fields_the_agent_needs_is_rejected() {
        let error = parse_meminfo(path(), "MemFree:  1 kB\n").expect_err("no MemTotal");

        assert_eq!(error.to_string(), "/proc/meminfo carries no MemTotal line");

        assert!(parse_meminfo(path(), "MemTotal:  1 kB\n").is_err());
        assert!(parse_meminfo(path(), "MemTotal:  wat kB\nMemAvailable: 1 kB\n").is_err());
    }

    #[test]
    fn a_block_size_is_lowercase_hexadecimal() {
        let path = Path::new("/sys/devices/system/memory/block_size_bytes");

        assert_eq!(
            parse_block_size(path, BLOCK_SIZE)
                .expect("a real block_size_bytes")
                .get(),
            128 << 20
        );
        assert_eq!(
            parse_block_size(path, "200000")
                .expect("a two megabyte block")
                .get(),
            2 << 20
        );

        let error = parse_block_size(path, "0\n").expect_err("a block of nothing");

        assert_eq!(
            error.to_string(),
            "/sys/devices/system/memory/block_size_bytes holds \"0\", which is not a positive hexadecimal byte count"
        );

        assert!(parse_block_size(path, "0x8000000").is_err());
        assert!(parse_block_size(path, "").is_err());
    }

    #[tokio::test]
    async fn a_probe_reads_the_guest_it_is_pointed_at() {
        let dir = TempDir::new().unwrap();
        let paths = Paths::under(dir.path());

        fs::create_dir_all(paths.meminfo.parent().unwrap()).unwrap();
        fs::create_dir_all(paths.block_size.parent().unwrap()).unwrap();
        fs::write(&paths.meminfo, MEMINFO).unwrap();
        fs::write(&paths.block_size, BLOCK_SIZE).unwrap();

        let topology = Topology::new(&paths);

        assert_eq!(topology.plugged().await.unwrap(), 2_096_756 * KB);
        assert_eq!(topology.block_size().await.unwrap().get(), 128 << 20);
        assert_eq!(topology.meminfo().await.unwrap().available, 1_729_396 * KB);
    }
}
