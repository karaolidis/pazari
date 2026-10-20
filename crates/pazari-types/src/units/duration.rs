use std::time::Duration;

use serde::{Deserialize, Serialize};

const MAX_DURATION: Duration = Duration::from_hours(24);

crate::bounded::bounded! {
    /// A recurring, strictly positive period.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(try_from = "Duration", into = "Duration")]
    pub struct Interval(Duration);

    const MAX: Duration = MAX_DURATION;
    const BOUNDS: &str = "(0s, 24h]";

    fn contains(value) -> bool { !value.is_zero() && value <= Self::MAX }
    fn display(shown, formatter) -> fmt::Result { write!(formatter, "{shown:?}") }
}

crate::bounded::bounded! {
    /// A one-shot wait.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(try_from = "Duration", into = "Duration")]
    pub struct Delay(Duration);

    const MAX: Duration = MAX_DURATION;
    const BOUNDS: &str = "[0s, 24h]";

    fn contains(value) -> bool { value <= Self::MAX }
    fn display(shown, formatter) -> fmt::Result { write!(formatter, "{shown:?}") }
}

impl Delay {
    pub const ZERO: Self = Self(Duration::ZERO);
}

#[cfg(test)]
mod tests {
    use super::*;

    const NANOSECOND: Duration = Duration::from_nanos(1);

    #[test]
    fn an_interval_spans_the_open_day() {
        assert!(Interval::try_new(Duration::ZERO).is_err());
        assert_eq!(
            Interval::try_new(NANOSECOND).map(Interval::get),
            Ok(NANOSECOND)
        );
        assert_eq!(
            Interval::try_new(MAX_DURATION).map(Interval::get),
            Ok(MAX_DURATION)
        );
        assert!(Interval::try_new(MAX_DURATION + NANOSECOND).is_err());
    }

    #[test]
    fn a_delay_spans_the_closed_day() {
        assert_eq!(
            Delay::try_new(Duration::ZERO).map(Delay::get),
            Ok(Duration::ZERO)
        );
        assert_eq!(Delay::ZERO.get(), Duration::ZERO);
        assert_eq!(
            Delay::try_new(MAX_DURATION).map(Delay::get),
            Ok(MAX_DURATION)
        );
        assert!(Delay::try_new(MAX_DURATION + NANOSECOND).is_err());
    }

    #[test]
    fn a_duration_displays_as_a_duration() {
        assert_eq!(
            Interval::try_new(Duration::from_secs(5))
                .expect("five seconds is an interval")
                .to_string(),
            "5s"
        );
        assert_eq!(Delay::ZERO.to_string(), "0ns");
    }

    #[test]
    fn deserialisation_validates() {
        assert!(serde_json::from_str::<Interval>(r#"{"secs":0,"nanos":0}"#).is_err());
        assert!(serde_json::from_str::<Delay>(r#"{"secs":0,"nanos":0}"#).is_ok());
        assert!(serde_json::from_str::<Delay>(r#"{"secs":86401,"nanos":0}"#).is_err());
    }
}
