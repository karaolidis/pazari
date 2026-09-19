use std::{fmt, path::Path};

use crate::runtime::error::RuntimeError;

/// A `memory.high` or `memory.max` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    Bytes(u64),

    /// The kernel's `max` sentinel.
    Max,
}

impl Limit {
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

/// The ten-second pressure averages in percent.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pressure {
    pub some10: f32,
    pub full10: f32,
}

pub fn events(path: &Path, text: &str) -> Result<MemoryEvents, RuntimeError> {
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

    Ok(MemoryEvents {
        high: high.ok_or_else(|| RuntimeError::Absent {
            path: path.to_path_buf(),
            key: "high",
        })?,
        max: max.ok_or_else(|| RuntimeError::Absent {
            path: path.to_path_buf(),
            key: "max",
        })?,
    })
}

pub fn pressure(path: &Path, text: &str) -> Result<Pressure, RuntimeError> {
    let mut some10 = None;
    let mut full10 = None;

    for line in text.lines() {
        let mut fields = line.split_whitespace();

        let slot = match fields.next() {
            Some("some") => &mut some10,
            Some("full") => &mut full10,
            _ => continue,
        };

        let Some(average) = fields.find_map(|field| field.strip_prefix("avg10=")) else {
            continue;
        };

        *slot = Some(percent(path, average)?);
    }

    Ok(Pressure {
        some10: some10.ok_or_else(|| RuntimeError::Absent {
            path: path.to_path_buf(),
            key: "some avg10",
        })?,
        full10: full10.ok_or_else(|| RuntimeError::Absent {
            path: path.to_path_buf(),
            key: "full avg10",
        })?,
    })
}

pub fn count(path: &Path, text: &str) -> Result<u64, RuntimeError> {
    number(path, text.trim(), "a decimal byte count")
}

pub fn limit(path: &Path, text: &str) -> Result<Limit, RuntimeError> {
    let text = text.trim();

    if text == "max" {
        return Ok(Limit::Max);
    }

    Ok(Limit::Bytes(number(
        path,
        text,
        "a decimal byte count or \"max\"",
    )?))
}

fn number(path: &Path, text: &str, expected: &'static str) -> Result<u64, RuntimeError> {
    text.parse().map_err(|_| RuntimeError::Malformed {
        path: path.to_path_buf(),
        value: text.to_owned(),
        expected,
    })
}

fn percent(path: &Path, text: &str) -> Result<f32, RuntimeError> {
    let value: f32 = text.parse().map_err(|_| RuntimeError::Malformed {
        path: path.to_path_buf(),
        value: text.to_owned(),
        expected: "a pressure percentage",
    })?;

    if !value.is_finite() || value < 0.0 {
        return Err(RuntimeError::Malformed {
            path: path.to_path_buf(),
            value: text.to_owned(),
            expected: "a pressure percentage",
        });
    }

    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::runtime::testing::{EVENTS, PRESSURE};

    fn path() -> PathBuf {
        PathBuf::from("/sys/fs/cgroup/workload/memory.events")
    }

    #[test]
    fn memory_events_yields_the_two_counters_that_matter() {
        assert_eq!(
            events(&path(), EVENTS).expect("a real memory.events"),
            MemoryEvents { high: 37, max: 2 }
        );
    }

    #[test]
    fn a_memory_events_without_its_counters_is_rejected() {
        let error = events(&path(), "low 0\noom 0\n").expect_err("no high line");

        assert_eq!(
            error.to_string(),
            "/sys/fs/cgroup/workload/memory.events carries no high line"
        );

        assert!(events(&path(), "low 0\nhigh 1\n").is_err());
        assert!(events(&path(), "high wat\nmax 0\n").is_err());
    }

    #[test]
    fn memory_pressure_yields_both_ten_second_averages() {
        let pressure = pressure(&path(), PRESSURE).expect("a real memory.pressure");

        assert!((pressure.some10 - 12.5).abs() < f32::EPSILON);
        assert!((pressure.full10 - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn a_memory_pressure_missing_a_line_is_rejected() {
        let error = pressure(&path(), "some avg10=1.00 avg60=0.00 avg300=0.00 total=1\n")
            .expect_err("no full line");

        assert_eq!(
            error.to_string(),
            "/sys/fs/cgroup/workload/memory.events carries no full avg10 line"
        );
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
        assert_eq!(limit(&path(), "max\n").expect("the sentinel"), Limit::Max);
        assert_eq!(
            limit(&path(), "2147483648\n").expect("a byte count"),
            Limit::Bytes(2_147_483_648)
        );

        let error = limit(&path(), "infinity").expect_err("not a limit");

        assert_eq!(
            error.to_string(),
            "/sys/fs/cgroup/workload/memory.events holds \"infinity\", which is not a decimal byte count or \"max\""
        );
    }

    #[test]
    fn a_limit_writes_the_way_the_kernel_reads_it() {
        assert_eq!(Limit::Bytes(2_147_483_648).to_string(), "2147483648");
        assert_eq!(Limit::Max.to_string(), "max");
        assert_eq!(Limit::Max.bytes(), None);
        assert_eq!(Limit::Max.or_infinity(), u64::MAX);
        assert_eq!(Limit::Bytes(42).or_infinity(), 42);
    }
}
