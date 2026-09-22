use pazari_proto::HostMessage;

use pazari_types::Timestamp;
use tracing::{info, warn};

use crate::broker::{Broker, phase::Reclaim, registry::GuestId};

impl Broker {
    pub(in crate::broker) fn on_reclaim(&mut self, now: Timestamp, guest: GuestId, desired: u64) {
        let Some(session) = self.ctx.guests.session(guest) else {
            warn!(
                guest = self.ctx.name(guest),
                "nothing to reclaim from: the guest is not connected"
            );

            return;
        };

        let peer = session.peer.clone();

        let Some(negotiated) = session.negotiated() else {
            warn!(
                guest = self.ctx.name(guest),
                "nothing to reclaim from: the guest has not said hello"
            );

            return;
        };

        if let Some(reclaim) = negotiated.reclaim {
            warn!(
                guest = self.ctx.name(guest),
                outstanding = reclaim.id,
                refused = reclaim.refused,
                "the guest is not to be asked for memory again yet",
            );

            return;
        }

        let deadline_ms = self.ctx.policy.reclaim_deadline.get();

        let Some(session) = self.ctx.guests.negotiated_mut(guest) else {
            return;
        };

        let id = session.take_reclaim_id();

        session.reclaim = Some(Reclaim {
            id,
            desired,
            deadline: now.saturating_add(deadline_ms),
            committed: None,
            refused: false,
        });

        info!(
            guest = self.ctx.name(guest),
            id, desired, "asking a guest for memory back"
        );

        self.ctx.send(
            &peer,
            HostMessage::ReclaimRequest {
                id,
                desired,
                deadline_ms,
            },
        );
    }
}
