pub mod uevent;

use std::{
    num::NonZeroU64,
    path::{Path, PathBuf},
};

use pazari_types::{KernelFileError, MemInfo};

pub use crate::runtime::topology::uevent::HotplugListener;
use crate::runtime::{config::Paths, error::RuntimeError};

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

        Ok(MemInfo::parse(&self.meminfo, &text)?)
    }

    pub async fn total_mem(&self) -> Result<u64, RuntimeError> {
        Ok(self.meminfo().await?.total)
    }

    pub async fn block_size(&self) -> Result<NonZeroU64, RuntimeError> {
        let text = tokio::fs::read_to_string(&self.block_size)
            .await
            .map_err(|source| RuntimeError::Read {
                path: self.block_size.clone(),
                source,
            })?;

        Ok(parse_block_size(&self.block_size, &text)?)
    }
}

pub fn parse_block_size(path: &Path, text: &str) -> Result<NonZeroU64, KernelFileError> {
    const EXPECTED: &str = "a positive hexadecimal byte count";

    let text = text.trim();

    u64::from_str_radix(text, 16)
        .ok()
        .and_then(NonZeroU64::new)
        .ok_or_else(|| KernelFileError::Malformed {
            path: path.to_path_buf(),
            value: text.to_owned(),
            expected: EXPECTED,
        })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use pazari_types::kernel::meminfo::KB;

    use super::*;
    use crate::runtime::testing::{BLOCK_SIZE, MEMINFO};

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

        assert_eq!(topology.total_mem().await.unwrap(), 2_096_756 * KB);
        assert_eq!(topology.block_size().await.unwrap().get(), 128 << 20);
        assert_eq!(topology.meminfo().await.unwrap().available, 1_729_396 * KB);
    }
}
