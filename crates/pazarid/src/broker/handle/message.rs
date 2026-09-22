use pazari_proto::GuestMessage;
use pazari_transport::PeerId;
use pazari_types::Timestamp;
use tracing::warn;

use crate::broker::{
    Broker,
    phase::{Said, State},
};

impl Broker {
    pub(in crate::broker) fn on_message(
        &mut self,
        now: Timestamp,
        peer: &PeerId,
        message: GuestMessage,
    ) {
        let Some(guest) = self.ctx.guests.lookup(peer) else {
            warn!(%peer, "a message from a peer no guest answers to");
            self.ctx.close_unknown(peer);

            return;
        };

        let Some(phase) = self.ctx.guests.session(guest).map(|session| session.phase) else {
            warn!(guest = self.ctx.name(guest), %peer, "a message on a connection the broker never accepted");
            self.ctx.close_unknown(peer);

            return;
        };

        let said = Said { now, guest, peer };
        let ctx = &mut self.ctx;

        let next = match message {
            GuestMessage::Hello { .. } => phase.on_hello(ctx, said, message),

            GuestMessage::GrantRequest {
                id,
                target,
                reason,
                urgency,
                ..
            } => phase.on_grant_request(ctx, said, id, target, reason, urgency),

            GuestMessage::OfferRelease { id, releasable, .. } => {
                phase.on_offer_release(ctx, said, id, releasable)
            }

            GuestMessage::ReclaimAck { id, will_release } => {
                phase.on_reclaim_ack(ctx, said, id, will_release)
            }

            GuestMessage::ReclaimNak { id, reason } => phase.on_reclaim_nak(ctx, said, id, &reason),

            GuestMessage::Status(status) => phase.on_status(ctx, said, status),

            GuestMessage::Bye {} => phase.on_bye(ctx, said),
        };

        if let Some(session) = self.ctx.guests.session_mut(guest) {
            session.phase = next;
        }
    }
}
