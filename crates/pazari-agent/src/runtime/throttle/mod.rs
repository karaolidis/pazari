use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use tokio::io::{Interest, unix::AsyncFd};

use crate::{
    engine::Event,
    runtime::{
        cgroup::{MEMORY_EVENTS, parse},
        clock::Clock,
        error::RuntimeError,
    },
};

/// Wakes the agent the moment the workload is braked.
#[derive(Debug)]
pub struct ThrottleWatcher {
    events: AsyncFd<File>,
    path: PathBuf,
    clock: Clock,
}

impl ThrottleWatcher {
    /// Opens the cgroup's `memory.events` and arms it for `EPOLLPRI`.
    pub fn open(cgroup: &Path, clock: Clock) -> Result<Self, RuntimeError> {
        let path = cgroup.join(MEMORY_EVENTS);

        let file = File::open(&path).map_err(|source| RuntimeError::Read {
            path: path.clone(),
            source,
        })?;

        let events = AsyncFd::with_interest(file, Interest::PRIORITY).map_err(|source| {
            RuntimeError::Watch {
                path: path.clone(),
                source,
            }
        })?;

        Ok(Self {
            events,
            path,
            clock,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn wake(&mut self) -> Result<Event, RuntimeError> {
        let mut ready = self
            .events
            .ready_mut(Interest::PRIORITY)
            .await
            .map_err(|source| RuntimeError::Watch {
                path: self.path.clone(),
                source,
            })?;

        ready.clear_ready();

        let events = reread(ready.get_inner_mut(), &self.path)?;

        Ok(Event::ThrottleWake {
            now: self.clock.now(),
            high_events: events.high,
            max_events: events.max,
        })
    }
}

fn reread(file: &mut File, path: &Path) -> Result<parse::MemoryEvents, RuntimeError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| RuntimeError::Read {
            path: path.to_path_buf(),
            source,
        })?;

    let mut text = String::new();

    file.read_to_string(&mut text)
        .map_err(|source| RuntimeError::Read {
            path: path.to_path_buf(),
            source,
        })?;

    parse::events(path, &text)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::runtime::{cgroup::parse::MemoryEvents, testing::EVENTS};

    #[test]
    fn a_reread_sees_the_counters_that_moved_since_the_last_one() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(MEMORY_EVENTS);

        fs::write(&path, EVENTS).unwrap();

        let mut file = File::open(&path).unwrap();

        assert_eq!(
            reread(&mut file, &path).unwrap(),
            MemoryEvents { high: 37, max: 2 }
        );

        fs::write(&path, "low 0\nhigh 38\nmax 2\noom 0\n").unwrap();

        assert_eq!(
            reread(&mut file, &path).unwrap(),
            MemoryEvents { high: 38, max: 2 }
        );
    }

    #[test]
    fn a_reread_shrinks_with_the_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(MEMORY_EVENTS);

        fs::write(&path, EVENTS).unwrap();

        let mut file = File::open(&path).unwrap();
        reread(&mut file, &path).unwrap();

        fs::write(&path, "high 1\nmax 0\n").unwrap();

        assert_eq!(
            reread(&mut file, &path).unwrap(),
            MemoryEvents { high: 1, max: 0 }
        );
    }

    #[test]
    fn a_cgroup_without_memory_events_cannot_be_watched() {
        let dir = TempDir::new().unwrap();

        let error =
            ThrottleWatcher::open(dir.path(), Clock::start()).expect_err("nothing to watch");

        assert!(
            error.to_string().contains("memory.events"),
            "unexpected error: {error}"
        );
    }
}
