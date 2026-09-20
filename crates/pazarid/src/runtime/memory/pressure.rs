use std::path::Path;

use crate::runtime::error::RuntimeError;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pressure {
    /// Time in which some task was stalled on memory.
    pub some10: f32,

    /// Time in which every task was.
    pub full10: f32,
}

impl Pressure {
    pub fn parse(path: &Path, text: &str) -> Result<Self, RuntimeError> {
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

        Ok(Self {
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
    use super::*;
    use crate::runtime::{memory::PRESSURE, testing::PRESSURE as FIXTURE};

    fn path() -> &'static Path {
        Path::new(PRESSURE)
    }

    #[test]
    fn host_pressure_yields_both_ten_second_averages() {
        let pressure = Pressure::parse(path(), FIXTURE).expect("a real /proc/pressure/memory");

        assert!((pressure.some10 - 12.5).abs() < f32::EPSILON);
        assert!((pressure.full10 - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn a_pressure_file_missing_a_line_is_rejected() {
        let error = Pressure::parse(path(), "some avg10=1.00 avg60=0.00 avg300=0.00 total=1\n")
            .expect_err("no full line");

        assert_eq!(
            error.to_string(),
            "/proc/pressure/memory carries no full avg10 line"
        );

        assert!(Pressure::parse(path(), "").is_err());
    }

    #[test]
    fn an_average_that_is_not_a_percentage_is_rejected() {
        let error = Pressure::parse(path(), "some avg10=wat\nfull avg10=0.00\n")
            .expect_err("wat is not a percentage");

        assert_eq!(
            error.to_string(),
            "/proc/pressure/memory holds \"wat\", which is not a pressure percentage"
        );

        assert!(Pressure::parse(path(), "some avg10=-1.0\nfull avg10=0.00\n").is_err());
        assert!(Pressure::parse(path(), "some avg10=inf\nfull avg10=0.00\n").is_err());
    }
}
