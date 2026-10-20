use pazari_proto::HostMessage;
use pazari_types::{Deadline, Timestamp};
use tracing::info;

use crate::broker::{
    Broker,
    effects::Effects,
    guest::Entry,
    phase::{Reclaim, Reply},
    registry::GuestId,
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

        let Entry { spec, state } = self.world.guests_mut().get_mut(guest);

        let Some((peer, session)) = state.askable() else {
            return;
        };

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
