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
