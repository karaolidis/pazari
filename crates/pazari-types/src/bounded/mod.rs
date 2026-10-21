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
