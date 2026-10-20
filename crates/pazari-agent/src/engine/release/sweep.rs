use std::num::NonZeroU64;

use pazari_types::{Deadline, Timestamp};

use crate::engine::{
    config::SweepConfig, cooldown::Cooldown, counters::Pressure, release::Release,
};

/// An unbroken run of spare memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Spare {
    since: Timestamp,
    bytes: NonZeroU64,
}

/// All the spare at once, once it has held for `hold`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sweep {
    config: SweepConfig,
    spare: Option<Spare>,
    cooldown: Cooldown,
}

impl Sweep {
    pub const fn new(config: SweepConfig) -> Self {
        Self {
            config,
            spare: None,
            cooldown: Cooldown::new(config.cooldown),
        }
    }

    fn held(&self, now: Timestamp) -> Option<NonZeroU64> {
        let spare = self.spare?;

        Deadline::after(spare.since, self.config.hold.get())
            .reached(now)
            .then_some(spare.bytes)
    }
}

impl Release for Sweep {
    fn observe(&mut self, now: Timestamp, slack: Option<u64>, pressure: Pressure) {
        let bytes = slack
            .filter(|&slack| pressure.calm() && slack >= self.config.min.get())
            .and_then(NonZeroU64::new);

        self.spare = bytes.map(|bytes| Spare {
            since: self.spare.map_or(now, |spare| spare.since),
            bytes,
        });

        if pressure.breached {
            self.cooldown.extend(now);
        }
    }

    fn due(&self, now: Timestamp) -> Option<NonZeroU64> {
        if self.cooldown.active(now) {
            return None;
        }

        self.held(now)
    }

    fn rested(&mut self, now: Timestamp) {
        self.spare = None;
        self.cooldown.restart(now);
    }

    fn recovered(&mut self) {
        self.spare = None;
        self.cooldown.lift();
    }
}

#[cfg(test)]
mod tests {
    use pazari_types::Delay;

    use super::*;
    use crate::engine::testing::{BREACHED, MIB, QUIET, THROTTLED, at, bytes, delay};

    fn sweep() -> Sweep {
        Sweep::new(SweepConfig {
            min: bytes(256 * MIB),
            hold: delay(300),
            cooldown: delay(30),
        })
    }

    fn spare(release: &mut Sweep, from: u64, to: u64, bytes: u64) {
        for second in from..=to {
            release.observe(at(second), Some(bytes), QUIET);
        }
    }

    #[test]
    fn a_release_waits_for_the_spare_to_hold_for_the_whole_hold() {
        let mut release = sweep();

        spare(&mut release, 0, 299, 4096 * MIB);
        assert_eq!(release.due(at(299)), None);

        spare(&mut release, 300, 300, 4096 * MIB);
        assert_eq!(release.due(at(300)), NonZeroU64::new(4096 * MIB));
    }

    #[test]
    fn a_release_returns_what_the_latest_interval_had_spare() {
        let mut release = sweep();

        spare(&mut release, 0, 150, 8192 * MIB);
        spare(&mut release, 151, 300, 1024 * MIB);

        assert_eq!(release.due(at(300)), NonZeroU64::new(1024 * MIB));
    }

    #[test]
    fn less_than_the_minimum_spare_starts_the_hold_over() {
        let mut release = sweep();

        spare(&mut release, 0, 200, 4096 * MIB);
        release.observe(at(201), Some(255 * MIB), QUIET);
        spare(&mut release, 202, 400, 4096 * MIB);

        assert_eq!(release.due(at(400)), None);
        assert_eq!(release.due(at(502)), NonZeroU64::new(4096 * MIB));
    }

    #[test]
    fn an_unknown_slack_starts_the_hold_over() {
        let mut release = sweep();

        spare(&mut release, 0, 200, 4096 * MIB);
        release.observe(at(201), None, QUIET);
        spare(&mut release, 202, 400, 4096 * MIB);

        assert_eq!(release.due(at(400)), None);
    }

    #[test]
    fn a_throttle_starts_the_hold_over() {
        let mut release = sweep();

        spare(&mut release, 0, 200, 4096 * MIB);
        release.observe(at(201), Some(4096 * MIB), THROTTLED);
        spare(&mut release, 202, 400, 4096 * MIB);

        assert_eq!(release.due(at(400)), None);
    }

    #[test]
    fn a_rest_starts_the_cooldown_and_the_hold_over() {
        let mut release = sweep();

        spare(&mut release, 0, 300, 4096 * MIB);
        release.rested(at(300));
        assert_eq!(release.due(at(300)), None);

        spare(&mut release, 301, 601, 4096 * MIB);
        assert_eq!(release.due(at(601)), NonZeroU64::new(4096 * MIB));
    }

    #[test]
    fn a_held_spare_still_waits_out_the_cooldown() {
        let mut release = Sweep::new(SweepConfig {
            hold: Delay::ZERO,
            ..sweep().config
        });

        release.rested(at(100));
        spare(&mut release, 101, 129, 4096 * MIB);

        assert_eq!(release.due(at(129)), None);
        assert_eq!(release.due(at(130)), NonZeroU64::new(4096 * MIB));
    }

    #[test]
    fn a_breach_pushes_the_cooldown_out() {
        let mut release = Sweep::new(SweepConfig {
            hold: Delay::ZERO,
            ..sweep().config
        });

        release.rested(at(100));
        spare(&mut release, 101, 119, 4096 * MIB);
        release.observe(at(120), Some(4096 * MIB), BREACHED);
        spare(&mut release, 121, 150, 4096 * MIB);

        assert_eq!(release.due(at(149)), None);
        assert_eq!(release.due(at(150)), NonZeroU64::new(4096 * MIB));
    }

    #[test]
    fn a_recovery_lifts_the_cooldown() {
        let mut release = Sweep::new(SweepConfig {
            hold: Delay::ZERO,
            ..sweep().config
        });

        release.rested(at(100));
        release.recovered();
        spare(&mut release, 101, 101, 4096 * MIB);

        assert_eq!(release.due(at(101)), NonZeroU64::new(4096 * MIB));
    }
}
