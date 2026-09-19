use std::num::NonZeroU64;

use crate::engine::{action::Action, config::EngineConfig};

/// Where `memory.high` sits relative to `memory.max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// `high == max - guard`.
    Open,
    /// `high == max`.
    Collapsed,
    /// `high == max - R`.
    Steady,
    /// Wherever titration left it.
    Hold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    service_reserve: u64,
    burst_reserve: NonZeroU64,
    open_guard: NonZeroU64,

    plugged: u64,
    ceiling: Option<u64>,

    high: u64,
    max: u64,
    band: Band,
}

impl Limits {
    pub const fn new(config: &EngineConfig, plugged: u64) -> Self {
        let max = plugged.saturating_sub(config.service_reserve);

        Self {
            service_reserve: config.service_reserve,
            burst_reserve: config.burst_reserve,
            open_guard: config.open_guard,
            plugged,
            ceiling: None,
            high: max,
            max,
            band: Band::Hold,
        }
    }

    pub const fn plugged(self) -> u64 {
        self.plugged
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

    pub const fn band(self) -> Band {
        self.band
    }

    pub const fn is_open(self) -> bool {
        matches!(self.band, Band::Open | Band::Collapsed)
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
        self.plugged.saturating_sub(self.service_reserve)
    }

    pub const fn steady_high(self) -> u64 {
        self.budget().saturating_sub(self.burst_reserve.get())
    }

    pub const fn set_plugged(&mut self, plugged: u64) -> bool {
        let changed = plugged != self.plugged;
        self.plugged = plugged;
        changed
    }

    pub fn raise_ceiling(&mut self, ceiling: u64) -> bool {
        let raised = self.ceiling.is_none_or(|current| ceiling > current);
        self.ceiling = Some(ceiling);
        raised
    }

    pub fn apply(&mut self, band: Band, out: &mut Vec<Action>) {
        self.band = band;

        let max = self.budget();

        let high = match band {
            Band::Open => max.saturating_sub(self.open_guard.get()),
            Band::Collapsed => max,
            Band::Steady => max.saturating_sub(self.burst_reserve.get()),
            Band::Hold => self.high.min(max),
        };

        if max >= self.max {
            self.set_max(max, out);
            self.set_high(high, out);
        } else {
            self.set_high(high, out);
            self.set_max(max, out);
        }
    }

    pub fn reapply(&mut self, out: &mut Vec<Action>) {
        self.apply(self.band, out);
    }

    pub fn set_high(&mut self, value: u64, out: &mut Vec<Action>) {
        if value != self.high {
            self.high = value;
            out.push(Action::SetHigh(value));
        }
    }

    fn set_max(&mut self, value: u64, out: &mut Vec<Action>) {
        if value != self.max {
            self.max = value;
            out.push(Action::SetMax(value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 256;
    const R: u64 = 128;
    const G: u64 = 32;

    fn limits(plugged: u64, high: u64, max: u64) -> Limits {
        Limits {
            service_reserve: S,
            burst_reserve: NonZeroU64::new(R).unwrap(),
            open_guard: NonZeroU64::new(G).unwrap(),
            plugged,
            ceiling: None,
            high,
            max,
            band: Band::Steady,
        }
    }

    fn applied(mut limits: Limits, band: Band) -> Vec<Action> {
        let mut out = Vec::new();
        limits.apply(band, &mut out);
        out
    }

    #[test]
    fn a_wider_plug_raises_max_before_high() {
        let grown = limits(4096, 1664, 1792);

        assert_eq!(
            applied(grown, Band::Steady),
            vec![Action::SetMax(3840), Action::SetHigh(3712)]
        );
    }

    #[test]
    fn a_narrower_plug_lowers_high_before_max() {
        let shrunk = limits(1024, 1664, 1792);

        assert_eq!(
            applied(shrunk, Band::Steady),
            vec![Action::SetHigh(640), Action::SetMax(768)]
        );
    }

    #[test]
    fn an_open_band_holds_back_the_guard() {
        assert_eq!(
            applied(limits(2048, 1664, 1792), Band::Open),
            vec![Action::SetHigh(1792 - G)]
        );
    }

    #[test]
    fn a_collapsed_band_puts_high_on_max() {
        assert_eq!(
            applied(limits(2048, 1664, 1792), Band::Collapsed),
            vec![Action::SetHigh(1792)]
        );
    }

    #[test]
    fn a_held_band_keeps_high_where_titration_left_it() {
        assert_eq!(applied(limits(2048, 1000, 1792), Band::Hold), vec![]);

        assert_eq!(
            applied(limits(1024, 1000, 1792), Band::Hold),
            vec![Action::SetHigh(768), Action::SetMax(768)]
        );
    }

    #[test]
    fn limits_already_in_place_emit_nothing() {
        assert_eq!(applied(limits(2048, 1664, 1792), Band::Steady), vec![]);
    }

    #[test]
    fn the_band_is_remembered_for_the_next_plug() {
        let mut limits = limits(2048, 1664, 1792);
        let mut out = Vec::new();

        limits.apply(Band::Open, &mut out);
        assert_eq!(limits.band(), Band::Open);

        out.clear();
        limits.set_plugged(4096);
        limits.reapply(&mut out);

        assert_eq!(out, vec![Action::SetMax(3840), Action::SetHigh(3840 - G)]);
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
