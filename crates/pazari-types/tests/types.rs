use std::time::Duration;

use pazari_types::{Delay, Fraction, Interval, MAX_DURATION, Psi, RangeError, Swappiness};

const NANOSECOND: Duration = Duration::from_nanos(1);

const JUST_OVER_ONE: f64 = 1.0 + f64::EPSILON;

const JUST_OVER_ONE_HUNDRED: f64 = f64::from_bits(100.0_f64.to_bits() + 1);

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
fn a_fraction_spans_zero_exclusive_to_one() {
    assert!(Fraction::try_new(0.0).is_err());
    assert!(Fraction::try_new(-0.0).is_err());
    assert!(Fraction::try_new(-0.5).is_err());
    assert_eq!(
        Fraction::try_new(f64::MIN_POSITIVE).map(Fraction::get),
        Ok(f64::MIN_POSITIVE)
    );
    assert_eq!(Fraction::try_new(1.0).map(Fraction::get), Ok(1.0));
    assert!(Fraction::try_new(JUST_OVER_ONE).is_err());
}

#[test]
fn a_psi_spans_zero_exclusive_to_one_hundred() {
    assert!(Psi::try_new(0.0).is_err());
    assert!(Psi::try_new(-1.0).is_err());
    assert_eq!(
        Psi::try_new(f64::MIN_POSITIVE).map(Psi::get),
        Ok(f64::MIN_POSITIVE)
    );
    assert_eq!(Psi::try_new(100.0).map(Psi::get), Ok(100.0));
    assert!(Psi::try_new(JUST_OVER_ONE_HUNDRED).is_err());
}

#[test]
fn no_ratio_admits_a_non_finite() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(Fraction::try_new(value).is_err(), "Fraction took {value}");
        assert!(Psi::try_new(value).is_err(), "Psi took {value}");
    }
}

#[test]
fn swappiness_spans_what_the_kernel_accepts() {
    assert_eq!(Swappiness::try_new(0).map(Swappiness::get), Ok(0));
    assert_eq!(Swappiness::NONE.get(), 0);
    assert_eq!(Swappiness::try_new(200).map(Swappiness::get), Ok(200));
    assert!(Swappiness::try_new(201).is_err());
    assert!(Swappiness::try_new(u8::MAX).is_err());
}

#[test]
fn a_rejection_names_what_was_rejected() {
    let error = Swappiness::try_new(255).expect_err("255 is over the kernel's limit");

    assert_eq!(error.what, "Swappiness");
    assert_eq!(error.value, "255");
    assert_eq!(error.bounds, "[0, 200]");
    assert_eq!(
        error.to_string(),
        "Swappiness rejects 255: expected [0, 200]"
    );
}

#[test]
fn every_type_displays_its_value() {
    assert_eq!(
        Interval::try_new(Duration::from_secs(5))
            .expect("five seconds is an interval")
            .to_string(),
        "5s"
    );
    assert_eq!(Delay::ZERO.to_string(), "0ns");
    assert_eq!(
        Fraction::try_new(0.25)
            .expect("a quarter is a fraction")
            .to_string(),
        "0.25"
    );
    assert_eq!(Psi::try_new(5.0).expect("5% is a psi").to_string(), "5");
    assert_eq!(Swappiness::NONE.to_string(), "0");
}

#[test]
fn deserialisation_validates() {
    assert!(serde_json::from_str::<Interval>(r#"{"secs":0,"nanos":0}"#).is_err());
    assert!(serde_json::from_str::<Delay>(r#"{"secs":0,"nanos":0}"#).is_ok());
    assert!(serde_json::from_str::<Delay>(r#"{"secs":86401,"nanos":0}"#).is_err());
    assert!(serde_json::from_str::<Fraction>("0").is_err());
    assert!(serde_json::from_str::<Fraction>("1.5").is_err());
    assert!(serde_json::from_str::<Psi>("100").is_ok());
    assert!(serde_json::from_str::<Psi>("101").is_err());
    assert!(serde_json::from_str::<Swappiness>("200").is_ok());
    assert!(serde_json::from_str::<Swappiness>("201").is_err());
}

fn round_trip<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let text = serde_json::to_string(value).expect("a valid value serialises");
    serde_json::from_str(&text).expect("its own output deserialises")
}

#[test]
fn serialisation_round_trips() {
    let interval = Interval::try_new(Duration::from_millis(1500)).expect("1.5s is an interval");
    let delay = Delay::try_new(Duration::from_secs(30)).expect("30s is a delay");
    let fraction = Fraction::try_new(0.5).expect("a half is a fraction");
    let psi = Psi::try_new(5.5).expect("5.5% is a psi");
    let swappiness = Swappiness::try_new(60).expect("60 is a swappiness");

    assert_eq!(round_trip(&interval), interval);
    assert_eq!(round_trip(&delay), delay);
    assert_eq!(round_trip(&fraction), fraction);
    assert_eq!(round_trip(&psi), psi);
    assert_eq!(round_trip(&swappiness), swappiness);

    assert_eq!(
        serde_json::to_string(&swappiness).expect("u8 serialises"),
        "60"
    );
    assert_eq!(
        serde_json::to_string(&fraction).expect("f64 serialises"),
        "0.5"
    );
}

#[test]
fn a_range_error_is_comparable() {
    let first = Fraction::try_new(2.0).expect_err("2.0 is over one");
    let second = Fraction::try_new(2.0).expect_err("2.0 is over one");

    assert_eq!(first, second);
    assert_eq!(
        first,
        RangeError {
            what: "Fraction",
            value: "2.0".to_owned(),
            bounds: "(0, 1]",
        }
    );
}
