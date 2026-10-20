use std::{io, path::PathBuf};

use pazari_types::{Pressure, Swappiness};
use tokio::{fs::OpenOptions, io::AsyncWriteExt};

use crate::{
    engine::OomGroup,
    runtime::{
        cgroup::{
            Cgroup, MEMORY_CURRENT, MEMORY_EVENTS, MEMORY_HIGH, MEMORY_MAX, MEMORY_OOM_GROUP,
            MEMORY_PRESSURE, MEMORY_RECLAIM, MEMORY_SWAP_CURRENT, Reclaimed,
            parse::{self, Limit, MemoryEvents},
        },
        error::RuntimeError,
    },
};

/// Reads and writes a cgroup's `memory.*` files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCgroup {
    path: PathBuf,
}

impl RawCgroup {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[must_use]
    pub fn released(&self) -> String {
        format!("{}/{MEMORY_HIGH} = {}", self.path.display(), Limit::Max)
    }

    pub async fn read(&self, name: &str) -> Result<String, RuntimeError> {
        let path = self.path.join(name);

        tokio::fs::read_to_string(&path)
            .await
            .map_err(|source| RuntimeError::Read { path, source })
    }

    pub async fn write(&self, name: &str, value: &str) -> Result<(), RuntimeError> {
        let path = self.path.join(name);

        let written = async {
            let mut file = OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)
                .await?;

            file.write_all(value.as_bytes()).await?;
            file.flush().await
        };

        written.await.map_err(|source| RuntimeError::Write {
            path,
            value: value.to_owned(),
            source,
        })
    }

    async fn count(&self, name: &'static str) -> Result<u64, RuntimeError> {
        let text = self.read(name).await?;

        Ok(parse::count(&self.path.join(name), &text)?)
    }
}

impl Cgroup for RawCgroup {
    async fn set_high(&self, limit: Limit) -> Result<(), RuntimeError> {
        self.write(MEMORY_HIGH, &limit.to_string()).await
    }

    async fn set_max(&self, limit: Limit) -> Result<(), RuntimeError> {
        self.write(MEMORY_MAX, &limit.to_string()).await
    }

    async fn set_oom_group(&self, group: OomGroup) -> Result<(), RuntimeError> {
        let value = match group {
            OomGroup::Whole => "1",
            OomGroup::PerProcess => "0",
        };

        self.write(MEMORY_OOM_GROUP, value).await
    }

    async fn events(&self) -> Result<MemoryEvents, RuntimeError> {
        let text = self.read(MEMORY_EVENTS).await?;

        Ok(MemoryEvents::parse(&self.path.join(MEMORY_EVENTS), &text)?)
    }

    async fn current(&self) -> Result<u64, RuntimeError> {
        self.count(MEMORY_CURRENT).await
    }

    async fn swap_current(&self) -> Result<u64, RuntimeError> {
        self.count(MEMORY_SWAP_CURRENT).await
    }

    async fn pressure(&self) -> Result<Pressure, RuntimeError> {
        let text = self.read(MEMORY_PRESSURE).await?;

        Ok(Pressure::parse(&self.path.join(MEMORY_PRESSURE), &text)?)
    }

    async fn reclaim(
        &self,
        bytes: u64,
        swappiness: Option<Swappiness>,
    ) -> Result<Reclaimed, RuntimeError> {
        let request = swappiness.map_or_else(
            || bytes.to_string(),
            |swappiness| format!("{bytes} swappiness={swappiness}"),
        );

        match self.write(MEMORY_RECLAIM, &request).await {
            Ok(()) => Ok(Reclaimed::Whole),
            Err(RuntimeError::Write { source, .. })
                if source.kind() == io::ErrorKind::WouldBlock =>
            {
                Ok(Reclaimed::Partial)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::runtime::testing::{EVENTS, PRESSURE, cgroup_dir};

    fn raw() -> (TempDir, RawCgroup) {
        let dir = cgroup_dir();
        let cgroup = RawCgroup::new(dir.path());

        (dir, cgroup)
    }

    fn read(dir: &TempDir, name: &str) -> String {
        fs::read_to_string(dir.path().join(name)).expect("the fixture file is there")
    }

    #[tokio::test]
    async fn the_reads_parse_what_the_kernel_writes() {
        let (dir, cgroup) = raw();

        fs::write(dir.path().join(MEMORY_EVENTS), EVENTS).unwrap();
        fs::write(dir.path().join(MEMORY_PRESSURE), PRESSURE).unwrap();
        fs::write(dir.path().join(MEMORY_CURRENT), "1610612736\n").unwrap();
        fs::write(dir.path().join(MEMORY_SWAP_CURRENT), "0\n").unwrap();

        assert_eq!(
            cgroup.events().await.unwrap(),
            MemoryEvents { high: 37, max: 2 }
        );
        assert_eq!(cgroup.current().await.unwrap(), 1_610_612_736);
        assert_eq!(cgroup.swap_current().await.unwrap(), 0);

        let pressure = cgroup.pressure().await.unwrap();
        assert!((pressure.some10 - 12.5).abs() < f64::EPSILON);
        assert!((pressure.full10 - 0.25).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn a_missing_file_names_itself() {
        let dir = TempDir::new().unwrap();
        let cgroup = RawCgroup::new(dir.path());

        let error = cgroup.current().await.expect_err("nothing is there");

        assert!(
            error
                .to_string()
                .ends_with("memory.current: No such file or directory (os error 2)"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn the_limits_land_byte_exact() {
        let (dir, cgroup) = raw();

        cgroup.set_max(Limit::Bytes(2_147_483_648)).await.unwrap();
        cgroup.set_high(Limit::Bytes(1_879_048_192)).await.unwrap();
        cgroup.set_oom_group(OomGroup::Whole).await.unwrap();

        assert_eq!(read(&dir, MEMORY_MAX), "2147483648");
        assert_eq!(read(&dir, MEMORY_HIGH), "1879048192");
        assert_eq!(read(&dir, MEMORY_OOM_GROUP), "1");

        cgroup.set_high(Limit::Max).await.unwrap();
        cgroup.set_oom_group(OomGroup::PerProcess).await.unwrap();

        assert_eq!(read(&dir, MEMORY_HIGH), "max");
        assert_eq!(read(&dir, MEMORY_OOM_GROUP), "0");
    }

    #[tokio::test]
    async fn a_reclaim_carries_the_swappiness_the_kernel_expects() {
        let (dir, cgroup) = raw();

        assert_eq!(
            cgroup
                .reclaim(67_108_864, Some(Swappiness::NONE))
                .await
                .unwrap(),
            Reclaimed::Whole
        );
        assert_eq!(read(&dir, MEMORY_RECLAIM), "67108864 swappiness=0");

        cgroup.reclaim(1_048_576, None).await.unwrap();
        assert_eq!(read(&dir, MEMORY_RECLAIM), "1048576");

        let swappiness = Swappiness::try_new(60).expect("60 is a swappiness");
        cgroup.reclaim(4096, Some(swappiness)).await.unwrap();
        assert_eq!(read(&dir, MEMORY_RECLAIM), "4096 swappiness=60");
    }
}
