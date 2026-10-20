/// Declares a newtype over a raw scalar that only admits part of its range.
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

            pub(crate) const NAME: &'static str = ::core::stringify!($name);
            pub(crate) const BOUNDS: &'static str = $bounds;

            pub fn try_new($value: $raw) -> ::core::result::Result<Self, $crate::RangeError> {
                if !$contains {
                    return Err($crate::RangeError::new(Self::NAME, &$value, Self::BOUNDS));
                }

                Ok(Self($value))
            }

            #[must_use]
            pub const fn get(self) -> $raw {
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

pub(crate) use bounded;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::{Delay, Interval, Psi, Swappiness};

    macro_rules! round_trips {
        ($type:ty, $raw:expr) => {{
            let value = <$type>::try_new($raw).expect("the fixture is inside the range");

            let text = serde_json::to_string(&value).expect("a valid value serialises");
            let back: $type = serde_json::from_str(&text).expect("its own output deserialises");

            assert_eq!(
                back,
                value,
                "{} lost {:?} in a round trip",
                <$type>::NAME,
                $raw
            );
        }};
    }

    macro_rules! rejects {
        ($type:ty, $raw:expr) => {{
            let error = <$type>::try_new($raw).expect_err("the fixture is outside the range");

            assert_eq!(error.what, <$type>::NAME);
            assert_eq!(error.bounds, <$type>::BOUNDS);
            assert_eq!(error.value, format!("{:?}", $raw));
        }};
    }

    #[test]
    fn every_bounded_type_survives_its_own_serialisation() {
        round_trips!(Interval, Duration::from_millis(1_500));
        round_trips!(Delay, Duration::from_secs(30));
        round_trips!(Psi, 5.5);
        round_trips!(Swappiness, 60);
    }

    #[test]
    fn every_rejection_names_its_type_and_bounds() {
        rejects!(Interval, Duration::ZERO);
        rejects!(Delay, Duration::from_secs(86_401));
        rejects!(Psi, 101.0);
        rejects!(Swappiness, 201);
    }
}
