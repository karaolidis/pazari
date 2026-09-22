mod fcfs;

use std::{fmt, time::Duration};

use pazari_proto::{DenyReason, GrantReason, Status, Urgency};
use pazari_types::Timestamp;

pub use crate::broker::arbiter::fcfs::Fcfs;
use crate::broker::{
    policy::{Policy, Revocation},
    registry::{GuestId, GuestSpec},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    Grow {
        target: u64,
        reason: GrantReason,
        urgency: Urgency,
    },

    /// The guest has already freed the difference.
    Release { target: u64 },
}

impl Ask {
    #[must_use]
    pub const fn target(self) -> u64 {
        match self {
            Self::Grow { target, .. } | Self::Release { target } => target,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GuestView<'a> {
    pub id: GuestId,

    pub spec: &'a GuestSpec,

    pub boot_mem: Option<u64>,

    pub size: u64,

    pub requested: Option<u64>,

    pub status: Option<Status>,

    pub stale: bool,

    /// Connected and past the handshake.
    pub connected: bool,

    pub granted: Option<Timestamp>,
}

impl GuestView<'_> {
    #[must_use]
    pub const fn outstanding(&self) -> u64 {
        match self.requested {
            Some(target) => target.saturating_sub(self.size),
            None => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HostView {
    /// `MemAvailable`.
    pub available: Option<u64>,

    /// `SwapFree`.
    pub swap_free: Option<u64>,

    /// The `some avg10` of `/proc/pressure/memory`, in percent.
    pub psi: Option<f32>,
}

impl HostView {
    #[must_use]
    pub fn grantable(&self, grant_swap: bool) -> Option<u64> {
        let available = self.available?;

        if !grant_swap {
            return Some(available);
        }

        Some(available.saturating_add(self.swap_free?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Request<'a> {
    pub now: Timestamp,

    pub guest: GuestView<'a>,

    pub ask: Ask,

    pub others: &'a [GuestView<'a>],

    pub host: HostView,

    pub policy: &'a Policy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// `target` may be less than was asked for.
    Grant { target: u64 },

    Deny {
        reason: DenyReason,
        retry_after: Option<Duration>,
    },
}

/// The replaceable half of the broker.
pub trait Arbiter: fmt::Debug + Send {
    fn decide(&mut self, request: &Request<'_>) -> Decision;

    fn revoke(
        &mut self,
        candidates: &[GuestView<'_>],
        revocation: Revocation,
    ) -> Option<(GuestId, u64)>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::testing::{GIB, MIB, guest, view};

    #[test]
    fn an_ask_names_the_size_it_is_after() {
        let grow = Ask::Grow {
            target: 3072 * MIB,
            reason: GrantReason::Throttle,
            urgency: Urgency::High,
        };

        assert_eq!(grow.target(), 3072 * MIB);
        assert_eq!(Ask::Release { target: 1024 * MIB }.target(), 1024 * MIB);
    }

    #[test]
    fn only_memory_on_its_way_out_of_the_host_is_outstanding() {
        let spec = guest("one", 4096 * MIB);

        let idle = view(&spec, 2048 * MIB, None);
        let growing = view(&spec, 2048 * MIB, Some(3072 * MIB));
        let shrinking = view(&spec, 3072 * MIB, Some(2048 * MIB));

        assert_eq!(idle.outstanding(), 0);
        assert_eq!(growing.outstanding(), 1024 * MIB);
        assert_eq!(shrinking.outstanding(), 0);
    }

    #[test]
    fn a_host_that_grants_no_swap_gives_away_what_it_has_and_nothing_more() {
        let host = HostView {
            available: Some(4 * GIB),
            swap_free: Some(8 * GIB),
            psi: None,
        };

        assert_eq!(host.grantable(false), Some(4 * GIB));
        assert_eq!(HostView::default().grantable(false), None);
    }

    #[test]
    fn a_host_that_grants_swap_counts_it_at_face_value() {
        let host = HostView {
            available: Some(4 * GIB),
            swap_free: Some(8 * GIB),
            psi: None,
        };

        assert_eq!(host.grantable(true), Some(12 * GIB));
    }

    #[test]
    fn a_host_granting_swap_it_has_not_read_gives_nothing_away() {
        let host = HostView {
            available: Some(4 * GIB),
            swap_free: None,
            psi: None,
        };

        assert_eq!(host.grantable(true), None);
        assert_eq!(host.grantable(false), Some(4 * GIB));
    }
}
