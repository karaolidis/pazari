use std::{fmt, path::Path};

use pazari_types::KernelFileError;

/// A `memory.high` or `memory.max` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    Bytes(u64),

    /// The kernel's `max` sentinel.
    Max,
}

impl Limit {
    pub fn parse(path: &Path, text: &str) -> Result<Self, KernelFileError> {
        let text = text.trim();

        if text == "max" {
            return Ok(Self::Max);
        }

        Ok(Self::Bytes(number(
            path,
            text,
            "a decimal byte count or \"max\"",
        )?))
    }

    #[must_use]
    pub const fn bytes(self) -> Option<u64> {
        match self {
            Self::Bytes(bytes) => Some(bytes),
            Self::Max => None,
        }
    }

    /// The same value the way systemd spells it in a unit property.
    #[must_use]
    pub const fn or_infinity(self) -> u64 {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::Max => u64::MAX,
        }
    }
}

impl fmt::Display for Limit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bytes(bytes) => write!(formatter, "{bytes}"),
            Self::Max => formatter.write_str("max"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemoryEvents {
    /// Times the workload was braked at `memory.high`.
    pub high: u64,

    /// Times it was held at `memory.max`.
    pub max: u64,
}

impl MemoryEvents {
    pub fn parse(path: &Path, text: &str) -> Result<Self, KernelFileError> {
        let mut high = None;
        let mut max = None;

        for line in text.lines() {
            let mut fields = line.split_whitespace();

            let (Some(key), Some(value)) = (fields.next(), fields.next()) else {
                continue;
            };

            let slot = match key {
                "high" => &mut high,
                "max" => &mut max,
                _ => continue,
            };

            *slot = Some(number(path, value, "a decimal counter")?);
        }

        Ok(Self {
            high: high.ok_or_else(|| KernelFileError::Absent {
                path: path.to_path_buf(),
                key: "high",
            })?,
            max: max.ok_or_else(|| KernelFileError::Absent {
                path: path.to_path_buf(),
                key: "max",
            })?,
        })
    }
}

pub fn count(path: &Path, text: &str) -> Result<u64, KernelFileError> {
    number(path, text.trim(), "a decimal byte count")
}

fn number(path: &Path, text: &str, expected: &'static str) -> Result<u64, KernelFileError> {
    text.parse().map_err(|_| KernelFileError::Malformed {
        path: path.to_path_buf(),
        value: text.to_owned(),
        expected,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::runtime::testing::EVENTS;

    fn path() -> PathBuf {
        PathBuf::from("/sys/fs/cgroup/workload/memory.events")
    }

    #[test]
    fn memory_events_yields_the_two_counters_that_matter() {
        assert_eq!(
            MemoryEvents::parse(&path(), EVENTS).expect("a real memory.events"),
            MemoryEvents { high: 37, max: 2 }
        );
    }

    #[test]
    fn a_memory_events_without_its_counters_is_rejected() {
        let error = MemoryEvents::parse(&path(), "low 0\noom 0\n").expect_err("no high line");

        assert_eq!(
            error.to_string(),
            "/sys/fs/cgroup/workload/memory.events carries no high line"
        );

        assert!(MemoryEvents::parse(&path(), "low 0\nhigh 1\n").is_err());
        assert!(MemoryEvents::parse(&path(), "high wat\nmax 0\n").is_err());
    }

    #[test]
    fn a_byte_count_tolerates_the_trailing_newline() {
        assert_eq!(
            count(&path(), "1610612736\n").expect("a byte count"),
            1_610_612_736
        );
        assert_eq!(count(&path(), "0").expect("a byte count"), 0);
        assert!(count(&path(), "max").is_err());
    }

    #[test]
    fn a_limit_reads_both_a_count_and_the_sentinel() {
        assert_eq!(
            Limit::parse(&path(), "max\n").expect("the sentinel"),
            Limit::Max
        );
        assert_eq!(
            Limit::parse(&path(), "2147483648\n").expect("a byte count"),
            Limit::Bytes(2_147_483_648)
        );

        let error = Limit::parse(&path(), "infinity").expect_err("not a limit");

        assert_eq!(
            error.to_string(),
            "/sys/fs/cgroup/workload/memory.events holds \"infinity\", which is not a decimal byte count or \"max\""
        );
    }
}
