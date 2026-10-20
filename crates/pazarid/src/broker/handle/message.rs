use pazari_proto::GuestMessage;
use pazari_transport::PeerId;
use pazari_types::Timestamp;
use tracing::warn;

use crate::broker::{Broker, effects::Effects, phase::State};

impl Broker {
    pub(in crate::broker) fn on_message(
        &mut self,
        now: Timestamp,
        peer: &PeerId,
        message: GuestMessage,
        fx: &mut Effects,
    ) {
        let Some(guest) = self.world.guests().lookup(peer) else {
            warn!(%peer, "message from an unknown peer");
            fx.close(peer);

            return;
        };

        let Some(phase) = self
            .world
            .guests()
            .session(guest)
            .map(|session| session.phase)
        else {
            warn!(guest = self.world.name(guest), %peer, "message on a connection the broker never accepted");
            fx.close(peer);

            return;
        };

        let next = self.transition(now, guest, peer, fx, |step| match message {
            GuestMessage::Hello {
                protocol,
                agent_version,
                block_size,
                total_mem,
            } => phase.on_hello(step, protocol, &agent_version, block_size, total_mem),

            GuestMessage::GrantRequest { id, target, .. } => {
                phase.on_grant_request(step, id, target)
            }

            GuestMessage::OfferRelease { id, releasable, .. } => {
                phase.on_offer_release(step, id, releasable)
            }

            GuestMessage::ReclaimAck { id, will_release } => {
                phase.on_reclaim_ack(step, id, will_release)
            }

            GuestMessage::ReclaimNak { id, reason } => phase.on_reclaim_nak(step, id, &reason),

            GuestMessage::Status(status) => phase.on_status(step, status),

            GuestMessage::Bye {} => phase.on_bye(step),
        });

        self.settle_phase(guest, next);
    }
}
