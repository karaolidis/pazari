use serde::{Deserialize, Serialize};

crate::bounded::bounded! {
    /// A pressure-stall percentage, in `(0, 100]`.
    #[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
    #[serde(try_from = "f64", into = "f64")]
    pub struct Psi(f64);

    const MAX: f64 = 100.0;
    const BOUNDS: &str = "(0, 100]";

    fn contains(value) -> bool { value.is_finite() && value > 0.0 && value <= Self::MAX }
    fn display(shown, formatter) -> fmt::Result { write!(formatter, "{shown}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JUST_OVER_ONE_HUNDRED: f64 = f64::from_bits(100.0_f64.to_bits() + 1);

    #[test]
    fn a_psi_spans_zero_exclusive_to_one_hundred() {
        assert!(Psi::try_new(0.0).is_err());
        assert!(Psi::try_new(-0.0).is_err());
        assert!(Psi::try_new(-1.0).is_err());
        assert_eq!(
            Psi::try_new(f64::MIN_POSITIVE).map(Psi::get),
            Ok(f64::MIN_POSITIVE)
        );
        assert_eq!(Psi::try_new(100.0).map(Psi::get), Ok(100.0));
        assert!(Psi::try_new(JUST_OVER_ONE_HUNDRED).is_err());
    }

    #[test]
    fn a_psi_admits_no_non_finite() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(Psi::try_new(value).is_err(), "Psi took {value}");
        }
    }

    #[test]
    fn a_psi_displays_and_serialises_as_its_number() {
        let psi = Psi::try_new(5.0).expect("5% is a psi");

        assert_eq!(psi.to_string(), "5");
        assert_eq!(serde_json::to_string(&psi).expect("f64 serialises"), "5.0");
    }

    #[test]
    fn deserialisation_validates() {
        assert!(serde_json::from_str::<Psi>("100").is_ok());
        assert!(serde_json::from_str::<Psi>("101").is_err());
        assert!(serde_json::from_str::<Psi>("0").is_err());
    }
}
