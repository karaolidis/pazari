use std::num::NonZeroU32;

use pazari_types::Interval;

/// The host-wide constants every decision is taken against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}
