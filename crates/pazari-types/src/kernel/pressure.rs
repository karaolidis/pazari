use std::path::Path;

use crate::error::KernelFileError;

const PERCENTAGE: &str = "a pressure percentage";

/// The ten-second pressure-stall averages of one resource, in percent.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pressure {
    /// Time in which some task was stalled on the resource.
    pub some10: f32,

    /// Time in which every task was.
    pub full10: f32,
}

impl Pressure {
    pub fn parse(path: &Path, text: &str) -> Result<Self, KernelFileError> {
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
            some10: some10.ok_or_else(|| KernelFileError::Absent {
                path: path.to_path_buf(),
                key: "some avg10",
            })?,
            full10: full10.ok_or_else(|| KernelFileError::Absent {
                path: path.to_path_buf(),
                key: "full avg10",
            })?,
        })
    }
}

fn percent(path: &Path, text: &str) -> Result<f32, KernelFileError> {
    let value: f32 = text.parse().map_err(|_| KernelFileError::Malformed {
        path: path.to_path_buf(),
        value: text.to_owned(),
        expected: PERCENTAGE,
    })?;

    if !value.is_finite() || value < 0.0 {
        return Err(KernelFileError::Malformed {
            path: path.to_path_buf(),
            value: text.to_owned(),
            expected: PERCENTAGE,
        });
    }

    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "some avg10=12.50 avg60=3.00 avg300=1.00 total=123456\n\
                        full avg10=0.25 avg60=0.10 avg300=0.05 total=6543\n";

    const CGROUP: &str = "some avg10=12.50 avg60=3.75 avg300=0.42 total=1234567\n\
                          full avg10=0.25 avg60=0.00 avg300=0.00 total=42\n";

    fn path() -> &'static Path {
        Path::new("/proc/pressure/memory")
    }

    fn parsed(text: &str) -> Pressure {
        Pressure::parse(path(), text).expect("a real pressure file")
    }

    #[test]
    fn a_pressure_file_yields_both_ten_second_averages() {
        for text in [HOST, CGROUP] {
            let pressure = parsed(text);

            assert!((pressure.some10 - 12.5).abs() < f32::EPSILON, "{text}");
            assert!((pressure.full10 - 0.25).abs() < f32::EPSILON, "{text}");
        }
    }

    #[test]
    fn a_quiet_resource_reads_as_no_pressure_rather_than_no_reading() {
        assert_eq!(
            parsed(
                "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n\
                    full avg10=0.00 avg60=0.00 avg300=0.00 total=0\n"
            ),
            Pressure::default()
        );
    }

    #[test]
    fn a_pressure_file_missing_a_line_is_rejected() {
        let error = Pressure::parse(path(), "some avg10=1.00 avg60=0.00 avg300=0.00 total=1\n")
            .expect_err("no full line");

        assert_eq!(
            error.to_string(),
            "/proc/pressure/memory carries no full avg10 line"
        );

        assert!(Pressure::parse(path(), "full avg10=1.00\n").is_err());
        assert!(Pressure::parse(path(), "").is_err());
    }

    #[test]
    fn a_line_without_the_ten_second_average_is_not_a_reading() {
        assert!(
            Pressure::parse(path(), "some avg60=1.00 total=1\nfull avg10=0.00\n").is_err(),
            "an avg10 was invented for a line that carried none",
        );
    }

    #[test]
    fn an_average_that_is_not_a_percentage_is_rejected() {
        let error = Pressure::parse(path(), "some avg10=wat\nfull avg10=0.00\n")
            .expect_err("wat is not a percentage");

        assert_eq!(
            error.to_string(),
            "/proc/pressure/memory holds \"wat\", which is not a pressure percentage"
        );

        for bad in ["-1.0", "inf", "-inf", "NaN"] {
            assert!(
                Pressure::parse(path(), &format!("some avg10={bad}\nfull avg10=0.00\n")).is_err(),
                "{bad} was taken for a percentage",
            );
        }
    }
}
