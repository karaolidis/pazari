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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::{
        phase::AwaitingHello,
        testing::{BLOCK, at, bytes, vsock},
    };

    #[test]
    fn a_fresh_connection_says_nothing_until_it_is_welcomed() {
        let mut session = Session::new(vsock(3));

        assert_eq!(session.phase, Phase::AwaitingHello(AwaitingHello::new()));
        assert_eq!(session.negotiated(), None);
        assert_eq!(session.negotiated_mut(), None);

        session.phase = Phase::Negotiated(Negotiated::new(bytes(BLOCK), at(0)));

        assert!(session.negotiated().is_some());
    }
}
