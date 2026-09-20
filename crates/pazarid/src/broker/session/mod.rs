use pazari_proto::Status;
use pazari_transport::PeerId;
use pazari_types::Timestamp;

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub peer: PeerId,
    pub phase: Phase,
}

impl Session {
    #[must_use]
    pub const fn new(peer: PeerId) -> Self {
        Self {
            peer,
            phase: Phase::AwaitingHello,
        }
    }

    #[must_use]
    pub const fn negotiated(&self) -> Option<&Negotiated> {
        match self.phase {
            Phase::AwaitingHello => None,
            Phase::Negotiated(ref negotiated) => Some(negotiated),
        }
    }

    pub const fn negotiated_mut(&mut self) -> Option<&mut Negotiated> {
        match self.phase {
            Phase::AwaitingHello => None,
            Phase::Negotiated(ref mut negotiated) => Some(negotiated),
        }
    }
}

/// How far along the handshake a connection is.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    AwaitingHello,
    Negotiated(Negotiated),
}

/// A connection the host has welcomed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Negotiated {
    /// What the guest said its geometry was in `hello`.
    pub boot_mem: u64,
    pub block_size: u64,

    /// The last guest-initiated id answered.
    pub answered: Option<u64>,

    /// The next host-initiated id.
    pub next_reclaim: u64,

    pub reclaim: Option<Reclaim>,

    pub status: Option<Status>,

    /// When that `status` or `welcome` arrived.
    pub heard: Timestamp,

    pub stale: bool,
}

impl Negotiated {
    #[must_use]
    pub const fn new(boot_mem: u64, block_size: u64, now: Timestamp) -> Self {
        Self {
            boot_mem,
            block_size,
            answered: None,
            next_reclaim: 1,
            reclaim: None,
            status: None,
            heard: now,
            stale: false,
        }
    }

    #[must_use]
    pub const fn accepts(&self, id: u64) -> bool {
        match self.answered {
            Some(answered) => id > answered,
            None => true,
        }
    }

    pub const fn accept(&mut self, id: u64) -> bool {
        if !self.accepts(id) {
            return false;
        }

        self.answered = Some(id);

        true
    }

    pub const fn take_reclaim_id(&mut self) -> u64 {
        let id = self.next_reclaim;
        self.next_reclaim = self.next_reclaim.saturating_add(1);
        id
    }
}

/// A `reclaim_request` the host asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reclaim {
    pub id: u64,

    pub desired: u64,

    pub deadline: Timestamp,

    /// What the guest committed to free, once it has acked.
    pub committed: Option<u64>,

    pub refused: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::testing::{BLOCK, BOOT, at, vsock};

    fn negotiated() -> Negotiated {
        Negotiated::new(BOOT, BLOCK, at(0))
    }

    #[test]
    fn a_fresh_connection_says_nothing_until_it_is_welcomed() {
        let mut session = Session::new(vsock(3));

        assert_eq!(session.phase, Phase::AwaitingHello);
        assert_eq!(session.negotiated(), None);
        assert_eq!(session.negotiated_mut(), None);

        session.phase = Phase::Negotiated(negotiated());

        assert!(session.negotiated().is_some());
    }

    #[test]
    fn guest_ids_must_climb() {
        let mut session = negotiated();

        assert!(session.accepts(0));
        assert!(session.accepts(9));

        assert!(session.accept(4));
        assert_eq!(session.answered, Some(4));

        assert!(!session.accepts(3));
        assert!(!session.accept(4));
        assert!(session.accepts(5));
        assert_eq!(session.answered, Some(4));

        assert!(session.accept(5));
        assert_eq!(session.answered, Some(5));
    }

    #[test]
    fn host_ids_start_at_one_and_climb() {
        let mut session = negotiated();

        assert_eq!(session.take_reclaim_id(), 1);
        assert_eq!(session.take_reclaim_id(), 2);
        assert_eq!(session.take_reclaim_id(), 3);
    }
}
