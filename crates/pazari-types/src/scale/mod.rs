use std::num::NonZeroU64;

/// A multiplier that doubles up to a cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scale {
    factor: NonZeroU64,
    cap: NonZeroU64,
}

impl Scale {
    const TWO: NonZeroU64 = NonZeroU64::new(2).expect("two is not zero");

    #[must_use]
    pub const fn new(cap: NonZeroU64) -> Self {
        Self {
            factor: NonZeroU64::MIN,
            cap,
        }
    }

    #[must_use]
    pub const fn doubled(self) -> Self {
        let next = self.factor.saturating_mul(Self::TWO);

        Self {
            factor: if next.get() > self.cap.get() {
                self.cap
            } else {
                next
            },
            cap: self.cap,
        }
    }

    #[must_use]
    pub const fn reset(self) -> Self {
        Self::new(self.cap)
    }

    #[must_use]
    pub const fn factor(self) -> NonZeroU64 {
        self.factor
    }

    #[must_use]
    pub const fn of(self, value: NonZeroU64) -> NonZeroU64 {
        value.saturating_mul(self.factor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("a positive cap")
    }

    #[test]
    fn a_scale_doubles_to_its_cap_and_stops() {
        let mut scale = Scale::new(cap(64));

        for expected in [2, 4, 8, 16, 32, 64, 64] {
            scale = scale.doubled();
            assert_eq!(scale.factor().get(), expected);
        }
    }

    #[test]
    fn a_cap_between_powers_of_two_is_reached_exactly() {
        let scale = Scale::new(cap(3)).doubled().doubled();

        assert_eq!(scale.factor().get(), 3);
    }

    #[test]
    fn a_scale_multiplies_what_it_is_applied_to() {
        let step = cap(512);
        let scale = Scale::new(cap(64));

        assert_eq!(scale.of(step).get(), 512);
        assert_eq!(scale.doubled().of(step).get(), 1024);
    }

    #[test]
    fn a_reset_keeps_the_cap() {
        let scale = Scale::new(cap(4)).doubled().doubled().reset();

        assert_eq!(scale, Scale::new(cap(4)));
    }
}
