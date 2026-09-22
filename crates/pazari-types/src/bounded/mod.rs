use std::fmt;

use crate::error::RangeError;

/// A newtype over a raw scalar that only admits part of its range.
pub trait Bounded: Copy + Sized {
    type Raw: Copy + fmt::Debug;

    const NAME: &'static str;

    const BOUNDS: &'static str;

    fn contains(value: Self::Raw) -> bool;

    fn wrap(value: Self::Raw) -> Self;

    fn get(self) -> Self::Raw;

    fn try_new(value: Self::Raw) -> Result<Self, RangeError> {
        if !Self::contains(value) {
            return Err(RangeError::new(Self::NAME, &value, Self::BOUNDS));
        }

        Ok(Self::wrap(value))
    }
}

/// Declares a [`Bounded`] newtype.
#[macro_export]
macro_rules! bounded {
    (
        $(#[$attr:meta])*
        $vis:vis struct $name:ident($raw:ty);

        const MAX: $max:ty = $ceiling:expr;
        const BOUNDS: &str = $bounds:literal;

        fn contains($value:ident) -> bool $contains:block
        fn display($shown:ident, $formatter:ident) -> fmt::Result $display:block
    ) => {
        $(#[$attr])*
        $vis struct $name($raw);

        impl $name {
            pub const MAX: $max = $ceiling;

            pub fn try_new(value: $raw) -> ::core::result::Result<Self, $crate::RangeError> {
                <Self as $crate::Bounded>::try_new(value)
            }

            #[must_use]
            pub const fn get(self) -> $raw {
                self.0
            }
        }

        impl $crate::Bounded for $name {
            type Raw = $raw;

            const NAME: &'static str = ::core::stringify!($name);
            const BOUNDS: &'static str = $bounds;

            fn contains($value: $raw) -> bool $contains

            fn wrap(value: $raw) -> Self {
                Self(value)
            }

            fn get(self) -> $raw {
                self.0
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, $formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                let $shown = self.0;
                $display
            }
        }

        impl ::core::convert::TryFrom<$raw> for $name {
            type Error = $crate::RangeError;

            fn try_from(value: $raw) -> ::core::result::Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl ::core::convert::From<$name> for $raw {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use std::{fmt, time::Duration};

    use serde::{Serialize, de::DeserializeOwned};

    use crate::{Bounded, Delay, Fraction, Interval, Psi, Swappiness};

    fn round_trips<T>(raw: T::Raw)
    where
        T: Bounded + Serialize + DeserializeOwned + PartialEq + fmt::Debug,
    {
        let value = T::try_new(raw).expect("the fixture is inside the range");

        let text = serde_json::to_string(&value).expect("a valid value serialises");
        let back: T = serde_json::from_str(&text).expect("its own output deserialises");

        assert_eq!(back, value, "{} lost {raw:?} in a round trip", T::NAME);
    }

    fn rejects<T>(raw: T::Raw)
    where
        T: Bounded + fmt::Debug,
    {
        let error = T::try_new(raw).expect_err("the fixture is outside the range");

        assert_eq!(error.what, T::NAME);
        assert_eq!(error.bounds, T::BOUNDS);
        assert_eq!(error.value, format!("{raw:?}"));
    }

    #[test]
    fn every_bounded_type_survives_its_own_serialisation() {
        round_trips::<Interval>(Duration::from_millis(1_500));
        round_trips::<Delay>(Duration::from_secs(30));
        round_trips::<Fraction>(0.5);
        round_trips::<Psi>(5.5);
        round_trips::<Swappiness>(60);
    }

    #[test]
    fn every_rejection_names_its_type_and_bounds() {
        rejects::<Interval>(Duration::ZERO);
        rejects::<Delay>(Duration::from_secs(86_401));
        rejects::<Fraction>(1.5);
        rejects::<Psi>(101.0);
        rejects::<Swappiness>(201);
    }

    #[test]
    fn the_range_agrees_with_what_it_admits() {
        assert!(Interval::contains(Duration::from_secs(1)));
        assert!(!Interval::contains(Duration::ZERO));
        assert_eq!(
            Interval::wrap(Duration::from_secs(1)).get(),
            Duration::from_secs(1)
        );
    }
}
