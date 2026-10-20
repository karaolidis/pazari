use std::{num::NonZeroU64, time::Duration};

const NANOS_PER_SECOND: u128 = 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pacer {
    rate: Option<NonZeroU64>,
    chunk: NonZeroU64,
}

impl Pacer {
    /// A phase that goes as fast as the faults allow.
    #[cfg(test)]
    const fn burst(chunk: NonZeroU64) -> Self {
        Self { rate: None, chunk }
    }

    /// A phase held to `rate` bytes a second.
    #[must_use]
    pub const fn at(rate: NonZeroU64, chunk: NonZeroU64) -> Self {
        Self {
            rate: Some(rate),
            chunk,
        }
    }

    /// A phase held to `rate`, or bursting if there is none.
    #[must_use]
    pub const fn new(rate: Option<NonZeroU64>, chunk: NonZeroU64) -> Self {
        Self { rate, chunk }
    }

    #[cfg(test)]
    const fn is_paced(&self) -> bool {
        self.rate.is_some()
    }

    /// The bytes the next chunk moves, given what the phase has left to move.
    #[must_use]
    pub fn step(&self, left: u64) -> u64 {
        self.chunk.get().min(left)
    }

    /// The earliest phase-elapsed time at which the byte after `issued` may go
    /// out.
    #[must_use]
    pub fn deadline(&self, issued: u64) -> Duration {
        let Some(rate) = self.rate else {
            return Duration::ZERO;
        };

        let nanos = u128::from(issued) * NANOS_PER_SECOND / u128::from(rate.get());

        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }

    /// The rate, in bytes a second, a phase actually held: what a report says
    /// it moved, against the target the rest of this type sets.
    #[must_use]
    pub fn measure(bytes: u64, span: Duration) -> u64 {
        let nanos = span.as_nanos();

        if nanos == 0 {
            return 0;
        }

        let rate = u128::from(bytes) * NANOS_PER_SECOND / nanos;

        u64::try_from(rate).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    fn bytes(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("a positive byte count")
    }

    fn deadlines(rate: u64, chunk: u64, chunks: u64) -> Vec<Duration> {
        let pacer = Pacer::at(bytes(rate), bytes(chunk));

        (0..chunks).map(|nth| pacer.deadline(nth * chunk)).collect()
    }

    #[test]
    fn a_bursting_phase_never_waits() {
        let pacer = Pacer::burst(bytes(MIB));

        assert!(!pacer.is_paced());
        assert_eq!(pacer.deadline(0), Duration::ZERO);
        assert_eq!(pacer.deadline(u64::MAX), Duration::ZERO);
    }

    #[test]
    fn the_first_chunk_of_a_paced_phase_goes_out_at_once() {
        assert_eq!(
            Pacer::at(bytes(MIB), bytes(MIB)).deadline(0),
            Duration::ZERO
        );
    }

    #[test]
    fn the_deadlines_are_one_chunk_of_the_rate_apart() {
        assert_eq!(
            deadlines(100 * MIB, MIB, 5),
            vec![
                Duration::ZERO,
                Duration::from_millis(10),
                Duration::from_millis(20),
                Duration::from_millis(30),
                Duration::from_millis(40),
            ]
        );
        assert_eq!(
            deadlines(500 * MIB, MIB, 3),
            vec![
                Duration::ZERO,
                Duration::from_millis(2),
                Duration::from_millis(4),
            ]
        );
    }

    #[test]
    fn a_deadline_is_taken_from_the_total_so_it_cannot_drift() {
        let chunk = 7 * (1 << 10);
        let pacer = Pacer::at(bytes(3 * MIB), bytes(chunk));

        for nth in 0..10_000_u64 {
            let issued = nth * chunk;

            assert_eq!(
                pacer.deadline(issued),
                Duration::from_nanos(issued * 1_000_000_000 / (3 * MIB)),
                "the {nth}th chunk drifted"
            );
        }
    }

    #[test]
    fn a_deadline_beyond_the_clock_saturates() {
        assert_eq!(
            Pacer::at(NonZeroU64::MIN, bytes(MIB)).deadline(u64::MAX),
            Duration::from_nanos(u64::MAX)
        );
    }

    #[test]
    fn a_chunk_is_never_more_than_what_is_left() {
        let pacer = Pacer::burst(bytes(MIB));

        assert_eq!(pacer.step(4 * MIB), MIB);
        assert_eq!(pacer.step(MIB), MIB);
        assert_eq!(pacer.step(3), 3);
        assert_eq!(pacer.step(0), 0);
    }

    #[test]
    fn a_rate_is_what_moved_over_how_long_it_took() {
        assert_eq!(Pacer::measure(MIB, Duration::from_secs(1)), MIB);
        assert_eq!(Pacer::measure(MIB, Duration::from_millis(500)), 2 * MIB);
        assert_eq!(Pacer::measure(0, Duration::from_secs(1)), 0);
        assert_eq!(Pacer::measure(MIB, Duration::ZERO), 0, "no time, no rate");
        assert_eq!(Pacer::measure(u64::MAX, Duration::from_nanos(1)), u64::MAX);
    }
}
