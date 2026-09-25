use pazari_transport::PeerId;
use pazari_types::Timestamp;
use tracing::{debug, info, warn};

use crate::broker::{Broker, action::Action, effects::Effects, phase::Phase, registry::GuestId};

impl Broker {
    pub(in crate::broker) fn on_connected(&mut self, peer: PeerId, fx: &mut Effects) {
        let Some(guest) = self.world.guests().lookup(&peer) else {
            warn!(%peer, "connection from an unknown peer");
            fx.emit(Action::Close { peer });

            return;
        };

        if self.world.open_session(guest, peer.clone()) {
            info!(
                guest = self.world.name(guest),
                %peer,
                "guest reconnected while the previous connection was still open",
            );
        } else {
            debug!(guest = self.world.name(guest), %peer, "guest connected");
        }
    }

    pub(in crate::broker) fn on_size(
        &mut self,
        now: Timestamp,
        guest: GuestId,
        size: u64,
        hotplugged: u64,
        fx: &mut Effects,
    ) {
        self.world.settle(guest, size, hotplugged);

        let Some(session) = self.world.guests().session(guest) else {
            return;
        };

        let peer = session.peer.clone();
        let phase = session.phase;

        let next = self.transition(now, guest, &peer, fx, |step| match phase {
            Phase::AwaitingHello(awaiting) => awaiting.welcome(step),
            Phase::Negotiated(negotiated) => {
                step.world.tell_geometry(step.guest, step.peer, step.fx);

                Phase::Negotiated(negotiated)
            }
        });

        self.settle_phase(guest, next);
    }

    pub(in crate::broker) fn on_disconnected(&mut self, peer: &PeerId) {
        let Some(guest) = self.world.guests().lookup(peer) else {
            return;
        };

        if self.world.drop_session(guest) {
            debug!(guest = self.world.name(guest), %peer, "guest disconnected");
        }
    }
}
