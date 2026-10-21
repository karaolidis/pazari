use pazari_transport::PeerId;

use crate::broker::phase::{Negotiated, Phase};

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub(crate) peer: PeerId,
    pub(crate) phase: Phase,
}

impl Session {
    #[must_use]
    pub const fn new(peer: PeerId) -> Self {
        Self {
            peer,
            phase: Phase::start(),
        }
    }

    #[must_use]
    pub const fn negotiated(&self) -> Option<&Negotiated> {
        self.phase.negotiated()
    }

    pub const fn negotiated_mut(&mut self) -> Option<&mut Negotiated> {
        self.phase.negotiated_mut()
    }
}
