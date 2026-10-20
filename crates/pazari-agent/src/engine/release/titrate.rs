use std::num::NonZeroU64;

use pazari_types::Timestamp;

use crate::engine::{
    config::TitrateConfig, cooldown::Cooldown, counters::Pressure, release::Release, streak::Streak,
};

/// One fixed step at a time. The guest must hold the step spare for a run of
/// calm intervals. The last resize must have cooled off.
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
        self.spare.reached(self.config.after)
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
    use super::*;
    use crate::engine::testing::{BREACHED, MIB, QUIET, THROTTLED, at, titrate as config};

    fn titrate() -> Titrate {
        Titrate::new(config())
    }

    fn spare(release: &mut Titrate, intervals: u32) {
        for _ in 0..intervals {
            release.observe(at(0), Some(256 * MIB), QUIET);
        }
    }

    #[test]
    fn a_release_needs_the_step_spare_for_every_interval() {
        let mut release = titrate();

        spare(&mut release, 2);
        assert_eq!(release.due(at(0), QUIET), None);

        spare(&mut release, 1);
        assert_eq!(release.due(at(0), QUIET), NonZeroU64::new(256 * MIB));
    }

    #[test]
    fn less_than_a_step_spare_is_not_spare() {
        let mut release = titrate();

        spare(&mut release, 2);
        release.observe(at(0), Some(255 * MIB), QUIET);

        assert!(!release.spared());
    }

    #[test]
    fn a_throttle_breaks_the_run() {
        let mut release = titrate();

        spare(&mut release, 2);
        release.observe(at(0), Some(256 * MIB), THROTTLED);
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
        assert_eq!(release.due(at(129), QUIET), None);
        assert_eq!(release.due(at(130), QUIET), NonZeroU64::new(256 * MIB));
    }

    #[test]
    fn a_breach_pushes_the_cooldown_out() {
        let mut release = titrate();

        release.rested(at(100));
        spare(&mut release, 3);

        assert_eq!(release.due(at(120), BREACHED), None);
        assert_eq!(release.due(at(149), QUIET), None);
        assert_eq!(release.due(at(150), QUIET), NonZeroU64::new(256 * MIB));
    }

    #[test]
    fn a_recovery_lifts_the_cooldown() {
        let mut release = titrate();

        release.rested(at(100));
        release.recovered();
        spare(&mut release, 3);

        assert_eq!(release.due(at(101), QUIET), NonZeroU64::new(256 * MIB));
    }
}
