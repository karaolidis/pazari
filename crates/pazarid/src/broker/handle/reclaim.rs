use pazari_proto::HostMessage;
use pazari_types::{Deadline, Timestamp};
use tracing::{info, warn};

use crate::broker::{
    Broker,
    effects::Effects,
    guest::Entry,
    phase::{Reclaim, Reply},
    registry::GuestId,
    session::Session,
};

impl Broker {
    pub(in crate::broker) fn on_reclaim(
        &mut self,
        now: Timestamp,
        guest: GuestId,
        desired: u64,
        fx: &mut Effects,
    ) {
        let answer_within = self.world.policy().reclaim_deadline.get();

        let Some(Entry { spec, state }) = self.world.guests_mut().get_mut(guest) else {
            return;
        };

        let Some(Session { peer, phase }) = state.session.as_mut() else {
            warn!(guest = spec.name, "cannot reclaim: guest is not connected");

            return;
        };

        let Some(session) = phase.negotiated_mut() else {
            warn!(
                guest = spec.name,
                "cannot reclaim: guest has not sent hello"
            );

            return;
        };

        if let Some(reclaim) = session.reclaim {
            let waiting = match reclaim.reply {
                Reply::Pending => "guest has yet to answer a reclaim",
                Reply::Committed(_) => "guest has yet to release what it promised",
                Reply::Refused => "guest is still in reclaim backoff",
            };

            warn!(guest = spec.name, outstanding = %reclaim.id, "{waiting}");

            return;
        }

        let id = session.take_reclaim_id();

        session.reclaim = Some(Reclaim {
            id,
            desired,
            deadline: Deadline::after(now, answer_within),
            reply: Reply::Pending,
        });

        info!(
            guest = spec.name,
            %id, desired, "requesting memory back from a guest"
        );

        fx.send(
            peer,
            HostMessage::ReclaimRequest {
                id,
                desired,
                deadline: answer_within,
            },
        );
    }
}
