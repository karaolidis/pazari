use std::num::{NonZeroU32, NonZeroU64};

use pazari_types::{Interval, Psi};

/// The host-wide constants every decision is taken against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Policy {
    pub welcome_status_interval: Interval,

    pub host_floor: u64,

    pub grant_min_interval: Interval,

    pub reconcile_deadline: Interval,

    /// Whether a stalled shrink reverts to the guest's actual size.
    pub shrink_revert: bool,

    /// Missed `status` intervals before a guest is marked stale.
    pub stale_after: NonZeroU32,

    pub reclaim_deadline: Interval,

    /// Whether free host swap counts towards what a grant may be backed by.
    pub grant_swap: bool,

    /// How the host takes memory back under its own pressure.
    pub revocation: Option<Revocation>,
}

/// Asking guests for memory back once the host itself is under pressure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Revocation {
    /// The host memory pressure at which the host starts asking.
    pub psi: Psi,

    /// How much memory one such request asks a guest for.
    pub step: NonZeroU64,
}
