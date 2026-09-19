use std::path::{Path, PathBuf};

use thiserror::Error;

/// Bytes in the kilobyte `/proc/meminfo` counts in.
pub const KB: u64 = 1024;

/// The `/proc/meminfo` fields pazari reads, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemInfo {
    /// `MemTotal`.
    pub total: u64,

    /// `MemAvailable`.
    pub available: u64,

    /// `SwapTotal`.
    pub swap_total: u64,
}

impl MemInfo {
    pub fn parse(path: &Path, text: &str) -> Result<Self, MemInfoError> {
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
                .ok_or_else(|| MemInfoError::Malformed {
                    path: path.to_path_buf(),
                    value: line.trim().to_owned(),
                    expected: "a count of kB",
                })?;

            *slot = Some(kilobytes.saturating_mul(KB));
        }

        Ok(Self {
            total: total.ok_or_else(|| MemInfoError::Absent {
                path: path.to_path_buf(),
                key: "MemTotal",
            })?,
            available: available.ok_or_else(|| MemInfoError::Absent {
                path: path.to_path_buf(),
                key: "MemAvailable",
            })?,
            swap_total: swap_total.unwrap_or(0),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MemInfoError {
    #[error("{} holds {value:?}, which is not {expected}", .path.display())]
    Malformed {
        path: PathBuf,
        value: String,
        expected: &'static str,
    },

    #[error("{} carries no {key} line", .path.display())]
    Absent { path: PathBuf, key: &'static str },
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMINFO: &str = "MemTotal:        2096756 kB\n\
                           MemFree:          131072 kB\n\
                           MemAvailable:    1729396 kB\n\
                           Buffers:           16384 kB\n\
                           Cached:          1600000 kB\n\
                           SwapCached:            0 kB\n\
                           SwapTotal:       1048572 kB\n\
                           SwapFree:        1048572 kB\n\
                           HugePages_Total:       0\n";

    fn path() -> &'static Path {
        Path::new("/proc/meminfo")
    }

    #[test]
    fn meminfo_reads_the_three_fields_in_bytes() {
        assert_eq!(
            MemInfo::parse(path(), MEMINFO).expect("a real /proc/meminfo"),
            MemInfo {
                total: 2_096_756 * KB,
                available: 1_729_396 * KB,
                swap_total: 1_048_572 * KB,
            }
        );
    }

    #[test]
    fn a_guest_without_swap_has_no_swap_total_line() {
        let info = MemInfo::parse(path(), "MemTotal:       1024 kB\nMemAvailable:    512 kB\n")
            .expect("swap is optional");

        assert_eq!(info.swap_total, 0);
        assert_eq!(info.total, 1024 * KB);
    }

    #[test]
    fn a_meminfo_without_the_fields_pazari_needs_is_rejected() {
        let error = MemInfo::parse(path(), "MemFree:  1 kB\n").expect_err("no MemTotal");

        assert_eq!(error.to_string(), "/proc/meminfo carries no MemTotal line");

        let error = MemInfo::parse(path(), "MemTotal:  1024 kB\n").expect_err("no MemAvailable");

        assert_eq!(
            error.to_string(),
            "/proc/meminfo carries no MemAvailable line"
        );

        assert!(MemInfo::parse(path(), "").is_err());
    }

    #[test]
    fn a_count_that_is_not_a_number_is_rejected() {
        let error = MemInfo::parse(path(), "MemTotal:  wat kB\nMemAvailable: 1 kB\n")
            .expect_err("wat is not a count");

        assert_eq!(
            error.to_string(),
            "/proc/meminfo holds \"MemTotal:  wat kB\", which is not a count of kB"
        );
    }
}
