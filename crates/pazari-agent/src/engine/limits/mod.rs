use std::num::NonZeroU64;

use crate::engine::{action::Action, config::EngineConfig, effects::Effects};

/// Where `memory.high` sits under `memory.max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brake {
    Engaged,
    Released,
}

/// Which way `MemTotal` went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moved {
    Grew,
    Shrank,
    Unchanged,
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
        matches!(self.brake, Brake::Released)
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

    pub const fn set_total_mem(&mut self, total_mem: u64) -> Moved {
        let moved = if total_mem > self.total_mem {
            Moved::Grew
        } else if total_mem < self.total_mem {
            Moved::Shrank
        } else {
            Moved::Unchanged
        };

        self.total_mem = total_mem;

        moved
    }

    /// Adopts the host's ceiling. A first ceiling counts as a raise.
    pub const fn set_ceiling(&mut self, ceiling: u64) -> Moved {
        let moved = match self.ceiling {
            Some(current) if ceiling < current => Moved::Shrank,
            Some(current) if ceiling == current => Moved::Unchanged,
            _ => Moved::Grew,
        };

        self.ceiling = Some(ceiling);

        moved
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

    fn set_high(&mut self, value: u64, fx: &mut Effects) {
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
    use crate::engine::testing::{BURST, MIB, SERVICE, bytes};

    fn limits(total_mem: u64, high: u64, max: u64) -> Limits {
        Limits {
            service_reserve: SERVICE,
            burst_reserve: bytes(BURST),
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
        fx.drain().actions
    }

    #[test]
    fn a_wider_plug_raises_max_before_high() {
        let grown = limits(4096 * MIB, 1536 * MIB, 1792 * MIB);

        assert_eq!(
            applied(grown, Brake::Engaged),
            vec![Action::SetMax(3840 * MIB), Action::SetHigh(3584 * MIB)]
        );
    }

    #[test]
    fn a_narrower_plug_lowers_high_before_max() {
        let shrunk = limits(1024 * MIB, 1536 * MIB, 1792 * MIB);

        assert_eq!(
            applied(shrunk, Brake::Engaged),
            vec![Action::SetHigh(512 * MIB), Action::SetMax(768 * MIB)]
        );
    }

    #[test]
    fn a_released_brake_puts_high_on_max() {
        assert_eq!(
            applied(limits(2048 * MIB, 1536 * MIB, 1792 * MIB), Brake::Released),
            vec![Action::SetHigh(1792 * MIB)]
        );
    }

    #[test]
    fn limits_already_in_place_emit_nothing() {
        assert_eq!(
            applied(limits(2048 * MIB, 1536 * MIB, 1792 * MIB), Brake::Engaged),
            vec![]
        );
    }

    #[test]
    fn the_brake_is_remembered_for_the_next_plug() {
        let mut limits = limits(2048 * MIB, 1536 * MIB, 1792 * MIB);
        let mut fx = Effects::default();

        limits.apply(Brake::Released, &mut fx);
        assert!(limits.released());

        fx.drain();
        limits.set_total_mem(4096 * MIB);
        limits.apply(limits.brake(), &mut fx);

        assert_eq!(
            fx.drain().actions,
            vec![Action::SetMax(3840 * MIB), Action::SetHigh(3840 * MIB)]
        );
    }

    #[test]
    fn an_engaged_brake_on_an_empty_budget_is_still_engaged() {
        let mut limits = limits(SERVICE, 0, 0);
        let mut fx = Effects::default();

        limits.apply(Brake::Engaged, &mut fx);

        assert_eq!(limits.high(), limits.max());
        assert!(!limits.released());
    }

    #[test]
    fn a_new_total_says_which_way_it_moved() {
        let mut limits = limits(2048 * MIB, 1536 * MIB, 1792 * MIB);

        assert_eq!(limits.set_total_mem(2048 * MIB), Moved::Unchanged);
        assert_eq!(limits.set_total_mem(2560 * MIB), Moved::Grew);
        assert_eq!(limits.set_total_mem(2304 * MIB), Moved::Shrank);
    }

    #[test]
    fn a_new_ceiling_says_which_way_it_moved() {
        let mut limits = limits(2048 * MIB, 1536 * MIB, 1792 * MIB);

        assert_eq!(limits.set_ceiling(4096 * MIB), Moved::Grew);
        assert_eq!(limits.set_ceiling(4096 * MIB), Moved::Unchanged);
        assert_eq!(limits.set_ceiling(2048 * MIB), Moved::Shrank);
        assert_eq!(limits.set_ceiling(8192 * MIB), Moved::Grew);
        assert_eq!(limits.ceiling(), Some(8192 * MIB));
    }
}
