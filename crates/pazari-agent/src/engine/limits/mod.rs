use std::num::NonZeroU64;

use crate::engine::{action::Action, config::EngineConfig, effects::Effects};

/// Where `memory.high` sits under `memory.max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brake {
    Engaged,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    service_reserve: u64,
    burst_reserve: NonZeroU64,

    total_mem: u64,
    ceiling: Option<u64>,

    high: u64,
    max: u64,
    brake: Brake,
}

impl Limits {
    pub const fn new(config: &EngineConfig, total_mem: u64) -> Self {
        let max = total_mem.saturating_sub(config.service_reserve);

        Self {
            service_reserve: config.service_reserve,
            burst_reserve: config.burst_reserve,
            total_mem,
            ceiling: None,
            high: max.saturating_sub(config.burst_reserve.get()),
            max,
            brake: Brake::Engaged,
        }
    }

    pub const fn total_mem(self) -> u64 {
        self.total_mem
    }

    pub const fn ceiling(self) -> Option<u64> {
        self.ceiling
    }

    pub const fn high(self) -> u64 {
        self.high
    }

    pub const fn max(self) -> u64 {
        self.max
    }

    pub const fn brake(self) -> Brake {
        self.brake
    }

    pub const fn released(self) -> bool {
        self.high >= self.max
    }

    pub const fn service_reserve(self) -> u64 {
        self.service_reserve
    }

    pub const fn burst_reserve(self) -> u64 {
        self.burst_reserve.get()
    }

    pub const fn reserves(self) -> u64 {
        self.service_reserve
            .saturating_add(self.burst_reserve.get())
    }

    pub const fn budget(self) -> u64 {
        self.total_mem.saturating_sub(self.service_reserve)
    }

    pub const fn engaged_high(self) -> u64 {
        self.budget().saturating_sub(self.burst_reserve.get())
    }

    pub const fn set_total_mem(&mut self, total_mem: u64) -> bool {
        let changed = total_mem != self.total_mem;
        self.total_mem = total_mem;
        changed
    }

    pub fn raise_ceiling(&mut self, ceiling: u64) -> bool {
        let raised = self.ceiling.is_none_or(|current| ceiling > current);
        self.ceiling = Some(ceiling);
        raised
    }

    pub fn apply(&mut self, brake: Brake, fx: &mut Effects) {
        self.brake = brake;

        let max = self.budget();

        let high = match brake {
            Brake::Engaged => self.engaged_high(),
            Brake::Released => max,
        };

        if max >= self.max {
            self.set_max(max, fx);
            self.set_high(high, fx);
        } else {
            self.set_high(high, fx);
            self.set_max(max, fx);
        }
    }

    pub fn reapply(&mut self, fx: &mut Effects) {
        self.apply(self.brake, fx);
    }

    pub fn set_high(&mut self, value: u64, fx: &mut Effects) {
        if value != self.high {
            self.high = value;
            fx.emit(Action::SetHigh(value));
        }
    }

    fn set_max(&mut self, value: u64, fx: &mut Effects) {
        if value != self.max {
            self.max = value;
            fx.emit(Action::SetMax(value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 256;
    const R: u64 = 128;

    fn limits(total_mem: u64, high: u64, max: u64) -> Limits {
        Limits {
            service_reserve: S,
            burst_reserve: NonZeroU64::new(R).unwrap(),
            total_mem,
            ceiling: None,
            high,
            max,
            brake: Brake::Engaged,
        }
    }

    fn applied(mut limits: Limits, brake: Brake) -> Vec<Action> {
        let mut fx = Effects::default();
        limits.apply(brake, &mut fx);
        fx.drain()
    }

    #[test]
    fn a_wider_plug_raises_max_before_high() {
        let grown = limits(4096, 1664, 1792);

        assert_eq!(
            applied(grown, Brake::Engaged),
            vec![Action::SetMax(3840), Action::SetHigh(3712)]
        );
    }

    #[test]
    fn a_narrower_plug_lowers_high_before_max() {
        let shrunk = limits(1024, 1664, 1792);

        assert_eq!(
            applied(shrunk, Brake::Engaged),
            vec![Action::SetHigh(640), Action::SetMax(768)]
        );
    }

    #[test]
    fn a_released_brake_puts_high_on_max() {
        assert_eq!(
            applied(limits(2048, 1664, 1792), Brake::Released),
            vec![Action::SetHigh(1792)]
        );
    }

    #[test]
    fn limits_already_in_place_emit_nothing() {
        assert_eq!(applied(limits(2048, 1664, 1792), Brake::Engaged), vec![]);
    }

    #[test]
    fn the_brake_is_remembered_for_the_next_plug() {
        let mut limits = limits(2048, 1664, 1792);
        let mut fx = Effects::default();

        limits.apply(Brake::Released, &mut fx);
        assert_eq!(limits.brake(), Brake::Released);

        fx.drain();
        limits.set_total_mem(4096);
        limits.reapply(&mut fx);

        assert_eq!(
            fx.drain(),
            vec![Action::SetMax(3840), Action::SetHigh(3840)]
        );
    }

    #[test]
    fn only_a_higher_ceiling_counts_as_raised() {
        let mut limits = limits(2048, 1664, 1792);

        assert!(limits.raise_ceiling(4096));
        assert!(!limits.raise_ceiling(4096));
        assert!(!limits.raise_ceiling(2048));
        assert!(limits.raise_ceiling(8192));
        assert_eq!(limits.ceiling(), Some(8192));
    }
}
