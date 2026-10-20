use std::num::{NonZeroU32, NonZeroU64};

use pazari_types::{Interval, Psi};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Policy {
    pub status_interval: Interval,

    pub stale_after: NonZeroU32,

    pub grant_floor: u64,

    pub grant_min_interval: Interval,

    pub grantable: Grantable,

    pub actuation_deadline: Interval,

    pub stalled_shrinks: StalledShrinks,

    pub reclaim_deadline: Interval,

    pub revocation: Option<Revocation>,
}

/// What counts as available memory when a grow is weighed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grantable {
    Ram,

    /// Free swap counts at face value.
    RamAndSwap,
}

/// What becomes of a shrink that misses its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StalledShrinks {
    /// Taken back to the size the guest still has.
    Revert,

    /// Left requested.
    Leave,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Revocation {
    pub psi: Psi,

    pub step: NonZeroU64,
}
