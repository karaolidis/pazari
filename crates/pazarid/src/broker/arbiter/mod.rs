mod fcfs;

use std::{fmt, time::Duration};

use pazari_proto::DenyReason;
use pazari_types::Timestamp;

pub use crate::broker::arbiter::fcfs::Fcfs;
use crate::broker::{
    guest::Geometry,
    phase::Negotiated,
    policy::{Grantable, Policy, Revocation},
    registry::{GuestId, GuestSpec},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    Grow {
        target: u64,
    },

    /// The guest has already freed the difference.
    Release {
        target: u64,
    },
}

impl Ask {
    #[must_use]
    pub const fn target(self) -> u64 {
        match self {
            Self::Grow { target } | Self::Release { target } => target,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GuestView<'a> {
    pub id: GuestId,

    pub spec: &'a GuestSpec,

    pub geometry: Geometry,

    pub requested: Option<u64>,

    /// Absent until the guest is connected and past the handshake.
    pub session: Option<&'a Negotiated>,

    pub granted: Option<Timestamp>,
}

impl GuestView<'_> {
    #[must_use]
    pub const fn outstanding(&self) -> u64 {
        match self.requested {
            Some(target) => target.saturating_sub(self.geometry.size),
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
    pub psi: Option<f64>,
}

impl HostView {
    #[must_use]
    pub fn grantable(&self, grantable: Grantable) -> Option<u64> {
        let available = self.available?;

        match grantable {
            Grantable::Ram => Some(available),
            Grantable::RamAndSwap => Some(available.saturating_add(self.swap_free?)),
        }
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
        let grow = Ask::Grow { target: 3072 * MIB };

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

        assert_eq!(host.grantable(Grantable::Ram), Some(4 * GIB));
        assert_eq!(HostView::default().grantable(Grantable::Ram), None);
    }

    #[test]
    fn a_host_that_grants_swap_counts_it_at_face_value() {
        let host = HostView {
            available: Some(4 * GIB),
            swap_free: Some(8 * GIB),
            psi: None,
        };

        assert_eq!(host.grantable(Grantable::RamAndSwap), Some(12 * GIB));
    }

    #[test]
    fn swap_the_host_has_not_read_leaves_nothing_to_grant_out_of_swap() {
        let host = HostView {
            available: Some(4 * GIB),
            swap_free: None,
            psi: None,
        };

        assert_eq!(host.grantable(Grantable::RamAndSwap), None);
        assert_eq!(host.grantable(Grantable::Ram), Some(4 * GIB));
    }
}
