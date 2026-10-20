use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::{Path, PathBuf},
    process,
};

use thiserror::Error;
use tracing::warn;

/// The byte every write repeats.
const PATTERN: u8 = 0xa5;

/// A file that grows the page cache, removed on drop.
#[derive(Debug)]
pub struct Scratch {
    path: PathBuf,
    file: File,
    written: u64,
}

impl Scratch {
    /// Creates a new file under `dir`.
    pub fn create(dir: &Path) -> Result<Self, ScratchError> {
        let path = dir.join(format!("pazari-stress.{}.cache", process::id()));

        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|source| ScratchError::Create {
                path: path.clone(),
                source,
            })?;

        Ok(Self {
            path,
            file,
            written: 0,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The bytes written so far.
    #[must_use]
    pub const fn written(&self) -> u64 {
        self.written
    }

    /// Appends `bytes` and syncs them, leaving the page cache clean.
    pub fn fill(&mut self, bytes: u64) -> Result<(), ScratchError> {
        let wrote =
            io::copy(&mut io::repeat(PATTERN).take(bytes), &mut self.file).map_err(|source| {
                ScratchError::Write {
                    path: self.path.clone(),
                    offset: self.written,
                    source,
                }
            })?;

        self.written = self.written.saturating_add(wrote);

        self.file.sync_data().map_err(|source| ScratchError::Sync {
            path: self.path.clone(),
            source,
        })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path) {
            warn!(path = %self.path.display(), %error, "failed to remove the scratch file");
        }
    }
}

#[derive(Debug, Error)]
pub enum ScratchError {
    #[error("failed to create the scratch file {}: {source}", path.display())]
    Create { path: PathBuf, source: io::Error },

    #[error("failed to write the scratch file {} at offset {offset}: {source}", path.display())]
    Write {
        path: PathBuf,
        offset: u64,
        source: io::Error,
    },

    #[error("failed to sync the scratch file {}: {source}", path.display())]
    Sync { path: PathBuf, source: io::Error },
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    const MIB: u64 = 1 << 20;

    #[test]
    fn a_fill_appends_what_it_was_asked_for() {
        let dir = TempDir::new().unwrap();
        let mut scratch = Scratch::create(dir.path()).unwrap();

        scratch.fill(3 * MIB / 2).unwrap();
        scratch.fill(MIB).unwrap();

        assert_eq!(scratch.written(), 5 * MIB / 2);
        assert_eq!(
            fs::metadata(scratch.path()).unwrap().len(),
            5 * MIB / 2,
            "the file does not hold what was written"
        );
    }

    #[test]
    fn the_file_is_gone_once_the_scratch_is() {
        let dir = TempDir::new().unwrap();
        let scratch = Scratch::create(dir.path()).unwrap();
        let path = scratch.path().to_owned();

        assert!(path.exists());

        drop(scratch);

        assert!(!path.exists());
    }

    #[test]
    fn a_directory_that_is_not_there_names_the_file() {
        let dir = TempDir::new().unwrap();
        let absent = dir.path().join("absent");

        let error = Scratch::create(&absent).expect_err("there is no directory to write in");

        assert!(
            error.to_string().contains(&*absent.to_string_lossy()),
            "unexpected error: {error}"
        );
    }
}
