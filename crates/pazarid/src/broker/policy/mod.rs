use std::num::{NonZeroU32, NonZeroU64};

use pazari_types::{Interval, Psi};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Policy {
    pub welcome_status_interval: Interval,

    pub host_floor: u64,

    pub grant_min_interval: Interval,

    pub reconcile_deadline: Interval,

    pub shrink_revert: bool,

    pub stale_after: NonZeroU32,

    pub reclaim_deadline: Interval,

    pub grant_swap: bool,

    pub revocation: Option<Revocation>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Revocation {
    pub psi: Psi,

    pub step: NonZeroU64,
}
