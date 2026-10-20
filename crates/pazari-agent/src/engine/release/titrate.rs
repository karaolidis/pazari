use std::num::NonZeroU64;

use pazari_types::Timestamp;

use crate::engine::{
    config::TitrateConfig, cooldown::Cooldown, counters::Pressure, release::Release, streak::Streak,
};

/// One fixed step at a time, once the guest has held it spare for a run of
/// calm intervals and the last resize has cooled off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Titrate {
    config: TitrateConfig,
    spare: Streak,
    cooldown: Cooldown,
}

impl Titrate {
    pub const fn new(config: TitrateConfig) -> Self {
        Self {
            config,
            spare: Streak::BROKEN,
            cooldown: Cooldown::new(config.cooldown),
        }
    }

    const fn spared(&self) -> bool {
        self.spare.reached(self.config.intervals)
    }
}

impl Release for Titrate {
    fn observe(&mut self, _now: Timestamp, slack: Option<u64>, pressure: Pressure) {
        let sparing = pressure.calm() && slack.is_some_and(|slack| slack >= self.config.step.get());

        self.spare = self.spare.then(sparing);
    }

    fn due(&mut self, now: Timestamp, pressure: Pressure) -> Option<NonZeroU64> {
        if pressure.breached {
            self.cooldown.extend(now);
        }

        (self.spared() && !self.cooldown.active(now)).then_some(self.config.step)
    }

    fn rested(&mut self, now: Timestamp) {
        self.spare = Streak::BROKEN;
        self.cooldown.restart(now);
    }

    fn recovered(&mut self) {
        self.spare = Streak::BROKEN;
        self.cooldown.lift();
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU32, time::Duration};

    use pazari_types::Delay;

    use super::*;

    const MIB: u64 = 1 << 20;

    const CALM: Pressure = Pressure {
        throttled: false,
        breached: false,
    };

    const BREACHED: Pressure = Pressure {
        throttled: false,
        breached: true,
    };

    fn at(secs: u64) -> Timestamp {
        Timestamp::from_millis(secs * 1_000)
    }

    fn titrate() -> Titrate {
        Titrate::new(TitrateConfig {
            step: NonZeroU64::new(256 * MIB).expect("a positive step"),
            intervals: NonZeroU32::new(3).expect("a positive count"),
            cooldown: Delay::try_new(Duration::from_secs(30)).expect("a short delay"),
        })
    }

    fn spare(release: &mut Titrate, intervals: u32) {
        for _ in 0..intervals {
            release.observe(at(0), Some(256 * MIB), CALM);
        }
    }

    #[test]
    fn a_release_needs_the_step_spare_for_every_interval() {
        let mut release = titrate();

        spare(&mut release, 2);
        assert_eq!(release.due(at(0), CALM), None);

        spare(&mut release, 1);
        assert_eq!(release.due(at(0), CALM), NonZeroU64::new(256 * MIB));
    }

    #[test]
    fn less_than_a_step_spare_is_not_spare() {
        let mut release = titrate();

        spare(&mut release, 2);
        release.observe(at(0), Some(255 * MIB), CALM);

        assert!(!release.spared());
    }

    #[test]
    fn a_throttle_breaks_the_run() {
        let mut release = titrate();

        spare(&mut release, 2);
        release.observe(
            at(0),
            Some(256 * MIB),
            Pressure {
                throttled: true,
                breached: false,
            },
        );
        spare(&mut release, 2);

        assert!(!release.spared());
    }

    #[test]
    fn a_rest_starts_the_cooldown_and_the_run_over() {
        let mut release = titrate();

        spare(&mut release, 3);
        release.rested(at(100));
        assert!(!release.spared());

        spare(&mut release, 3);
        assert_eq!(release.due(at(129), CALM), None);
        assert_eq!(release.due(at(130), CALM), NonZeroU64::new(256 * MIB));
    }

    #[test]
    fn a_breach_pushes_the_cooldown_out() {
        let mut release = titrate();

        release.rested(at(100));
        spare(&mut release, 3);

        assert_eq!(release.due(at(120), BREACHED), None);
        assert_eq!(release.due(at(149), CALM), None);
        assert_eq!(release.due(at(150), CALM), NonZeroU64::new(256 * MIB));
    }

    #[test]
    fn a_recovery_lifts_the_cooldown() {
        let mut release = titrate();

        release.rested(at(100));
        release.recovered();
        spare(&mut release, 3);

        assert_eq!(release.due(at(101), CALM), NonZeroU64::new(256 * MIB));
    }
}
