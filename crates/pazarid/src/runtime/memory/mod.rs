use std::{
    future::Future,
    io,
    path::{Path, PathBuf},
};

use pazari_types::{MemInfo, Pressure};

use crate::runtime::error::RuntimeError;

pub const MEMINFO: &str = "/proc/meminfo";

pub const PRESSURE: &str = "/proc/pressure/memory";

pub const SWAPS: &str = "/proc/swaps";

pub trait HostMemory: Send + Sync {
    fn meminfo(&self) -> impl Future<Output = Result<MemInfo, RuntimeError>> + Send;

    /// `None` where the kernel carries no pressure accounting.
    fn pressure(&self) -> impl Future<Output = Result<Option<Pressure>, RuntimeError>> + Send;

    fn swaps(&self) -> impl Future<Output = Result<Vec<String>, RuntimeError>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcMemory {
    meminfo: PathBuf,
    pressure: PathBuf,
    swaps: PathBuf,
}

impl ProcMemory {
    #[must_use]
    pub fn new() -> Self {
        Self {
            meminfo: PathBuf::from(MEMINFO),
            pressure: PathBuf::from(PRESSURE),
            swaps: PathBuf::from(SWAPS),
        }
    }

    #[must_use]
    pub fn under(root: &Path) -> Self {
        Self {
            meminfo: root.join(MEMINFO.trim_start_matches('/')),
            pressure: root.join(PRESSURE.trim_start_matches('/')),
            swaps: root.join(SWAPS.trim_start_matches('/')),
        }
    }
}

impl Default for ProcMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl HostMemory for ProcMemory {
    async fn meminfo(&self) -> Result<MemInfo, RuntimeError> {
        let text = tokio::fs::read_to_string(&self.meminfo)
            .await
            .map_err(|source| RuntimeError::Read {
                path: self.meminfo.clone(),
                source,
            })?;

        Ok(MemInfo::parse(&self.meminfo, &text)?)
    }

    async fn pressure(&self) -> Result<Option<Pressure>, RuntimeError> {
        let text = match tokio::fs::read_to_string(&self.pressure).await {
            Ok(text) => text,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(RuntimeError::Read {
                    path: self.pressure.clone(),
                    source,
                });
            }
        };

        Ok(Some(Pressure::parse(&self.pressure, &text)?))
    }

    async fn swaps(&self) -> Result<Vec<String>, RuntimeError> {
        let text = match tokio::fs::read_to_string(&self.swaps).await {
            Ok(text) => text,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(RuntimeError::Read {
                    path: self.swaps.clone(),
                    source,
                });
            }
        };

        Ok(text
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            // The header column is a word, not a path.
            .filter(|device| device.starts_with('/'))
            .map(str::to_owned)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use pazari_types::kernel::meminfo::KB;
    use tempfile::TempDir;

    use super::*;
    use crate::runtime::testing::{
        MEMINFO as MEMINFO_FIXTURE, MockMemory, PRESSURE as PRESSURE_FIXTURE,
        SWAPS as SWAPS_FIXTURE, meminfo,
    };

    fn fixture(dir: &TempDir, absolute: &str, text: &str) {
        let path = dir.path().join(absolute.trim_start_matches('/'));

        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn proc() -> (TempDir, ProcMemory) {
        let dir = TempDir::new().unwrap();

        fixture(&dir, MEMINFO, MEMINFO_FIXTURE);
        fixture(&dir, PRESSURE, PRESSURE_FIXTURE);
        fixture(&dir, SWAPS, SWAPS_FIXTURE);

        let memory = ProcMemory::under(dir.path());

        (dir, memory)
    }

    #[test]
    fn a_parse_failure_reaches_the_caller_naming_the_file() {
        let error = MemInfo::parse(Path::new(MEMINFO), "MemTotal:  1024 kB\nMemFree:  256 kB\n")
            .expect_err("no MemAvailable");

        assert_eq!(
            RuntimeError::from(error).to_string(),
            "/proc/meminfo carries no MemAvailable line"
        );
    }

    #[tokio::test]
    async fn the_adapter_reads_the_proc_tree_it_is_pointed_at() {
        let (_dir, memory) = proc();

        let info = memory.meminfo().await.unwrap();

        assert_eq!(info.available, 1_729_396 * KB);
        assert_eq!(info.total, 2_096_756 * KB);
        assert_eq!(info.swap_total, 1_048_572 * KB);
        assert_eq!(info.swap_free, 1_048_572 * KB);

        let pressure = memory.pressure().await.unwrap().expect("a pressure file");

        assert!((pressure.some10 - 12.5).abs() < f64::EPSILON);

        assert_eq!(
            memory.swaps().await.unwrap(),
            vec!["/dev/zram0".to_owned(), "/swap/swapfile".to_owned()]
        );
    }

    #[tokio::test]
    async fn a_kernel_without_pressure_accounting_says_nothing_about_it() {
        let dir = TempDir::new().unwrap();

        fixture(&dir, MEMINFO, MEMINFO_FIXTURE);

        let memory = ProcMemory::under(dir.path());

        assert_eq!(memory.pressure().await.unwrap(), None);
        assert_eq!(memory.swaps().await.unwrap(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_meminfo_that_is_not_there_is_an_error() {
        let dir = TempDir::new().unwrap();

        let memory = ProcMemory::under(dir.path());

        assert!(memory.meminfo().await.is_err());
        assert_eq!(ProcMemory::new(), ProcMemory::default());
    }

    #[tokio::test]
    async fn a_mock_stands_in_for_the_host() {
        let memory = MockMemory::new(meminfo(8 << 30, 8 << 30, 0));

        assert_eq!(memory.meminfo().await.unwrap().available, 8 << 30);
        assert_eq!(memory.pressure().await.unwrap(), None);
        assert_eq!(memory.swaps().await.unwrap(), Vec::<String>::new());

        memory.set(meminfo(8 << 30, 1 << 30, 0));

        assert_eq!(memory.meminfo().await.unwrap().available, 1 << 30);
    }
}
