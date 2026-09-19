use std::{future::Future, path::PathBuf};

use pazari_types::MemInfo;

use crate::runtime::error::RuntimeError;

pub const MEMINFO: &str = "/proc/meminfo";

pub trait HostMemory: Send + Sync {
    fn available(&self) -> impl Future<Output = Result<u64, RuntimeError>> + Send;
}

/// `MemAvailable` out of `/proc/meminfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcMemory {
    path: PathBuf,
}

impl ProcMemory {
    #[must_use]
    pub fn new() -> Self {
        Self::at(MEMINFO)
    }

    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Default for ProcMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl HostMemory for ProcMemory {
    async fn available(&self) -> Result<u64, RuntimeError> {
        let text = tokio::fs::read_to_string(&self.path)
            .await
            .map_err(|source| RuntimeError::Read {
                path: self.path.clone(),
                source,
            })?;

        Ok(MemInfo::parse(&self.path, &text)?.available)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use std::path::Path;

    use pazari_types::meminfo::KB;

    use super::*;
    use crate::runtime::testing::{MEMINFO as FIXTURE, MockMemory};

    #[test]
    fn a_parse_failure_reaches_the_caller_naming_the_file() {
        let error = MemInfo::parse(Path::new(MEMINFO), "MemTotal:  1024 kB\n")
            .expect_err("no MemAvailable");

        assert_eq!(
            RuntimeError::from(error).to_string(),
            "/proc/meminfo carries no MemAvailable line"
        );
    }

    #[tokio::test]
    async fn the_adapter_reads_the_meminfo_it_is_pointed_at() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("meminfo");

        fs::write(&path, FIXTURE).unwrap();

        let memory = ProcMemory::at(&path);

        assert_eq!(memory.available().await.unwrap(), 1_729_396 * KB);
    }

    #[tokio::test]
    async fn a_meminfo_that_is_not_there_is_an_error() {
        let dir = TempDir::new().unwrap();

        let memory = ProcMemory::at(dir.path().join("meminfo"));

        assert!(memory.available().await.is_err());
        assert_eq!(ProcMemory::new(), ProcMemory::default());
    }

    #[tokio::test]
    async fn a_mock_stands_in_for_the_host() {
        let memory = MockMemory::new(8 << 30);

        assert_eq!(memory.available().await.unwrap(), 8 << 30);

        memory.set(1 << 30);

        assert_eq!(memory.available().await.unwrap(), 1 << 30);
    }
}
