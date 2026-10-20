use pazari_proto::HostMessage;
use pazari_types::{Deadline, Timestamp};
use tracing::{info, warn};

use crate::broker::{Broker, effects::Effects, phase::Reclaim, registry::GuestId};

impl Broker {
    pub(in crate::broker) fn on_reclaim(
        &mut self,
        now: Timestamp,
        guest: GuestId,
        desired: u64,
        fx: &mut Effects,
    ) {
        let Some(session) = self.world.guests().session(guest) else {
            warn!(
                guest = self.world.name(guest),
                "cannot reclaim: guest is not connected"
            );

            return;
        };

        let peer = session.peer.clone();

        let Some(negotiated) = session.negotiated() else {
            warn!(
                guest = self.world.name(guest),
                "cannot reclaim: guest has not sent hello"
            );

            return;
        };

        if let Some(reclaim) = negotiated.reclaim {
            warn!(
                guest = self.world.name(guest),
                outstanding = %reclaim.id,
                refused = reclaim.refused,
                "guest is still in reclaim backoff",
            );

            return;
        }

        let answer_within = self.world.policy().reclaim_deadline.get();

        let Some(session) = self.world.guests_mut().negotiated_mut(guest) else {
            return;
        };

        let id = session.take_reclaim_id();

        session.reclaim = Some(Reclaim {
            id,
            desired,
            deadline: Deadline::after(now, answer_within),
            committed: None,
            refused: false,
        });

        info!(
            guest = self.world.name(guest),
            %id, desired, "requesting memory back from a guest"
        );

        fx.send(
            &peer,
            HostMessage::ReclaimRequest {
                id,
                desired,
                deadline: answer_within,
            },
        );
    }
}
